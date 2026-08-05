//! The end-to-end contract: shut the daemon down, start it again, and the user's
//! desk is back — with an honest account of what is still running.
//!
//! These go through the public API only, in the order the daemon uses it.

use turn_core::ids::SessionId;
use turn_core::model::layout::{Pane, PaneKind, RestoreBehaviour};
use turn_core::model::node::{NodeKind, Relation};
use turn_core::model::session::RestoreState;
use turn_core::model::{Layout, ProcessNode, Session, Template, Workspace};
use turn_core::state::{AwaitingReason, DisplayState, Lifecycle, Turn};
use turn_core::{AttentionPolicy, Confidence, EventKind, EventSource, TurnEvent};
use turn_store::{Retention, Store};

const T0: i64 = 1_700_000_000_000;

/// Builds the state of a working afternoon: two workspaces, three sessions, an
/// agent with a subagent and a test runner, an event log and a pending demand.
fn seed(store: &Store) -> SessionId {
    store.templates().install_built_ins(T0).unwrap();
    // A working desk may have richer user presets, but the application itself
    // ships only the portable Two Shells starter.
    let mut coding = Template::coding(T0);
    coding.built_in = false;
    store.templates().save(&coding).unwrap();
    let roots = store
        .path()
        .unwrap()
        .parent()
        .unwrap()
        .join("workspace-roots");
    let turn_root = roots.join("turn");
    let website_root = roots.join("website");
    std::fs::create_dir_all(&turn_root).unwrap();
    std::fs::create_dir_all(&website_root).unwrap();

    let mut turn_ws = Workspace::new("turn", turn_root.to_string_lossy(), T0);
    turn_ws.default_template = Some(coding.id.clone());
    turn_ws.default_agent = Some("claude".into());
    turn_ws.init_commands = vec!["nvm use".into()];
    store.workspaces().save(&turn_ws).unwrap();

    let other_ws = Workspace::new("website", website_root.to_string_lossy(), T0);
    store.workspaces().save(&other_ws).unwrap();

    // The session under test, laid out from the built-in template.
    let mut session = Session::new(
        turn_ws.id.clone(),
        "Fix climbing bugs",
        "/repos/turn",
        coding.instantiate(),
        T0,
    );
    session.template_id = Some(coding.id.clone());
    session.git_branch = Some("fix/climbing".into());
    session.linked_ref = Some("#104".into());
    session.tags = vec!["bug".into()];
    session.pinned = true;
    session.attention = AttentionPolicy::silent();

    let agent_pane = session.layout.panes()[0].id.clone();
    let mut agent = ProcessNode::agent(session.id.clone(), "claude", "/repos/turn", T0);
    agent.pid = Some(4242);
    agent.lifecycle = Lifecycle::Alive;
    agent.turn = Some(Turn::AwaitingUser {
        reason: AwaitingReason::Permission,
    });
    agent.interaction_pending = true;
    if let Some(info) = agent.agent.as_mut() {
        info.external_id = Some("claude-thread-9f2c".into());
        info.resumable = true;
        info.last_message = Some("May I run make verify?".into());
    }
    let agent_id = session.tree.insert(agent);
    if let Some(pane) = session.layout.get_mut(&agent_pane) {
        pane.node_id = Some(agent_id.clone());
    }

    let mut subagent = ProcessNode::agent(session.id.clone(), "explore", "/repos/turn", T0 + 100);
    subagent.kind = NodeKind::Subagent;
    subagent.lifecycle = Lifecycle::Alive;
    subagent.turn = Some(Turn::Active);
    subagent.link_to(agent_id.clone(), Relation::Confirmed);
    session.tree.insert(subagent);

    let mut tests = ProcessNode::process(
        session.id.clone(),
        NodeKind::TestRunner,
        "cargo test",
        "/repos/turn",
        T0 + 200,
    );
    tests.pid = Some(4300);
    tests.lifecycle = Lifecycle::Alive;
    // The process table said this looked like a child; nothing confirmed it.
    tests.link_to(agent_id.clone(), Relation::Inferred);
    session.tree.insert(tests);

    store.sessions().save(&session).unwrap();

    // A second, quieter session in the same workspace.
    let mut quiet = Session::new(
        turn_ws.id.clone(),
        "Read the docs",
        "/repos/turn",
        Layout::single(Pane::new(PaneKind::Shell).with_restore(RestoreBehaviour::Relaunch)),
        T0 - 60_000,
    );
    quiet.last_activity_ms = T0 - 60_000;
    store.sessions().save(&quiet).unwrap();

    // And one somewhere else entirely.
    let elsewhere = Session::new(
        other_ws.id.clone(),
        "Ship the landing page",
        "/repos/website",
        Layout::single(Pane::new(PaneKind::Agent).with_command("codex")),
        T0,
    );
    store.sessions().save(&elsewhere).unwrap();

    let events = vec![
        TurnEvent::new(
            session.id.clone(),
            EventKind::AgentStarted {
                tool: "claude-code".into(),
                model: Some("opus".into()),
                external_id: Some("claude-thread-9f2c".into()),
            },
            EventSource::Hook {
                tool: "claude-code".into(),
                event_name: "SessionStart".into(),
            },
            Confidence::Explicit,
            T0 + 1,
        )
        .with_node(agent_id.clone()),
        TurnEvent::new(
            session.id.clone(),
            EventKind::AgentTurnCompleted {
                last_message: Some("tests are running".into()),
                background_tasks: 1,
            },
            EventSource::Hook {
                tool: "claude-code".into(),
                event_name: "Stop".into(),
            },
            Confidence::Explicit,
            T0 + 2,
        )
        .with_node(agent_id.clone()),
        // A guess, from watching the terminal.
        TurnEvent::new(
            session.id.clone(),
            EventKind::AgentWaitingForUser {
                reason: AwaitingReason::Input,
                summary: Some("prompt looks idle".into()),
            },
            EventSource::PtyHeuristic {
                rule: "idle_prompt".into(),
            },
            Confidence::Explicit,
            T0 + 3,
        )
        .with_node(agent_id.clone()),
    ];
    store.events().append_all(&events).unwrap();

    store
        .settings()
        .set("ui.sidebar_width", &280_i64, T0)
        .unwrap();
    store
        .settings()
        .set("attention.default", &AttentionPolicy::default(), T0)
        .unwrap();

    session.id
}

#[test]
fn a_whole_desk_survives_a_restart_and_reports_what_it_cannot_vouch_for() {
    let temp = tempfile::tempdir().unwrap();
    let session_id = {
        let store = Store::open_in(temp.path()).unwrap();
        seed(&store)
    };

    // A brand new daemon, with nothing in memory.
    let store = Store::open_in(temp.path()).unwrap();

    let workspaces = store.workspaces().list_active().unwrap();
    assert_eq!(workspaces.len(), 2);
    let turn_ws = workspaces
        .iter()
        .find(|w| w.name == "turn")
        .expect("the workspace is back");
    assert_eq!(turn_ws.init_commands, vec!["nvm use".to_string()]);
    assert!(turn_ws.default_template.is_some());

    let sessions = store.sessions().list_for_workspace(&turn_ws.id).unwrap();
    assert_eq!(sessions.len(), 2, "and only this workspace's sessions");
    assert_eq!(sessions[0].name, "Fix climbing bugs", "most recent first");

    let restored = store
        .sessions()
        .load_for_restore(&session_id)
        .unwrap()
        .expect("the session is back");

    // The layout is exactly the shape the user left.
    assert_eq!(restored.layout.pane_count(), 3);
    assert!(restored.layout.sizes_are_normalised());
    assert!(restored.layout.active.is_some());
    assert_eq!(restored.git_branch.as_deref(), Some("fix/climbing"));
    assert_eq!(restored.linked_ref.as_deref(), Some("#104"));
    assert!(restored.pinned);
    assert_eq!(restored.attention, AttentionPolicy::silent());

    // The process tree kept its shape, including how sure each link is.
    assert_eq!(restored.tree.len(), 3);
    assert_eq!(restored.tree.roots().len(), 1);
    assert_eq!(restored.tree.subagent_count(), 1);
    let agent = restored.tree.primary_agent().expect("the agent is back");
    assert_eq!(restored.tree.children(&agent.id).len(), 2);
    let inferred = restored
        .tree
        .children(&agent.id)
        .into_iter()
        .find(|n| n.kind == NodeKind::TestRunner)
        .expect("the test runner is back");
    assert_eq!(
        inferred.relation,
        Relation::Inferred,
        "a guessed parent link must not be promoted by a round trip"
    );
    assert!(inferred.relation.is_provisional());

    // Enough metadata to attempt a re-attach.
    assert_eq!(agent.pid, Some(4242));
    assert_eq!(agent.command, "claude");
    assert_eq!(agent.cwd, "/repos/turn");
    assert_eq!(
        agent.agent.as_ref().unwrap().external_id.as_deref(),
        Some("claude-thread-9f2c")
    );
    assert!(agent.agent.as_ref().unwrap().resumable);

    // And an honest verdict: we do not own these processes any more.
    for node in restored.tree.iter() {
        assert_eq!(
            node.lifecycle,
            Lifecycle::Orphaned,
            "{} claimed to be {:?} after a restart",
            node.command,
            node.lifecycle
        );
        assert!(node.lifecycle.is_running());
        assert!(
            !node.lifecycle.is_terminal(),
            "nothing is declared dead for us"
        );
    }

    // The agent's last turn state is preserved, so the sidebar can say why the
    // session needs the user before anything re-attaches.
    assert_eq!(
        agent.turn,
        Some(Turn::AwaitingUser {
            reason: AwaitingReason::Permission
        })
    );
    assert_eq!(restored.display_state(), DisplayState::NeedsPermission);
    assert!(restored.needs_user());
}

#[test]
fn the_event_log_still_says_which_states_were_guesses() {
    let temp = tempfile::tempdir().unwrap();
    let session_id = {
        let store = Store::open_in(temp.path()).unwrap();
        seed(&store)
    };

    let store = Store::open_in(temp.path()).unwrap();
    let events = store.events().list_for_session(&session_id, 100).unwrap();
    assert_eq!(events.len(), 3);

    let guessed = events
        .iter()
        .find(|e| matches!(e.kind, EventKind::AgentWaitingForUser { .. }))
        .expect("the heuristic event is stored");
    assert_eq!(guessed.confidence, Confidence::InferredHigh);
    assert!(guessed.confidence.is_provisional());
    assert!(!guessed.confidence.may_steal_focus());
    assert_eq!(
        guessed.source,
        EventSource::PtyHeuristic {
            rule: "idle_prompt".into()
        },
        "Turn can still name the rule that guessed, weeks later"
    );

    let stop = events
        .iter()
        .find(|e| matches!(e.kind, EventKind::AgentTurnCompleted { .. }))
        .expect("the Stop hook event is stored");
    assert_eq!(stop.confidence, Confidence::Explicit);
    assert!(matches!(
        stop.kind,
        EventKind::AgentTurnCompleted {
            background_tasks: 1,
            ..
        }
    ));
}

#[test]
fn a_partial_restore_is_recorded_so_the_ui_can_explain_itself() {
    let temp = tempfile::tempdir().unwrap();
    let session_id = {
        let store = Store::open_in(temp.path()).unwrap();
        seed(&store)
    };

    // The supervisor looked in the process table, re-attached one process and
    // could not find the other two. That verdict is the supervisor's to write.
    let store = Store::open_in(temp.path()).unwrap();
    let mut restored = store
        .sessions()
        .load_for_restore(&session_id)
        .unwrap()
        .unwrap();
    let ids: Vec<_> = restored.tree.iter().map(|n| n.id.clone()).collect();
    restored.tree.get_mut(&ids[0]).unwrap().lifecycle = Lifecycle::Reconnected;
    for id in ids.iter().skip(1) {
        restored.tree.get_mut(id).unwrap().lifecycle = Lifecycle::Lost;
    }
    restored.restore_state = RestoreState::PartiallyRestored;
    store.sessions().save(&restored).unwrap();

    let store = Store::open_in(temp.path()).unwrap();
    let again = store.sessions().get(&session_id).unwrap().unwrap();
    assert_eq!(again.restore_state, RestoreState::PartiallyRestored);
    assert!(again.restore_state.needs_explanation());
    assert_eq!(
        again
            .tree
            .iter()
            .filter(|n| n.lifecycle == Lifecycle::Lost)
            .count(),
        2
    );

    // `Lost` is terminal, so it is never re-orphaned into looking alive again.
    let for_restore = store
        .sessions()
        .load_for_restore(&session_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        for_restore
            .tree
            .iter()
            .filter(|n| n.lifecycle == Lifecycle::Lost)
            .count(),
        2,
        "a process already given up on must not be resurrected as orphaned"
    );
}

#[test]
fn settings_templates_and_preferences_come_back_too() {
    let temp = tempfile::tempdir().unwrap();
    {
        let store = Store::open_in(temp.path()).unwrap();
        seed(&store);
    }

    let store = Store::open_in(temp.path()).unwrap();
    assert_eq!(
        store.settings().get::<i64>("ui.sidebar_width").unwrap(),
        Some(280)
    );
    assert_eq!(
        store
            .settings()
            .get::<AttentionPolicy>("attention.default")
            .unwrap(),
        Some(AttentionPolicy::default())
    );

    let templates = store.templates().list().unwrap();
    assert_eq!(templates.len(), 2);
    // Installing again on this launch adds nothing.
    assert_eq!(store.templates().install_built_ins(T0 + 1).unwrap(), 0);
    assert_eq!(store.templates().count().unwrap(), 2);
    assert_eq!(templates.iter().filter(|t| t.built_in).count(), 1);
    assert!(templates
        .iter()
        .any(|template| template.built_in && template.name == "Two Shells"));
    assert!(templates
        .iter()
        .any(|template| !template.built_in && template.name == "Coding"));
}

#[test]
fn a_long_running_install_prunes_its_history_without_losing_the_recent_past() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open_in(temp.path()).unwrap();
    let session_id = seed(&store);

    // A year of chatter on one session.
    let day = 24 * 60 * 60 * 1_000_i64;
    let noise: Vec<TurnEvent> = (0..400)
        .map(|i| {
            TurnEvent::new(
                session_id.clone(),
                EventKind::AgentIdle,
                EventSource::Supervisor,
                Confidence::Explicit,
                T0 - i * day,
            )
        })
        .collect();
    store.events().append_all(&noise).unwrap();
    assert_eq!(store.events().count().unwrap(), 403);

    let outcome = store.events().prune(&Retention::default(), T0).unwrap();
    assert!(outcome.total() > 300, "old chatter is dropped: {outcome:?}");

    let left = store.events().list_for_session(&session_id, 1_000).unwrap();
    assert_eq!(left.len(), Retention::default().keep_per_session);
    assert_eq!(
        left[0].timestamp_ms,
        T0 + 3,
        "the newest event is the one that is definitely kept"
    );

    // The rest of the desk is untouched by pruning.
    let session = store.sessions().get(&session_id).unwrap().unwrap();
    assert_eq!(session.tree.len(), 3);
    assert_eq!(session.layout.pane_count(), 3);
}

#[test]
fn a_pending_demand_for_the_user_outlives_the_daemon_that_recorded_it() {
    use turn_core::attention::{AttentionEntry, EntryState};
    use turn_core::ids::AttentionId;

    let temp = tempfile::tempdir().unwrap();
    let session_id = {
        let store = Store::open_in(temp.path()).unwrap();
        let session_id = seed(&store);
        let node = store
            .sessions()
            .get(&session_id)
            .unwrap()
            .unwrap()
            .tree
            .primary_agent()
            .unwrap()
            .id
            .clone();
        store
            .attention()
            .upsert(&AttentionEntry {
                id: AttentionId::new(),
                session_id: session_id.clone(),
                node_id: Some(node),
                parent_node_id: None,
                subject_external_id: None,
                reason: AwaitingReason::Permission,
                summary: Some("run make verify".into()),
                confidence: Confidence::Explicit,
                created_ms: T0,
                updated_ms: T0,
                state: EntryState::Pending,
                priority_boost: -20,
                survives_owner_exit: false,
                demand_kind: Default::default(),
            })
            .unwrap();
        session_id
    };

    let store = Store::open_in(temp.path()).unwrap();
    let queue = store.attention().load_queue().unwrap();
    assert_eq!(queue.len(), 1);

    let next = queue.next(T0 + 5_000).expect("the demand is still queued");
    assert_eq!(next.session_id, session_id);
    assert_eq!(next.reason, AwaitingReason::Permission);
    assert_eq!(next.summary.as_deref(), Some("run make verify"));
    assert_eq!(
        next.created_ms, T0,
        "its age is preserved, so it does not lose its place"
    );
    assert!(next.node_id.is_some(), "and it knows which pane to jump to");
}

#[test]
fn a_second_session_from_the_same_template_is_stored_independently() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open_in(temp.path()).unwrap();
    seed(&store);

    let coding: Template = store.templates().find_by_name("Coding").unwrap().unwrap();
    let workspace = store
        .workspaces()
        .list_active()
        .unwrap()
        .into_iter()
        .find(|w| w.name == "turn")
        .unwrap();

    let first = Session::new(
        workspace.id.clone(),
        "Review the PR",
        "/repos/turn",
        coding.instantiate(),
        T0 + 1_000,
    );
    let second = Session::new(
        workspace.id.clone(),
        "Review the other PR",
        "/repos/turn",
        coding.instantiate(),
        T0 + 2_000,
    );
    store.sessions().save(&first).unwrap();
    store.sessions().save(&second).unwrap();

    let stored_first = store.sessions().get(&first.id).unwrap().unwrap();
    let stored_second = store.sessions().get(&second.id).unwrap().unwrap();
    let first_panes: Vec<_> = stored_first
        .layout
        .panes()
        .iter()
        .map(|p| p.id.clone())
        .collect();
    for pane in stored_second.layout.panes() {
        assert!(
            !first_panes.contains(&pane.id),
            "pane {} is shared between two sessions",
            pane.id
        );
    }
    assert_eq!(stored_first.layout.pane_count(), 3);
    assert_eq!(stored_second.layout.pane_count(), 3);
}

#[test]
fn closing_a_pane_in_a_stored_session_does_not_leave_the_process_row_behind() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open_in(temp.path()).unwrap();
    let session_id = seed(&store);

    let mut session = store.sessions().get(&session_id).unwrap().unwrap();
    let doomed = session
        .tree
        .iter()
        .find(|n| n.kind == NodeKind::TestRunner)
        .map(|n| n.id.clone())
        .unwrap();
    let pane = session.layout.panes()[2].id.clone();
    assert!(session.layout.close(&pane));
    session.tree.remove(&doomed);
    store.sessions().save(&session).unwrap();

    let store = Store::open_in(temp.path()).unwrap();
    let back = store.sessions().get(&session_id).unwrap().unwrap();
    assert_eq!(back.layout.pane_count(), 2);
    assert_eq!(back.tree.len(), 2);
    assert!(store.nodes().get(&doomed).unwrap().is_none());
    assert!(back.layout.sizes_are_normalised());
}

#[test]
fn two_stores_on_the_same_file_see_each_others_writes() {
    // The daemon is the only writer, but a CLI invocation reads the same file
    // while it runs. Write-ahead logging is what makes that safe.
    let temp = tempfile::tempdir().unwrap();
    let writer = Store::open_in(temp.path()).unwrap();
    let reader = Store::open_in(temp.path()).unwrap();

    let session_id = seed(&writer);
    let seen = reader
        .sessions()
        .get(&session_id)
        .unwrap()
        .expect("the reader sees the committed session");
    assert_eq!(seen.name, "Fix climbing bugs");
    assert_eq!(seen.tree.len(), 3);
    assert_eq!(reader.events().count_for_session(&session_id).unwrap(), 3);
}
