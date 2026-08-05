//! Restarting the daemon, and being honest about what came back.

mod common;

use common::*;
use turn_core::attention::{AttentionEntry, EntryState};
use turn_core::ids::{AttentionId, CheckoutId};
use turn_core::model::{LeaseState, PaneKind, RestoreState};
use turn_core::state::{AwaitingReason, Lifecycle};
use turn_core::Confidence;
use turn_proto::{ErrorCode, NewPane, ProtoErrorContext, Request, Response, ServerEvent};

/// Creates a workspace and a session from the Coding template.
async fn seed(daemon: &TestDaemon, ui: &mut Client) -> turn_proto::SessionDetails {
    let workspace = workspace_of(
        ui.ask(Request::CreateWorkspace {
            name: "restarted".to_string(),
            root: daemon.data_dir().display().to_string(),
        })
        .await,
    );
    let templates = match ui.ask(Request::ListTemplates).await {
        Response::Templates { templates } => templates,
        other => panic!("expected templates, got {other:?}"),
    };
    let coding = templates
        .iter()
        .find(|template| template.name == "Coding")
        .expect("the Coding template");
    let session = session_of(
        ui.ask(Request::CreateSessionFromTemplate {
            workspace_id: workspace.id,
            template_id: coding.id.clone(),
            name: Some("Work in progress".to_string()),
            cwd: None,
            branch: Some("feature/restore".to_string()),
            task: None,
        })
        .await,
    );
    details_of(
        ui.ask(Request::GetSession {
            session_id: session.id,
        })
        .await,
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restart_brings_back_the_desk_and_reports_what_it_could_not_recover() {
    let daemon = TestDaemon::start().await;
    let mut ui = daemon.connect().await;
    let before = seed(&daemon, &mut ui).await;

    let session_id = before.summary.id.clone();
    let workspace_id = before.summary.workspace_id.clone();
    let pane_ids: Vec<_> = before.layout.panes().iter().map(|p| p.id.clone()).collect();
    let pids: Vec<u32> = before.tree.iter().filter_map(|node| node.pid).collect();
    assert!(
        pids.len() >= 2,
        "Claude and the shell must run; Fang also runs when installed"
    );
    assert!(pids.iter().all(|pid| pid_is_alive(*pid)));
    let before_lease = match ui
        .ask(Request::GetWorkspaceWriteLease {
            workspace_id: workspace_id.clone(),
        })
        .await
    {
        Response::WorkspaceWriteLease {
            lease: Some(lease), ..
        } => lease,
        other => panic!("expected an active write lease, got {other:?}"),
    };
    drop(ui);

    let daemon = daemon.restart().await;
    let mut ui = daemon.connect().await;

    // The desk is back.
    let workspaces = match ui
        .ask(Request::ListWorkspaces {
            include_archived: false,
        })
        .await
    {
        Response::Workspaces { workspaces } => workspaces,
        other => panic!("expected workspaces, got {other:?}"),
    };
    assert_eq!(workspaces.len(), 1);
    assert_eq!(workspaces[0].id, workspace_id);
    assert_eq!(workspaces[0].name, "restarted");

    let recovery_lease = match ui
        .ask(Request::GetWorkspaceWriteLease {
            workspace_id: workspace_id.clone(),
        })
        .await
    {
        Response::WorkspaceWriteLease {
            lease: Some(lease), ..
        } => lease,
        other => panic!("expected the fenced write lease, got {other:?}"),
    };
    assert_eq!(recovery_lease.id, before_lease.id);
    assert_eq!(recovery_lease.generation, before_lease.generation);
    assert_eq!(recovery_lease.state, LeaseState::RecoveryRequired);
    assert_eq!(
        recovery_lease.heartbeat_ms, before_lease.heartbeat_ms,
        "starting a new daemon must not forge the previous owner's heartbeat"
    );

    let sessions = match ui
        .ask(Request::ListSessions {
            workspace_id: Some(workspace_id.clone()),
            include_archived: false,
        })
        .await
    {
        Response::Sessions { sessions } => sessions,
        other => panic!("expected sessions, got {other:?}"),
    };
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id, session_id);
    assert_eq!(sessions[0].name, "Work in progress");

    let after = details_of(
        ui.ask(Request::GetSession {
            session_id: session_id.clone(),
        })
        .await,
    );
    let after_panes: Vec<_> = after.layout.panes().iter().map(|p| p.id.clone()).collect();
    assert_eq!(
        after_panes, pane_ids,
        "the same panes, with the same identities"
    );
    assert_eq!(after.summary.git_branch.as_deref(), Some("feature/restore"));

    // The processes did not survive the daemon that owned their ptys, and every one of
    // them is reported as lost rather than quietly started again.
    assert!(
        pids.iter().all(|pid| !pid_is_alive(*pid)),
        "the old processes are gone"
    );
    for node in &after.tree {
        assert_eq!(
            node.lifecycle,
            Lifecycle::Lost,
            "{} was running before the restart; it must be reported, not guessed at",
            node.title
        );
    }
    assert_eq!(
        after.summary.running_count, 0,
        "nothing was relaunched on the daemon's own initiative"
    );
    assert_eq!(after.summary.restore_state, RestoreState::LayoutOnly);
    assert!(
        after.summary.restore_needs_explanation,
        "the user must be told, not left to notice a dead pane"
    );

    // And the restore report reached the client that connected after it happened.
    let (state, panes) = ui
        .wait_for("the restore report", |event| match event {
            ServerEvent::RestoreResult {
                session_id: id,
                state,
                panes,
                ..
            } if id == &session_id => Some((*state, panes.clone())),
            _ => None,
        })
        .await;
    assert_eq!(state, RestoreState::LayoutOnly);
    assert_eq!(
        panes.len(),
        pids.len(),
        "every pane that had a process needs a restore report"
    );
    for pane in &panes {
        assert_eq!(pane.lifecycle, Lifecycle::Lost);
        assert!(
            pane.can_relaunch,
            "a pane Turn could start again says so — as an offer"
        );
    }

    // A restart never auto-adopts the old write authority. Even an explicit process
    // relaunch fails closed until the user reconciles the fenced lease.
    let shell_pane = after
        .layout
        .panes()
        .into_iter()
        .find(|pane| pane.kind == PaneKind::Shell)
        .cloned()
        .expect("the shell pane");
    let dead_node = shell_pane.node_id.clone().expect("its lost node record");
    let refused = ui
        .try_ask(Request::RelaunchNode {
            session_id: session_id.clone(),
            node_id: dead_node.clone(),
            resume: false,
        })
        .await
        .expect_err("recovery-required is not launch authority");
    assert_eq!(refused.code, ErrorCode::Refused);
    assert!(refused
        .detail
        .as_deref()
        .is_some_and(|detail| detail.contains("reconciliation")));

    // Re-acquiring without releasing is also not adoption. It remains a typed
    // conflict naming the durable owner and the recovery-required lease.
    let conflict = ui
        .try_ask(Request::AcquireWorkspaceWriteLease {
            workspace_id: workspace_id.clone(),
            session_id: session_id.clone(),
            checkout_id: CheckoutId::primary_for(&workspace_id),
        })
        .await
        .expect_err("the inherited claim must be reconciled explicitly");
    assert_eq!(conflict.code, ErrorCode::Conflict);
    assert!(matches!(
        conflict.context.as_deref(),
        Some(ProtoErrorContext::WorkspaceWriteLeaseConflict {
            lease,
            owner,
            ..
        }) if lease.id == before_lease.id
            && lease.state == LeaseState::RecoveryRequired
            && owner.session_id == session_id
    ));

    ui.ask(Request::ReleaseWorkspaceWriteLease {
        workspace_id: workspace_id.clone(),
        lease_id: recovery_lease.id,
        expected_generation: recovery_lease.generation,
    })
    .await;
    let acquired = match ui
        .ask(Request::AcquireWorkspaceWriteLease {
            workspace_id: workspace_id.clone(),
            session_id: session_id.clone(),
            checkout_id: CheckoutId::primary_for(&workspace_id),
        })
        .await
    {
        Response::WorkspaceWriteLease {
            lease: Some(lease), ..
        } => lease,
        other => panic!("expected an explicitly reacquired lease, got {other:?}"),
    };
    assert_eq!(acquired.state, LeaseState::Active);
    assert!(acquired.generation > before_lease.generation);

    // The offer is answered by the user only after explicit reconciliation.
    let relaunched = node_of(
        ui.ask(Request::RelaunchNode {
            session_id: session_id.clone(),
            node_id: dead_node.clone(),
            resume: false,
        })
        .await,
    );
    assert!(relaunched.lifecycle.is_running());
    assert!(pid_is_alive(relaunched.pid.expect("a fresh pid")));
    assert_eq!(
        relaunched
            .pane_bindings
            .iter()
            .map(|binding| &binding.pane_id)
            .collect::<Vec<_>>(),
        vec![&shell_pane.id]
    );

    daemon.shutdown().await;
}

/// A process that outlived the daemon is alive but out of reach, and saying so is the
/// difference between "your work is still running" and "your work is gone".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_process_that_outlived_the_daemon_is_reported_as_orphaned_not_lost() {
    let daemon = TestDaemon::start().await;
    let mut ui = daemon.connect().await;
    let before = seed(&daemon, &mut ui).await;
    let session_id = before.summary.id.clone();
    drop(ui);

    let dir = daemon.stop().await;
    let data_dir = dir.path().to_path_buf();

    // A process Turn did not start and cannot reach: the shape of an agent that survived
    // a daemon crash. Recorded in the store the way the daemon would have recorded it.
    let mut survivor = std::process::Command::new("sleep")
        .arg("300")
        .spawn()
        .expect("sleep must start");
    let survivor_pid = survivor.id();
    let mut expected_entries = Vec::new();
    let discarded_id;

    {
        let store = turn_store::Store::open_in(&data_dir).expect("the store must open");
        let mut node = turn_core::model::ProcessNode::process(
            session_id.clone(),
            turn_core::model::NodeKind::Server,
            "sleep 300",
            data_dir.display().to_string(),
            turn_core::now_ms(),
        );
        node.pid = Some(survivor_pid);
        node.lifecycle = Lifecycle::Alive;
        node.title = "survivor".to_string();
        store.nodes().upsert(&node).expect("the node must save");

        // The same pid, but a command that does not match: this is the pid-reuse case.
        // The kernel would say "alive" and it would be a stranger's process.
        let mut impostor = turn_core::model::ProcessNode::process(
            session_id.clone(),
            turn_core::model::NodeKind::Server,
            "definitely-not-sleep --serve",
            data_dir.display().to_string(),
            turn_core::now_ms(),
        );
        impostor.pid = Some(survivor_pid);
        impostor.lifecycle = Lifecycle::Alive;
        impostor.title = "impostor".to_string();
        store.nodes().upsert(&impostor).expect("the node must save");

        // Every durable field and state must survive for a process that survives.
        // The final demand belongs to the pid-reuse impostor and must be the only
        // one removed by runtime reconciliation.
        for (index, (reason, state, confidence, boost)) in [
            (
                AwaitingReason::Question,
                EntryState::Pending,
                Confidence::Explicit,
                7,
            ),
            (
                AwaitingReason::Permission,
                EntryState::Snoozed {
                    until_ms: turn_core::now_ms() + 600_000,
                },
                Confidence::Integrated,
                19,
            ),
            (
                AwaitingReason::Credentials,
                EntryState::Acknowledged,
                Confidence::InferredHigh,
                -4,
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let created_ms = turn_core::now_ms() - 120_000 + index as i64 * 1_000;
            let entry = AttentionEntry {
                id: AttentionId::new(),
                session_id: session_id.clone(),
                node_id: Some(node.id.clone()),
                parent_node_id: None,
                subject_external_id: None,
                reason,
                summary: Some(format!("durable demand {index}")),
                confidence,
                created_ms,
                updated_ms: created_ms + 333,
                state,
                priority_boost: boost,
            };
            store
                .attention()
                .upsert(&entry)
                .expect("the entry must save");
            expected_entries.push(entry);
        }
        let scoped = AttentionEntry {
            id: AttentionId::new(),
            session_id: session_id.clone(),
            node_id: None,
            parent_node_id: Some(node.id.clone()),
            subject_external_id: Some("worker-not-declared-yet".into()),
            reason: AwaitingReason::Permission,
            summary: Some("out-of-order worker demand".into()),
            confidence: Confidence::Unknown,
            created_ms: turn_core::now_ms() - 90_000,
            updated_ms: turn_core::now_ms() - 89_000,
            state: EntryState::Snoozed {
                until_ms: turn_core::now_ms() + 300_000,
            },
            priority_boost: 11,
        };
        store
            .attention()
            .upsert(&scoped)
            .expect("the scoped entry must save");
        expected_entries.push(scoped);
        let discarded = AttentionEntry {
            id: AttentionId::new(),
            session_id: session_id.clone(),
            node_id: None,
            parent_node_id: Some(impostor.id.clone()),
            subject_external_id: Some("worker-with-dead-parent".into()),
            reason: AwaitingReason::Input,
            summary: Some("nobody remains to answer this".into()),
            confidence: Confidence::Explicit,
            created_ms: turn_core::now_ms() - 240_000,
            updated_ms: turn_core::now_ms() - 239_000,
            state: EntryState::Pending,
            priority_boost: 100,
        };
        discarded_id = discarded.id.clone();
        store
            .attention()
            .upsert(&discarded)
            .expect("the discarded entry must save");
    }

    let daemon = TestDaemon::adopt(dir).await;
    let mut ui = daemon.connect().await;
    let after = details_of(
        ui.ask(Request::GetSession {
            session_id: session_id.clone(),
        })
        .await,
    );

    let survivor_view = after
        .tree
        .iter()
        .find(|node| node.title == "survivor")
        .expect("the surviving node");
    assert_eq!(
        survivor_view.lifecycle,
        Lifecycle::Orphaned,
        "it is in the process table and running, but Turn does not hold it"
    );
    assert!(survivor_view.lifecycle.is_running());

    let impostor_view = after
        .tree
        .iter()
        .find(|node| node.title == "impostor")
        .expect("the impostor node");
    assert_eq!(
        impostor_view.lifecycle,
        Lifecycle::Lost,
        "the pid is alive but it is not the command we recorded; a pid is not an identity"
    );

    assert_eq!(
        after.summary.restore_state,
        RestoreState::PartiallyRestored,
        "some of the work is alive and some of it is not"
    );

    // Exact and unresolved scoped demands for the surviving process come back
    // byte-for-byte as domain values. The unresolved demand whose hook parent
    // turned out to be gone does not: there is nobody left to answer it.
    let entries = attention_list_of(ui.ask(Request::ListAttention { session_id: None }).await);
    assert_eq!(entries.len(), expected_entries.len(), "{entries:#?}");
    assert!(!entries.iter().any(|view| view.entry.id == discarded_id));
    for expected in &expected_entries {
        let restored = entries
            .iter()
            .find(|view| view.entry.id == expected.id)
            .unwrap_or_else(|| panic!("missing restored demand {}", expected.id));
        assert_eq!(
            &restored.entry, expected,
            "id, age, snooze/ack state and boost must be preserved"
        );
        if expected.node_id.is_some() {
            assert_eq!(
                restored.entry.node_id.as_ref(),
                Some(&survivor_view.node_id)
            );
        } else {
            assert_eq!(restored.entry.node_id, None);
            assert_eq!(
                restored.entry.parent_node_id.as_ref(),
                Some(&survivor_view.node_id)
            );
            assert_eq!(
                restored.entry.subject_external_id.as_deref(),
                Some("worker-not-declared-yet")
            );
        }
    }

    // Nothing Turn does not hold can be written to, however alive it looks.
    let error = ui
        .try_ask(Request::WritePty {
            session_id: session_id.clone(),
            node_id: survivor_view.node_id.clone(),
            data: turn_proto::TerminalBytes::new(b"hello?\n".to_vec()),
        })
        .await
        .expect_err("an orphan is out of reach");
    assert_eq!(error.code, turn_proto::ErrorCode::ProcessNotRunning);

    daemon.shutdown().await;
    let _ = survivor.kill();
    let _ = survivor.wait();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn data_directory_and_socket_ownership_are_independent_and_recoverable() {
    let daemon = TestDaemon::start().await;
    let socket = daemon.socket().to_path_buf();

    // Seed an active lease without starting a process. If the contender reaches
    // Core::restore it will fence this lease as recovery-required, which is the
    // split-brain regression this test is meant to catch.
    let mut ui = daemon.connect().await;
    let workspace = workspace_of(
        ui.ask(Request::CreateWorkspace {
            name: "single writer".to_string(),
            root: daemon.data_dir().display().to_string(),
        })
        .await,
    );
    session_of(
        ui.ask(Request::CreateSession {
            workspace_id: workspace.id.clone(),
            name: "lease owner".to_string(),
            cwd: None,
            panes: Some(vec![NewPane::new(PaneKind::Terminal)]),
            note: None,
            tags: Vec::new(),
        })
        .await,
    );
    let lease_before = match ui
        .ask(Request::GetWorkspaceWriteLease {
            workspace_id: workspace.id.clone(),
        })
        .await
    {
        Response::WorkspaceWriteLease {
            lease: Some(lease), ..
        } => lease,
        other => panic!("expected the active lease, got {other:?}"),
    };
    assert_eq!(lease_before.state, LeaseState::Active);
    drop(ui);

    // A different socket does not create another store/PTY ownership domain.
    let alternate_socket = daemon.data_dir().join("alternate.sock");
    let mut config = turnd::Config::in_dir(daemon.data_dir());
    config.socket_path = alternate_socket.clone();
    let error = turnd::start(config)
        .await
        .expect_err("a second daemon must refuse to start");
    assert!(
        matches!(error, turnd::DaemonError::DataDirInUse { .. }),
        "{error}"
    );
    assert!(error.is_contention());
    assert!(error.to_string().contains("already running"), "{error}");
    assert!(
        !alternate_socket.exists(),
        "contention is rejected before the second transport is bound"
    );

    // A symlink spelling of the same directory reaches the same inode lock rather
    // than creating a textual second ownership claim.
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let alias = daemon.data_dir().join("state-alias");
        symlink(daemon.data_dir(), &alias).expect("the data directory alias");
        let mut aliased = turnd::Config::in_dir(&alias);
        aliased.socket_path = daemon.data_dir().join("aliased.sock");
        let error = turnd::start(aliased)
            .await
            .expect_err("a filesystem alias must not evade store ownership");
        assert!(matches!(error, turnd::DaemonError::DataDirInUse { .. }));
    }

    // The first one is still serving and, crucially, the rejected Core never fenced
    // its lease. A heartbeat may advance naturally; identity and active state may not.
    let mut ui = daemon.connect().await;
    let lease_after = match ui
        .ask(Request::GetWorkspaceWriteLease {
            workspace_id: workspace.id.clone(),
        })
        .await
    {
        Response::WorkspaceWriteLease {
            lease: Some(lease), ..
        } => lease,
        other => panic!("expected the original active lease, got {other:?}"),
    };
    assert_eq!(lease_after.id, lease_before.id);
    assert_eq!(lease_after.generation, lease_before.generation);
    assert_eq!(lease_after.session_id, lease_before.session_id);
    assert_eq!(lease_after.state, LeaseState::Active);
    let pid = ui.welcome.daemon_pid;
    assert_eq!(pid, std::process::id());
    drop(ui);

    // Socket ownership remains a separate guard: a daemon with a different store
    // cannot displace the first daemon's live transport. Its data-dir lock is dropped
    // on this failed start, so a subsequent normal start there succeeds.
    let other_dir = tempfile::tempdir().expect("another data directory");
    let mut socket_contender = turnd::Config::in_dir(other_dir.path());
    socket_contender.socket_path = socket.clone();
    let error = turnd::start(socket_contender)
        .await
        .expect_err("the live socket must not be displaced");
    assert!(
        matches!(error, turnd::DaemonError::AlreadyRunning { .. }),
        "{error}"
    );
    let independent = turnd::start(turnd::Config::in_dir(other_dir.path()))
        .await
        .expect("a failed socket bind must release its unrelated data-dir lock");
    independent.shutdown().await;

    let dir = daemon.stop().await;
    assert!(
        !socket.exists(),
        "a clean shutdown takes its socket with it, so the next start has nothing to diagnose"
    );

    // A socket file left behind by a daemon that died is not a reason to refuse.
    std::fs::write(&socket, b"leftover").expect("the stale file must be writable");
    let daemon = TestDaemon::adopt(dir).await;
    let mut ui = daemon.connect().await;
    ui.ask(Request::ListTemplates).await;
    daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropping_the_api_handle_does_not_unlock_a_detached_core() {
    let dir = tempfile::tempdir().expect("a temporary data directory");
    let first = turnd::start(turnd::Config::in_dir(dir.path()))
        .await
        .expect("the first daemon");
    drop(first);

    // DaemonHandle explicitly documents that Drop does not stop the daemon. The
    // detached Core still owns its PTYs and therefore must retain the shared lock.
    let mut contender = turnd::Config::in_dir(dir.path());
    contender.socket_path = dir.path().join("detached-contender.sock");
    let error = turnd::start(contender)
        .await
        .expect_err("a detached core must keep store ownership");
    assert!(
        matches!(error, turnd::DaemonError::DataDirInUse { .. }),
        "{error}"
    );

    // The test runtime owns and cancels the deliberately detached tasks at the end
    // of this test, which models the process boundary that finally releases the lock.
}

/// `--no-persist` has to mean it: a daemon told not to write must not leave a database
/// behind, or the flag is a promise the next start-up quietly breaks.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_daemon_told_not_to_persist_leaves_nothing_on_disk() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let config = turnd::Config::in_dir(dir.path())
        .with_registry(fake_registry())
        .without_persistence();
    let daemon = turnd::start(config).await.expect("the daemon must start");

    let mut ui = Client::connect(daemon.socket_path()).await;
    let workspace = workspace_of(
        ui.ask(Request::CreateWorkspace {
            name: "ephemeral".to_string(),
            root: dir.path().display().to_string(),
        })
        .await,
    );
    // It behaves identically while it runs.
    let session = session_of(
        ui.ask(Request::CreateSession {
            workspace_id: workspace.id.clone(),
            name: "gone in a moment".to_string(),
            cwd: None,
            panes: None,
            note: None,
            tags: Vec::new(),
        })
        .await,
    );
    assert_eq!(session.running_count, 1);
    drop(ui);
    daemon.shutdown().await;

    assert!(
        !dir.path().join("turn.db").exists(),
        "an in-memory store must not write a database"
    );
}

/// The restore explanation has to mean something. A session that had nothing running
/// lost nothing, and a flag that fires for every session on every start says as little as
/// no flag at all — including about the sessions that really did lose work.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_that_had_nothing_to_lose_does_not_claim_turn_lost_something() {
    let daemon = TestDaemon::start().await;
    let mut ui = daemon.connect().await;
    let working = seed(&daemon, &mut ui).await;

    // A copy, which is a session set up for another run of the same task: same shape,
    // no processes. Nothing about a restart can take anything away from it.
    let copy = session_of(
        ui.ask(Request::DuplicateSession {
            session_id: working.summary.id.clone(),
        })
        .await,
    );
    assert_eq!(copy.running_count, 0, "duplicating starts nothing");
    drop(ui);

    let daemon = daemon.restart().await;
    let mut ui = daemon.connect().await;

    let restored_copy = details_of(
        ui.ask(Request::GetSession {
            session_id: copy.id.clone(),
        })
        .await,
    );
    assert_eq!(
        restored_copy.summary.restore_state,
        RestoreState::Live,
        "nothing was running, so nothing was restored"
    );
    assert!(
        !restored_copy.summary.restore_needs_explanation,
        "there is nothing to explain, and saying otherwise is how a warning becomes noise"
    );

    // The session that really did lose its processes still says so, in the same restart.
    let restored_work = details_of(
        ui.ask(Request::GetSession {
            session_id: working.summary.id.clone(),
        })
        .await,
    );
    assert_eq!(
        restored_work.summary.restore_state,
        RestoreState::LayoutOnly
    );
    assert!(
        restored_work.summary.restore_needs_explanation,
        "the session that lost work must still be explained"
    );

    daemon.shutdown().await;
}

/// The fourth product rule, executable: **Turn never relaunches on restore.** It is
/// structurally true — `RelaunchNode` is the only request that starts anything — and this
/// is the test that keeps it true, including for the panes that say they are safe to
/// restart. `RestoreBehaviour::Relaunch` marks a pane Turn *may offer* to start again; a
/// daemon that read it as permission would be running the user's commands for them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restart_relaunches_nothing_even_for_a_pane_that_says_it_is_safe_to() {
    let daemon = TestDaemon::start().await;
    let mut ui = daemon.connect().await;
    let before = seed(&daemon, &mut ui).await;
    let session_id = before.summary.id.clone();

    // The Coding template's shell pane asks to be relaunched on restore; the agent pane
    // does not. Both must be left alone.
    let eager: Vec<_> = before
        .layout
        .panes()
        .into_iter()
        .filter(|pane| pane.restore == turn_core::model::RestoreBehaviour::Relaunch)
        .map(|pane| pane.id.clone())
        .collect();
    assert!(
        !eager.is_empty(),
        "this test is worthless without a pane that says it is safe to relaunch"
    );

    let before_nodes: Vec<(turn_core::ids::PaneId, turn_core::ids::NodeId)> = before
        .layout
        .panes()
        .into_iter()
        .filter_map(|pane| pane.node_id.clone().map(|node| (pane.id.clone(), node)))
        .collect();
    let pids: Vec<u32> = before.tree.iter().filter_map(|node| node.pid).collect();
    // A launch leaves a scratch directory per node, which is the filesystem's own record
    // of an adapter having prepared one. Nothing new may appear here.
    let scratch_root = turnd::paths::session_scratch(daemon.data_dir(), &session_id);
    let launched_before = scratch_dirs(&scratch_root);
    assert!(!launched_before.is_empty(), "the first launch wrote one");
    drop(ui);

    let daemon = daemon.restart().await;
    let mut ui = daemon.connect().await;
    let after = details_of(
        ui.ask(Request::GetSession {
            session_id: session_id.clone(),
        })
        .await,
    );

    // Every pane still points at the process it had, which is now reported as lost. A
    // relaunch would have retired those nodes and minted new ones.
    let after_nodes: Vec<(turn_core::ids::PaneId, turn_core::ids::NodeId)> = after
        .layout
        .panes()
        .into_iter()
        .filter_map(|pane| pane.node_id.clone().map(|node| (pane.id.clone(), node)))
        .collect();
    assert_eq!(
        after_nodes, before_nodes,
        "a pane's process was replaced without the user asking"
    );
    for node in &after.tree {
        assert_eq!(node.lifecycle, Lifecycle::Lost, "{}", node.title);
        assert!(!node.lifecycle.is_running());
    }
    assert_eq!(after.summary.running_count, 0);
    assert!(
        pids.iter().all(|pid| !pid_is_alive(*pid)),
        "the old processes are gone, and nothing took their place"
    );
    assert_eq!(
        scratch_dirs(&scratch_root),
        launched_before,
        "an adapter prepared a launch during restore"
    );

    // What the user gets instead is an offer, for the eager pane as much as any other.
    let (_, panes) = ui
        .wait_for("the restore report", |event| match event {
            ServerEvent::RestoreResult {
                session_id: id,
                state,
                panes,
                ..
            } if id == &session_id => Some((*state, panes.clone())),
            _ => None,
        })
        .await;
    for pane_id in &eager {
        let outcome = panes
            .iter()
            .find(|outcome| &outcome.pane_id == pane_id)
            .expect("the eager pane is in the report");
        assert!(!outcome.lifecycle.is_running(), "{:?}", outcome.lifecycle);
        assert!(
            outcome.can_relaunch,
            "an offer is the most a restore may do about it"
        );
    }

    daemon.shutdown().await;
}

/// The node directories a session's launches have written.
fn scratch_dirs(root: &std::path::Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    names
}
