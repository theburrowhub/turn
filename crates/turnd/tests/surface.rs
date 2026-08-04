//! The rest of the surface.
//!
//! Between this file and the others, every one of the forty operations the protocol
//! defines is served and asserted on at least once. An operation the daemon answered with
//! `internal` would be a client feature that silently does not work, and the protocol
//! promises a typed result for every one of them.

mod common;

use common::*;
use turn_core::model::PaneKind;
use turn_core::state::Turn;
use turn_proto::{
    CloseDisposition, ErrorCode, FocusTarget, NewPane, PtySize, Request, Response, ServerEvent,
};

async fn workspace(
    daemon: &TestDaemon,
    ui: &mut Client,
    name: &str,
) -> turn_proto::WorkspaceSummary {
    workspace_of(
        ui.ask(Request::CreateWorkspace {
            name: name.to_string(),
            root: daemon.data_dir().display().to_string(),
        })
        .await,
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_workspace_can_be_renamed_duplicated_archived_and_closed() {
    let daemon = TestDaemon::start().await;
    let mut ui = daemon.connect().await;
    let original = workspace(&daemon, &mut ui, "original").await;

    let renamed = workspace_of(
        ui.ask(Request::RenameWorkspace {
            workspace_id: original.id.clone(),
            name: "  renamed  ".to_string(),
        })
        .await,
    );
    assert_eq!(renamed.name, "renamed", "a name is trimmed, not stored raw");

    let error = ui
        .try_ask(Request::RenameWorkspace {
            workspace_id: original.id.clone(),
            name: "   ".to_string(),
        })
        .await
        .expect_err("a blank name would leave an unidentifiable row");
    assert_eq!(error.code, ErrorCode::InvalidArgument);

    let error = ui
        .try_ask(Request::CreateWorkspace {
            name: "relative".to_string(),
            root: "some/where".to_string(),
        })
        .await
        .expect_err("a relative root would resolve against the daemon's own directory");
    assert_eq!(error.code, ErrorCode::InvalidArgument);

    // A session in the original, so duplication can be shown not to copy it.
    ui.ask(Request::CreateSession {
        workspace_id: original.id.clone(),
        name: "in the original".to_string(),
        cwd: None,
        panes: None,
        note: None,
        tags: Vec::new(),
    })
    .await;

    let copy = workspace_of(
        ui.ask(Request::DuplicateWorkspace {
            workspace_id: original.id.clone(),
            name: None,
        })
        .await,
    );
    assert_eq!(copy.name, "renamed (copy)");
    assert_ne!(copy.id, original.id);
    assert_eq!(copy.root, original.root, "the settings come with it");
    assert_eq!(copy.session_count, 0, "the sessions do not");

    // Archived workspaces leave the switcher but stay on disk.
    ui.ask(Request::ArchiveWorkspace {
        workspace_id: copy.id.clone(),
        archived: true,
    })
    .await;
    let visible = match ui
        .ask(Request::ListWorkspaces {
            include_archived: false,
        })
        .await
    {
        Response::Workspaces { workspaces } => workspaces,
        other => panic!("expected workspaces, got {other:?}"),
    };
    assert!(visible.iter().all(|workspace| workspace.id != copy.id));
    let all = match ui
        .ask(Request::ListWorkspaces {
            include_archived: true,
        })
        .await
    {
        Response::Workspaces { workspaces } => workspaces,
        other => panic!("expected workspaces, got {other:?}"),
    };
    assert!(all.iter().any(|workspace| workspace.id == copy.id));
    // Undo is the same code path as do.
    ui.ask(Request::ArchiveWorkspace {
        workspace_id: copy.id.clone(),
        archived: false,
    })
    .await;

    // Closing a workspace closes its sessions and keeps the workspace: deleting a project
    // because its last window closed is not something the protocol can express.
    ui.ask(Request::CloseWorkspace {
        workspace_id: original.id.clone(),
        disposition: CloseDisposition::Terminate,
    })
    .await;
    let still_there = match ui
        .ask(Request::ListWorkspaces {
            include_archived: false,
        })
        .await
    {
        Response::Workspaces { workspaces } => workspaces,
        other => panic!("expected workspaces, got {other:?}"),
    };
    assert!(still_there
        .iter()
        .any(|workspace| workspace.id == original.id));
    let sessions = match ui
        .ask(Request::ListSessions {
            workspace_id: Some(original.id.clone()),
            include_archived: false,
        })
        .await
    {
        Response::Sessions { sessions } => sessions,
        other => panic!("expected sessions, got {other:?}"),
    };
    assert!(sessions
        .iter()
        .all(|session| session.status == turn_core::model::SessionStatus::Paused));

    daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_can_be_renamed_duplicated_and_filed_away() {
    let daemon = TestDaemon::start().await;
    let mut ui = daemon.connect().await;
    let workspace = workspace(&daemon, &mut ui, "sessions").await;

    let session = session_of(
        ui.ask(Request::CreateSession {
            workspace_id: workspace.id.clone(),
            name: "first go".to_string(),
            cwd: None,
            panes: Some(vec![
                NewPane::new(PaneKind::Agent).with_command("cat"),
                NewPane::new(PaneKind::Shell),
            ]),
            note: None,
            tags: vec!["experiment".to_string()],
        })
        .await,
    );
    assert_eq!(session.pane_count, 2);
    assert_eq!(session.running_count, 2);

    let renamed = session_of(
        ui.ask(Request::RenameSession {
            session_id: session.id.clone(),
            name: "second thoughts".to_string(),
        })
        .await,
    );
    assert_eq!(renamed.name, "second thoughts");

    // A duplicate is a session set up for another run of the same task — not another run.
    let copy = session_of(
        ui.ask(Request::DuplicateSession {
            session_id: session.id.clone(),
        })
        .await,
    );
    assert_eq!(copy.name, "second thoughts (copy)");
    assert_eq!(copy.pane_count, 2);
    assert_eq!(
        copy.running_count, 0,
        "duplicating must not start anything: launching it is the user's next decision"
    );
    assert_eq!(copy.parent_session.as_ref(), Some(&session.id));
    let copied = details_of(
        ui.ask(Request::GetSession {
            session_id: copy.id.clone(),
        })
        .await,
    );
    assert!(
        copied
            .layout
            .panes()
            .iter()
            .all(|pane| pane.node_id.is_none()),
        "the copy's panes must not point at the original's processes"
    );
    assert_eq!(copied.summary.tags, vec!["experiment".to_string()]);

    // Archiving takes a session out of the list, and says so.
    let removed = {
        ui.ask(Request::ArchiveSession {
            session_id: copy.id.clone(),
            archived: true,
        })
        .await;
        ui.wait_for("the session to leave the list", |event| match event {
            ServerEvent::SessionRemoved { session_id, .. } if session_id == &copy.id => {
                Some(session_id.clone())
            }
            _ => None,
        })
        .await
    };
    assert_eq!(removed, copy.id);
    let listed = match ui
        .ask(Request::ListSessions {
            workspace_id: None,
            include_archived: false,
        })
        .await
    {
        Response::Sessions { sessions } => sessions,
        other => panic!("expected sessions, got {other:?}"),
    };
    assert!(listed.iter().all(|session| session.id != copy.id));
    let with_archived = match ui
        .ask(Request::ListSessions {
            workspace_id: None,
            include_archived: true,
        })
        .await
    {
        Response::Sessions { sessions } => sessions,
        other => panic!("expected sessions, got {other:?}"),
    };
    assert!(with_archived.iter().any(|session| session.id == copy.id));

    ui.ask(Request::ArchiveSession {
        session_id: copy.id.clone(),
        archived: false,
    })
    .await;

    daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn panes_can_be_swapped_focused_by_name_and_detached_without_stopping_anything() {
    let daemon = TestDaemon::start().await;
    let mut ui = daemon.connect().await;
    let workspace = workspace(&daemon, &mut ui, "panes").await;
    let session = session_of(
        ui.ask(Request::CreateSession {
            workspace_id: workspace.id.clone(),
            name: "two panes".to_string(),
            cwd: None,
            panes: Some(vec![
                NewPane::new(PaneKind::Shell),
                NewPane::new(PaneKind::Terminal).with_command("cat"),
            ]),
            note: None,
            tags: Vec::new(),
        })
        .await,
    );
    let details = details_of(
        ui.ask(Request::GetSession {
            session_id: session.id.clone(),
        })
        .await,
    );
    let panes: Vec<_> = details
        .layout
        .panes()
        .iter()
        .map(|p| p.id.clone())
        .collect();
    let kinds: Vec<_> = details.layout.panes().iter().map(|p| p.kind).collect();

    let swapped = layout_of(
        ui.ask(Request::SwapPanes {
            session_id: session.id.clone(),
            a: panes[0].clone(),
            b: panes[1].clone(),
        })
        .await,
    );
    let after: Vec<_> = swapped.panes().iter().map(|p| p.kind).collect();
    assert_eq!(
        after,
        vec![kinds[1], kinds[0]],
        "the panes exchanged places and the geometry did not move"
    );

    let focused = layout_of(
        ui.ask(Request::FocusPane {
            session_id: session.id.clone(),
            target: FocusTarget::Pane {
                pane_id: panes[1].clone(),
            },
        })
        .await,
    );
    assert_eq!(focused.active.as_ref(), Some(&panes[1]));
    let previous = layout_of(
        ui.ask(Request::FocusPane {
            session_id: session.id.clone(),
            target: FocusTarget::Previous,
        })
        .await,
    );
    assert_ne!(previous.active.as_ref(), Some(&panes[1]));

    let error = ui
        .try_ask(Request::FocusPane {
            session_id: session.id.clone(),
            target: FocusTarget::Pane {
                pane_id: turn_core::ids::PaneId::from_stored("pane_nothere"),
            },
        })
        .await
        .expect_err("an unknown pane");
    assert_eq!(error.code, ErrorCode::NotFound);

    // Attaching and detaching is about who is watching, not about what is running.
    let node = details.tree[0].node_id.clone();
    let pid = details.tree[0].pid.expect("a pid");
    let pane_of_node = details.tree[0].pane_bindings[0].pane_id.clone();
    ui.attach_cells(&session.id, &pane_of_node, PtySize::new(24, 80))
        .await;
    ui.ask(Request::ResizePty {
        session_id: session.id.clone(),
        node_id: node.clone(),
        size: PtySize::new(50, 200),
    })
    .await;
    ui.ask(Request::DetachPane {
        session_id: session.id.clone(),
        pane_id: pane_of_node.clone(),
    })
    .await;
    assert!(pid_is_alive(pid), "detaching must not stop the process");

    // Closing a pane while keeping its process leaves the process visible in the tree
    // with no pane, which is how a background job keeps its place.
    let layout = layout_of(
        ui.ask(Request::ClosePane {
            session_id: session.id.clone(),
            pane_id: pane_of_node.clone(),
            disposition: CloseDisposition::KeepProcesses,
        })
        .await,
    );
    assert_eq!(layout.pane_count(), 1);
    assert!(pid_is_alive(pid));
    let tree = tree_of(
        ui.ask(Request::GetProcessTree {
            session_id: session.id.clone(),
        })
        .await,
    );
    let kept = tree
        .iter()
        .find(|view| view.node_id == node)
        .expect("the process is still tracked");
    assert!(kept.lifecycle.is_running());
    assert!(
        kept.pane_bindings.is_empty(),
        "it has no pane, and it is not hidden"
    );

    daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_process_can_be_interrupted_and_killed_and_a_plain_one_has_no_turn_to_correct() {
    let daemon = TestDaemon::start_plain().await;
    let mut ui = daemon.connect().await;
    let workspace = workspace(&daemon, &mut ui, "control").await;
    // `cat` rather than a shell: an interactive shell installs its own signal handling
    // and may well shrug off an interrupt, which is correct of it and useless here. This
    // test is about the interrupt being *delivered*, so it needs a process that does not
    // catch it.
    let session = session_of(
        ui.ask(Request::CreateSession {
            workspace_id: workspace.id.clone(),
            name: "control".to_string(),
            cwd: None,
            panes: Some(vec![NewPane::new(PaneKind::Terminal).with_command("cat")]),
            note: None,
            tags: Vec::new(),
        })
        .await,
    );
    let details = details_of(
        ui.ask(Request::GetSession {
            session_id: session.id.clone(),
        })
        .await,
    );
    let node = details.tree[0].node_id.clone();
    let pid = details.tree[0].pid.expect("a pid");
    assert!(pid_is_alive(pid));

    // The interrupt is the control character written to the tty, not `kill(pid)`, which
    // is what makes it reach the whole foreground process group — the `cargo test` an
    // agent started, not only the agent. `cat` does not catch it, so it dies: the
    // observable proof that the tty delivered the signal.
    ui.ask(Request::InterruptNode {
        session_id: session.id.clone(),
        node_id: node.clone(),
    })
    .await;
    let ended = ui
        .wait_for("the interrupt to be delivered", |event| match event {
            ServerEvent::NodeStateChanged {
                node_id, lifecycle, ..
            } if node_id == &node && !lifecycle.is_running() => Some(lifecycle.clone()),
            _ => None,
        })
        .await;
    assert!(
        matches!(ended, turn_core::state::Lifecycle::Signaled { .. }),
        "a signal delivered through the tty, not an exit code: {ended:?}"
    );

    // A plain terminal has no turn axis, and giving it one would put a state in the UI
    // that nothing could ever move again.
    let error = ui
        .try_ask(Request::CorrectState {
            session_id: session.id.clone(),
            node_id: node.clone(),
            lifecycle: None,
            turn: Some(Turn::Active),
            note: None,
        })
        .await
        .expect_err("a plain terminal has no turn state");
    assert_eq!(error.code, ErrorCode::Conflict);

    let error = ui
        .try_ask(Request::CorrectState {
            session_id: session.id.clone(),
            node_id: node.clone(),
            lifecycle: None,
            turn: None,
            note: Some("something is wrong".to_string()),
        })
        .await
        .expect_err("a correction has to say what the state actually is");
    assert_eq!(error.code, ErrorCode::InvalidArgument);

    // Nothing to interrupt, kill or write to any more, and the refusal says so rather
    // than reporting a success that did nothing.
    for request in [
        Request::InterruptNode {
            session_id: session.id.clone(),
            node_id: node.clone(),
        },
        Request::KillNode {
            session_id: session.id.clone(),
            node_id: node.clone(),
        },
        Request::TerminateNode {
            session_id: session.id.clone(),
            node_id: node.clone(),
        },
    ] {
        let op = request.op();
        match ui.try_ask(request).await {
            Err(error) => assert_eq!(error.code, ErrorCode::ProcessNotRunning, "{op}"),
            Ok(response) => panic!("{op} must be refused, got {response:?}"),
        }
    }

    daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acknowledging_a_demand_keeps_it_in_the_queue_but_ranks_it_below_a_new_one() {
    let daemon = TestDaemon::start().await;
    let mut ui = daemon.connect().await;
    let workspace = workspace(&daemon, &mut ui, "attention").await;
    let session = session_of(
        ui.ask(Request::CreateSession {
            workspace_id: workspace.id.clone(),
            name: "asks twice".to_string(),
            cwd: None,
            panes: Some(vec![NewPane::new(PaneKind::Agent).with_command("cat")]),
            note: None,
            tags: Vec::new(),
        })
        .await,
    );
    let details = details_of(
        ui.ask(Request::GetSession {
            session_id: session.id.clone(),
        })
        .await,
    );
    let node = details.tree[0].node_id.clone();
    let hook = hook_url(daemon.data_dir(), &session.id, &node);

    post_hook(&hook, &notification("agent_needs_input", "Anything?")).await;
    let entry = ui
        .wait_for("the demand", |event| match event {
            ServerEvent::AttentionQueueChanged { entries } if !entries.is_empty() => {
                Some(entries[0].entry.id.clone())
            }
            _ => None,
        })
        .await;

    let before = attention_list_of(ui.ask(Request::ListAttention { session_id: None }).await);
    let score_before = before[0].score;

    ui.ask(Request::AcknowledgeAttention {
        attention_id: entry.clone(),
    })
    .await;

    let after = attention_list_of(ui.ask(Request::ListAttention { session_id: None }).await);
    assert_eq!(after.len(), 1, "seen is not the same as dealt with");
    assert_eq!(after[0].entry.id, entry);
    assert!(matches!(
        after[0].entry.state,
        turn_core::attention::EntryState::Acknowledged
    ));
    assert!(
        after[0].score < score_before,
        "an acknowledged demand ranks below a pending one: {} then {}",
        score_before,
        after[0].score
    );

    let error = ui
        .try_ask(Request::AcknowledgeAttention {
            attention_id: turn_core::ids::AttentionId::from_stored("attn_nothere"),
        })
        .await
        .expect_err("an unknown entry");
    assert_eq!(error.code, ErrorCode::NotFound);

    daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_command_turn_cannot_find_is_reported_as_missing_and_the_rest_still_starts() {
    let daemon = TestDaemon::start_plain().await;
    let mut ui = daemon.connect().await;
    let workspace = workspace(&daemon, &mut ui, "missing tools").await;

    // One pane runs something that is not installed; the other is a shell. A template
    // mentioning a tool the user has not got must still give them the rest of their desk.
    let session = session_of(
        ui.ask(Request::CreateSession {
            workspace_id: workspace.id.clone(),
            name: "half a desk".to_string(),
            cwd: None,
            panes: Some(vec![
                NewPane::new(PaneKind::Terminal).with_command("definitely-not-installed-9f2a"),
                NewPane::new(PaneKind::Shell),
            ]),
            note: None,
            tags: Vec::new(),
        })
        .await,
    );
    assert_eq!(session.pane_count, 2);
    assert_eq!(
        session.running_count, 1,
        "the pane whose command is missing is left empty rather than failing the session"
    );

    let details = details_of(
        ui.ask(Request::GetSession {
            session_id: session.id.clone(),
        })
        .await,
    );
    let empty = details
        .layout
        .panes()
        .into_iter()
        .find(|pane| pane.command.as_deref() == Some("definitely-not-installed-9f2a"))
        .cloned()
        .expect("the pane is still there");
    assert!(empty.node_id.is_none());

    // Attaching to it works, and says plainly that there is nothing behind it.
    let attachment = ui
        .attach_cells(&session.id, &empty.id, PtySize::new(24, 80))
        .await;
    assert_eq!(attachment.node_id, None);
    assert!(attachment.replay.is_empty());
    assert_eq!(attachment.bytes_seen, 0);
    // A blank screen at the client's own size, rather than nothing to draw.
    let screen = attachment.screen.as_ref().expect("a screen all the same");
    assert_eq!((screen.rows, screen.cols), (24, 80));
    assert!(
        screen.text().trim().is_empty(),
        "an empty pane must not have contents invented for it: {:?}",
        screen.text()
    );

    daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_request_before_the_handshake_is_refused_and_the_connection_ends() {
    let daemon = TestDaemon::start().await;

    // No `hello` first. A daemon that served this would have no idea what dialect the
    // peer speaks.
    let mut raw = tokio::net::UnixStream::connect(daemon.socket())
        .await
        .expect("the socket must accept");
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    // At the current version, so the refusal is unambiguously about the missing
    // handshake rather than about the dialect.
    raw.write_all(
        format!(
            r#"{{"v":{},"type":"request","id":"r-1","request":{{"op":"list_templates"}}}}"#,
            turn_proto::PROTOCOL_VERSION
        )
        .as_bytes(),
    )
    .await
    .unwrap();
    raw.write_all(b"\n").await.unwrap();
    raw.flush().await.unwrap();

    let mut buffer = vec![0u8; 4096];
    let read = raw.read(&mut buffer).await.expect("an answer");
    let line = String::from_utf8_lossy(&buffer[..read]);
    assert!(
        line.contains("handshake_required"),
        "expected a handshake_required error, got {line}"
    );

    // And a client that does handshake is unaffected.
    let mut ui = daemon.connect().await;
    ui.ask(Request::ListTemplates).await;

    daemon.shutdown().await;
}

/// A user who stops something on purpose has not had a failure, and the row must not say
/// they have. A badge that fires when nothing is wrong is how people learn to ignore the
/// one that matters.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stopping_a_process_on_purpose_reads_as_stopped_rather_than_failed() {
    let daemon = TestDaemon::start_plain().await;
    let mut ui = daemon.connect().await;
    let workspace = workspace(&daemon, &mut ui, "deliberate").await;
    // `cat` again: it ignores nothing, so `SIGTERM` really does kill it, which is the
    // case that used to be recorded as a signal death and shown in red.
    let session = session_of(
        ui.ask(Request::CreateSession {
            workspace_id: workspace.id.clone(),
            name: "stop me".to_string(),
            cwd: None,
            panes: Some(vec![NewPane::new(PaneKind::Terminal).with_command("cat")]),
            note: None,
            tags: Vec::new(),
        })
        .await,
    );
    let details = details_of(
        ui.ask(Request::GetSession {
            session_id: session.id.clone(),
        })
        .await,
    );
    let node = details.tree[0].node_id.clone();

    ui.ask(Request::TerminateNode {
        session_id: session.id.clone(),
        node_id: node.clone(),
    })
    .await;

    let state = ui
        .wait_for("the process to stop", |event| match event {
            ServerEvent::NodeStateChanged {
                node_id,
                lifecycle,
                display_state,
                ..
            } if node_id == &node && !lifecycle.is_running() => Some(*display_state),
            _ => None,
        })
        .await;
    assert_eq!(
        state,
        turn_core::state::DisplayState::Stopped,
        "the user asked for this; it is not a failure"
    );

    let after = details_of(
        ui.ask(Request::GetSession {
            session_id: session.id.clone(),
        })
        .await,
    );
    assert_ne!(
        after.summary.display_state,
        turn_core::state::DisplayState::Failed,
        "and the session it was in is not flagged either"
    );
    let stopped = after
        .tree
        .iter()
        .find(|view| view.node_id == node)
        .expect("the node stays in the tree");
    assert!(!stopped.lifecycle.is_failure(), "{:?}", stopped.lifecycle);

    daemon.shutdown().await;
}

/// A relaunch replaces the process behind a pane, and what the old one was configured
/// with has to go with it: a settings file naming a revoked hook URL and token is worse
/// than no file, and it would sit there for as long as the session lives.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn relaunching_a_pane_takes_the_old_launchs_configuration_with_it() {
    let daemon = TestDaemon::start().await;
    let mut ui = daemon.connect().await;
    let workspace = workspace(&daemon, &mut ui, "relaunch").await;
    let session = session_of(
        ui.ask(Request::CreateSession {
            workspace_id: workspace.id.clone(),
            name: "start again".to_string(),
            cwd: None,
            panes: Some(vec![NewPane::new(PaneKind::Agent).with_command("cat")]),
            note: None,
            tags: Vec::new(),
        })
        .await,
    );
    let details = details_of(
        ui.ask(Request::GetSession {
            session_id: session.id.clone(),
        })
        .await,
    );
    let old_node = details.tree[0].node_id.clone();
    let old_scratch = turnd::paths::node_scratch(daemon.data_dir(), &session.id, &old_node);
    wait_for_path(&old_scratch).await;

    ui.ask(Request::TerminateNode {
        session_id: session.id.clone(),
        node_id: old_node.clone(),
    })
    .await;
    ui.wait_for("the process to stop", |event| match event {
        ServerEvent::NodeStateChanged {
            node_id, lifecycle, ..
        } if node_id == &old_node && !lifecycle.is_running() => Some(()),
        _ => None,
    })
    .await;

    let fresh = node_of(
        ui.ask(Request::RelaunchNode {
            session_id: session.id.clone(),
            node_id: old_node.clone(),
            resume: false,
        })
        .await,
    );
    assert_ne!(fresh.node_id, old_node, "a relaunch is a new process");
    assert!(
        !old_scratch.exists(),
        "the retired launch's configuration is still on disk at {}",
        old_scratch.display()
    );
    // And the new one has its own, with the token that is actually live.
    let fresh_scratch = turnd::paths::node_scratch(daemon.data_dir(), &session.id, &fresh.node_id);
    wait_for_path(&fresh_scratch).await;
    assert!(
        turnd::paths::session_scratch(daemon.data_dir(), &session.id).exists(),
        "the session's own scratch space stays"
    );

    daemon.shutdown().await;
}
