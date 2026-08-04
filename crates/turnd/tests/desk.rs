//! The desk: workspaces, sessions, panes, real processes and their output.

mod common;

use common::*;
use turn_core::model::{PaneKind, RestoreBehaviour};
use turn_proto::{
    CloseDisposition, ErrorCode, FocusTarget, NewPane, PtySize, Request, Response, ServerEvent,
};

/// Creates a workspace and a session from the built-in Coding template.
async fn coding_session(daemon: &TestDaemon, ui: &mut Client) -> turn_proto::SessionSummary {
    let workspace = workspace_of(
        ui.ask(Request::CreateWorkspace {
            name: "turn".to_string(),
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
        .expect("the built-in Coding template must be installed");

    session_of(
        ui.ask(Request::CreateSessionFromTemplate {
            workspace_id: workspace.id.clone(),
            template_id: coding.id.clone(),
            name: Some("Fix the flaky test".to_string()),
            cwd: None,
            branch: None,
            task: None,
        })
        .await,
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_from_the_coding_template_has_its_panes_and_real_processes_behind_them() {
    let daemon = TestDaemon::start().await;
    let mut ui = daemon.connect().await;
    let session = coding_session(&daemon, &mut ui).await;

    assert_eq!(session.name, "Fix the flaky test");
    assert_eq!(
        session.pane_count, 3,
        "the Coding template is an agent, a shell and the agent tree"
    );

    let details = details_of(
        ui.ask(Request::GetSession {
            session_id: session.id.clone(),
        })
        .await,
    );
    let panes = details.layout.panes();
    assert_eq!(panes.len(), 3);

    // The two terminal panes have processes; Turn's own view has none, and must not have
    // had something invented to put in it.
    let with_process: Vec<_> = panes.iter().filter(|pane| pane.node_id.is_some()).collect();
    assert_eq!(with_process.len(), 2, "panes with a process: {panes:#?}");
    let view_pane = panes
        .iter()
        .find(|pane| pane.kind == PaneKind::AgentTree)
        .expect("the agent tree pane");
    assert!(view_pane.node_id.is_none(), "a Turn view has no process");

    // Not "the daemon says it is running": the kernel says so.
    assert_eq!(details.tree.len(), 2);
    for node in &details.tree {
        let pid = node.pid.expect("a spawned process has a pid");
        assert!(
            pid_is_alive(pid),
            "{} claims pid {pid} is {:?} but the process table disagrees",
            node.title,
            node.lifecycle
        );
        assert!(node.lifecycle.is_running());
    }

    // The agent pane is an agent — it has the turn axis — and the shell is not.
    let agent = details
        .tree
        .iter()
        .find(|node| node.is_agentic)
        .expect("the agent pane's node");
    assert!(agent.turn.is_some(), "an agent carries the turn axis");
    let shell = details
        .tree
        .iter()
        .find(|node| !node.is_agentic)
        .expect("the shell pane's node");
    assert!(
        shell.turn.is_none(),
        "a shell owes the user nothing, so it has no turn state"
    );

    daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn writing_to_an_attached_pane_produces_the_processs_output() {
    let daemon = TestDaemon::start().await;
    let mut ui = daemon.connect().await;
    let session = coding_session(&daemon, &mut ui).await;

    let details = details_of(
        ui.ask(Request::GetSession {
            session_id: session.id.clone(),
        })
        .await,
    );
    let shell = details
        .layout
        .panes()
        .into_iter()
        .find(|pane| pane.kind == PaneKind::Shell)
        .cloned()
        .expect("the shell pane");
    let node = shell.node_id.clone().expect("the shell has a process");

    let attachment = ui
        .attach_cells(&session.id, &shell.id, PtySize::new(30, 100))
        .await;
    assert_eq!(attachment.node_id.as_ref(), Some(&node));
    assert_eq!(attachment.size, PtySize::new(30, 100));
    assert_eq!(attachment.next_seq, 0, "the live stream starts at zero");
    let screen = attachment.screen.as_ref().expect("the screen as cells");
    assert_eq!(
        (screen.rows, screen.cols),
        (30, 100),
        "the screen must arrive at the geometry the client asked for"
    );

    ui.ask(Request::WritePty {
        session_id: session.id.clone(),
        node_id: node.clone(),
        data: turn_proto::TerminalBytes::new(b"echo hello\n".to_vec()),
    })
    .await;

    // The cells the daemon pushed, applied the way a renderer applies them.
    let screen = ui.wait_for_screen("hello").await;
    assert!(screen.contains("hello"), "the screen reads {screen:?}");

    daemon.shutdown().await;
}

/// The headline claim, tested the only way that means anything: the client that was
/// watching goes away, a new one arrives, and the terminal is still there.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_client_rebuilds_a_terminal_the_previous_one_was_watching() {
    let daemon = TestDaemon::start().await;
    let mut first = daemon.connect().await;
    let session = coding_session(&daemon, &mut first).await;

    let details = details_of(
        first
            .ask(Request::GetSession {
                session_id: session.id.clone(),
            })
            .await,
    );
    let shell = details
        .layout
        .panes()
        .into_iter()
        .find(|pane| pane.kind == PaneKind::Shell)
        .cloned()
        .expect("the shell pane");
    let node = shell.node_id.clone().expect("the shell has a process");

    first
        .attach_cells(&session.id, &shell.id, PtySize::new(24, 80))
        .await;
    first
        .ask(Request::WritePty {
            session_id: session.id.clone(),
            node_id: node.clone(),
            data: turn_proto::TerminalBytes::new(b"echo marker-9f2a\n".to_vec()),
        })
        .await;
    first.wait_for_screen("marker-9f2a").await;

    // The UI goes away. The daemon holds the pty, so the process does not.
    drop(first);

    let mut second = daemon.connect().await;
    let attachment = second
        .attach_cells(&session.id, &shell.id, PtySize::new(24, 80))
        .await;
    let rebuilt = attachment
        .screen
        .as_ref()
        .expect("a cells attachment carries the screen")
        .text();
    assert!(
        rebuilt.contains("marker-9f2a"),
        "the screen must rebuild what the previous client saw; got {rebuilt:?}"
    );
    assert!(
        attachment.bytes_seen > 0,
        "the pane has produced output the daemon has been holding"
    );
    // Still the same process: nothing was relaunched to make this work.
    let tree = tree_of(
        second
            .ask(Request::GetProcessTree {
                session_id: session.id.clone(),
            })
            .await,
    );
    let same = tree
        .iter()
        .find(|view| view.node_id == node)
        .expect("the same node");
    assert!(same.lifecycle.is_running());

    daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_saved_layout_makes_a_second_session_of_the_same_shape_with_its_own_panes() {
    let daemon = TestDaemon::start().await;
    let mut ui = daemon.connect().await;
    let session = coding_session(&daemon, &mut ui).await;

    let original = details_of(
        ui.ask(Request::GetSession {
            session_id: session.id.clone(),
        })
        .await,
    );
    let template = match ui
        .ask(Request::SaveLayoutAsTemplate {
            session_id: session.id.clone(),
            name: "My desk".to_string(),
            description: Some("captured in a test".to_string()),
            hotkey: None,
        })
        .await
    {
        Response::Template { template } => template,
        other => panic!("expected a template, got {other:?}"),
    };
    assert_eq!(template.pane_count, 3);
    assert!(!template.built_in);

    let second = session_of(
        ui.ask(Request::CreateSessionFromTemplate {
            workspace_id: session.workspace_id.clone(),
            template_id: template.id.clone(),
            name: Some("Another run".to_string()),
            cwd: None,
            branch: None,
            task: None,
        })
        .await,
    );
    let copy = details_of(
        ui.ask(Request::GetSession {
            session_id: second.id.clone(),
        })
        .await,
    );

    let shape = |details: &turn_proto::SessionDetails| -> Vec<(PaneKind, Option<String>)> {
        details
            .layout
            .panes()
            .iter()
            .map(|pane| (pane.kind, pane.command.clone()))
            .collect()
    };
    assert_eq!(
        shape(&original),
        shape(&copy),
        "the same shape, pane for pane"
    );

    let original_ids: Vec<_> = original
        .layout
        .panes()
        .iter()
        .map(|p| p.id.clone())
        .collect();
    let copy_ids: Vec<_> = copy.layout.panes().iter().map(|p| p.id.clone()).collect();
    for id in &copy_ids {
        assert!(
            !original_ids.contains(id),
            "two sessions from one template must not share pane identity"
        );
    }
    // And its own processes, not the original's.
    let original_nodes: Vec<_> = original.tree.iter().map(|n| n.node_id.clone()).collect();
    for node in &copy.tree {
        assert!(!original_nodes.contains(&node.node_id));
    }

    daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pane_operations_answer_with_the_layout_and_tell_the_other_client() {
    let daemon = TestDaemon::start().await;
    let mut ui = daemon.connect().await;
    let mut observer = daemon.connect().await;
    let session = coding_session(&daemon, &mut ui).await;

    let first_pane = details_of(
        ui.ask(Request::GetSession {
            session_id: session.id.clone(),
        })
        .await,
    )
    .layout
    .panes()[0]
        .id
        .clone();

    let layout = layout_of(
        ui.ask(Request::SplitPane {
            session_id: session.id.clone(),
            pane_id: first_pane.clone(),
            direction: turn_core::model::Direction::Vertical,
            pane: NewPane::new(PaneKind::Shell).with_command("cat"),
        })
        .await,
    );
    assert_eq!(layout.pane_count(), 4);
    assert!(layout.sizes_are_normalised());

    // The other client is told, without having asked.
    let pushed = observer
        .wait_for("a layout change", |event| match event {
            ServerEvent::LayoutChanged { layout, session_id } if session_id == &session.id => {
                Some(layout.clone())
            }
            _ => None,
        })
        .await;
    assert_eq!(pushed.pane_count(), 4);

    // Zoom is a toggle and leaves the tree alone, so un-zooming restores the geometry.
    let zoomed = layout_of(
        ui.ask(Request::ZoomPane {
            session_id: session.id.clone(),
            pane_id: first_pane.clone(),
        })
        .await,
    );
    assert_eq!(zoomed.zoomed.as_ref(), Some(&first_pane));
    let unzoomed = layout_of(
        ui.ask(Request::ZoomPane {
            session_id: session.id.clone(),
            pane_id: first_pane.clone(),
        })
        .await,
    );
    assert_eq!(unzoomed.zoomed, None);
    assert_eq!(unzoomed.root, zoomed.root, "zooming must not move a pane");

    // Focus, and a clamped resize that cannot make a pane vanish.
    let focused = layout_of(
        ui.ask(Request::FocusPane {
            session_id: session.id.clone(),
            target: FocusTarget::Next,
        })
        .await,
    );
    assert!(focused.active.is_some());
    let resized = layout_of(
        ui.ask(Request::ResizePane {
            session_id: session.id.clone(),
            pane_id: first_pane.clone(),
            delta: 0.9,
        })
        .await,
    );
    assert!(
        resized.sizes_are_normalised(),
        "a clamped resize still leaves the split adding up"
    );

    daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn closing_a_session_does_exactly_what_the_disposition_says() {
    let daemon = TestDaemon::start().await;
    let mut ui = daemon.connect().await;
    let session = coding_session(&daemon, &mut ui).await;
    let details = details_of(
        ui.ask(Request::GetSession {
            session_id: session.id.clone(),
        })
        .await,
    );
    let pids: Vec<u32> = details.tree.iter().filter_map(|node| node.pid).collect();
    assert_eq!(pids.len(), 2);

    // Keeping the processes is the whole point of the daemon: the window closes, the
    // work does not.
    ui.ask(Request::CloseSession {
        session_id: session.id.clone(),
        disposition: CloseDisposition::KeepProcesses,
    })
    .await;
    for pid in &pids {
        assert!(pid_is_alive(*pid), "pid {pid} must survive a detach");
    }

    // The injected agent configuration lives under Turn's own data directory, keyed by
    // session, and goes when the session's processes do.
    let agent_node = details
        .tree
        .iter()
        .find(|node| node.is_agentic)
        .expect("the agent node");
    let scratch = turnd::paths::node_scratch(daemon.data_dir(), &session.id, &agent_node.node_id);
    assert!(scratch.exists(), "the adapter's scratch directory");

    ui.ask(Request::CloseSession {
        session_id: session.id.clone(),
        disposition: CloseDisposition::Terminate,
    })
    .await;
    assert!(
        !turnd::paths::session_scratch(daemon.data_dir(), &session.id).exists(),
        "closing a session takes its injected configuration with it"
    );

    // The processes are asked to stop, and the session is parked rather than deleted.
    let mut stopped = false;
    // Generous: this waits on the operating system reaping two processes, which a loaded
    // machine can take its time over. A test that fails there fails for no reason.
    for _ in 0..200 {
        if pids.iter().all(|pid| !pid_is_alive(*pid)) {
            stopped = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(stopped, "terminate must actually stop the processes");
    let after = details_of(
        ui.ask(Request::GetSession {
            session_id: session.id.clone(),
        })
        .await,
    );
    assert_eq!(
        after.summary.status,
        turn_core::model::SessionStatus::Paused
    );

    daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_daemon_refuses_the_things_it_should_and_says_why() {
    let daemon = TestDaemon::start().await;
    let mut ui = daemon.connect().await;
    let session = coding_session(&daemon, &mut ui).await;
    let details = details_of(
        ui.ask(Request::GetSession {
            session_id: session.id.clone(),
        })
        .await,
    );
    let pane = details.layout.panes()[0].id.clone();

    let unknown_session = turn_core::ids::SessionId::from_stored("sess_nothere");
    let error = ui
        .try_ask(Request::GetSession {
            session_id: unknown_session.clone(),
        })
        .await
        .expect_err("an unknown session is not found");
    assert_eq!(error.code, ErrorCode::NotFound);

    let error = ui
        .try_ask(Request::ResizePane {
            session_id: session.id.clone(),
            pane_id: pane.clone(),
            delta: 2.0,
        })
        .await
        .expect_err("a delta is a fraction of the split");
    assert_eq!(error.code, ErrorCode::InvalidArgument);

    // A float too large for an `f32` arrives as infinity, which would propagate into
    // every sibling's size. It has to be sent by hand: `serde_json` writes a non-finite
    // float as `null`, so a typed client cannot even express it.
    ui.send_raw(&format!(
        r#"{{"v":{},"type":"request","id":"raw-1","request":{{"op":"resize_pane","session_id":"{}","pane_id":"{}","delta":1e40}}}}"#,
        turn_proto::PROTOCOL_VERSION, session.id, pane
    ))
    .await;
    let error = ui.expect_error().await;
    assert_eq!(error.code, ErrorCode::InvalidArgument, "{error}");

    // A line that is not a message costs one line, and the connection carries on.
    ui.send_raw("this is not json").await;
    let error = ui.expect_error().await;
    assert_eq!(error.code, ErrorCode::MalformedMessage);
    ui.ask(Request::ListTemplates).await;

    let error = ui
        .try_ask(Request::DetachPane {
            session_id: session.id.clone(),
            pane_id: pane.clone(),
        })
        .await
        .expect_err("this client never attached");
    assert_eq!(error.code, ErrorCode::PaneNotAttached);

    // A one-pane session has nowhere to put the cursor if the pane goes.
    let single = session_of(
        ui.ask(Request::CreateSession {
            workspace_id: session.workspace_id.clone(),
            name: "one pane".to_string(),
            cwd: None,
            panes: None,
            note: None,
            tags: Vec::new(),
        })
        .await,
    );
    let only = details_of(
        ui.ask(Request::GetSession {
            session_id: single.id.clone(),
        })
        .await,
    )
    .layout
    .panes()[0]
        .id
        .clone();
    let error = ui
        .try_ask(Request::ClosePane {
            session_id: single.id.clone(),
            pane_id: only,
            disposition: CloseDisposition::Terminate,
        })
        .await
        .expect_err("the last pane cannot be closed");
    assert_eq!(error.code, ErrorCode::Conflict);

    // A node Turn does not hold cannot be written to, and the error says so rather than
    // pretending the keystroke landed.
    let error = ui
        .try_ask(Request::WritePty {
            session_id: session.id.clone(),
            node_id: turn_core::ids::NodeId::from_stored("proc_nothere"),
            data: turn_proto::TerminalBytes::new(b"x".to_vec()),
        })
        .await
        .expect_err("an unknown node");
    assert_eq!(error.code, ErrorCode::NotFound);

    daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_with_no_panes_asked_for_gets_one_working_shell() {
    let daemon = TestDaemon::start().await;
    let mut ui = daemon.connect().await;
    let workspace = workspace_of(
        ui.ask(Request::CreateWorkspace {
            name: "bare".to_string(),
            root: daemon.data_dir().display().to_string(),
        })
        .await,
    );
    let session = session_of(
        ui.ask(Request::CreateSession {
            workspace_id: workspace.id.clone(),
            name: "quick look".to_string(),
            cwd: None,
            panes: None,
            note: Some("no panes given".to_string()),
            tags: vec!["scratch".to_string()],
        })
        .await,
    );
    assert_eq!(session.pane_count, 1);
    assert_eq!(session.running_count, 1);

    let details = details_of(
        ui.ask(Request::GetSession {
            session_id: session.id.clone(),
        })
        .await,
    );
    let pane = details.layout.panes()[0].clone();
    assert_eq!(pane.kind, PaneKind::Shell);
    assert_eq!(
        pane.restore,
        RestoreBehaviour::Relaunch,
        "a shell is safe to bring back unprompted"
    );
    let pid = details.tree[0].pid.expect("a real shell");
    assert!(pid_is_alive(pid));

    daemon.shutdown().await;
}

/// The fallback tier for hierarchy: a child nothing reported, found in the process
/// table, and labelled as the guess it is.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_child_process_nothing_announced_is_adopted_as_an_inferred_link() {
    let daemon = TestDaemon::start_plain().await;
    let mut ui = daemon.connect().await;
    let workspace = workspace_of(
        ui.ask(Request::CreateWorkspace {
            name: "supervised".to_string(),
            root: daemon.data_dir().display().to_string(),
        })
        .await,
    );
    // A shell that starts something and waits. Nothing reports the child to Turn: this is
    // the case the process table exists for.
    let mut pane = NewPane::new(PaneKind::Terminal).with_command("sh");
    pane.args = vec!["-c".to_string(), "sleep 30 & wait".to_string()];
    let session = session_of(
        ui.ask(Request::CreateSession {
            workspace_id: workspace.id,
            name: "runs a server".to_string(),
            cwd: None,
            panes: Some(vec![pane]),
            note: None,
            tags: Vec::new(),
        })
        .await,
    );

    let nodes = ui
        .wait_for("the child to be adopted", |event| match event {
            ServerEvent::TreeChanged { session_id, nodes }
                if session_id == &session.id && nodes.len() >= 2 =>
            {
                Some(nodes.clone())
            }
            _ => None,
        })
        .await;

    let child = nodes
        .iter()
        .find(|node| node.depth == 1)
        .expect("the adopted child");
    assert_eq!(
        child.relationship.kind,
        turn_core::model::RelationshipKind::SpawnedBy,
        "a pid whose parent happens to match is not the same claim as a tool reporting it"
    );
    assert_eq!(
        child.relationship.confidence,
        turn_core::event::Confidence::InferredHigh
    );
    assert!(
        child.relationship_is_provisional,
        "the UI must be able to draw this edge differently"
    );
    assert_eq!(child.parent.as_ref(), Some(&nodes[0].node_id));
    assert!(child.command.contains("sleep"));
    assert!(pid_is_alive(child.pid.expect("an observed pid")));

    daemon.shutdown().await;
}
