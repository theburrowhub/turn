//! The desk: workspaces, sessions, panes, real processes and their output.

mod common;

use common::*;
use std::path::Path;
use std::process::Command as SystemCommand;
use turn_core::model::{PaneKind, RestoreBehaviour, SessionMode, Template};
use turn_proto::{
    CloseDisposition, ErrorCode, FocusTarget, NewPane, ProtoErrorContext, PtySize, Request,
    Response, ServerEvent,
};

/// Persists the richer Coding shape as an explicitly user-owned test fixture.
///
/// Turn ships only the portable Two Shells preset. Tests that exercise agents,
/// optional TUIs or a three-Pane shape must opt into those commands instead of
/// quietly depending on Coding being a built-in.
async fn save_custom_coding_template(ui: &mut Client) -> turn_proto::TemplateSummary {
    let shipped = match ui.ask(Request::ListTemplates).await {
        Response::Templates { templates } => templates,
        other => panic!("expected the shipped templates, got {other:?}"),
    };
    let built_ins: Vec<_> = shipped
        .iter()
        .filter(|template| template.built_in)
        .collect();
    assert_eq!(built_ins.len(), 1, "Turn ships one portable preset");
    assert_eq!(built_ins[0].name, "Two Shells");
    assert_eq!(built_ins[0].pane_count, 2);
    assert!(
        built_ins[0].commands.is_empty(),
        "the shipped preset must not assume optional executables"
    );

    let mut coding = Template::coding(0);
    coding.built_in = false;
    let template = match ui
        .ask(Request::CreateLayoutTemplate {
            name: coding.name,
            layout: Box::new(coding.layout),
            description: coding.description,
        })
        .await
    {
        Response::Template { template } => template,
        other => panic!("expected the custom Coding template, got {other:?}"),
    };
    assert!(
        !template.built_in,
        "test fixtures must never become built-ins"
    );
    template
}

/// Creates a workspace and a session from an explicit custom Coding fixture.
async fn custom_coding_session(daemon: &TestDaemon, ui: &mut Client) -> turn_proto::SessionSummary {
    let workspace = workspace_of(
        ui.ask(Request::CreateWorkspace {
            name: "turn".to_string(),
            root: daemon.data_dir().display().to_string(),
        })
        .await,
    );
    let coding = save_custom_coding_template(ui).await;

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
async fn a_session_from_a_custom_coding_template_has_real_processes_behind_its_panes() {
    let daemon = TestDaemon::start().await;
    let mut ui = daemon.connect().await;
    let session = custom_coding_session(&daemon, &mut ui).await;

    assert_eq!(session.name, "Fix the flaky test");
    assert_eq!(
        session.pane_count, 3,
        "the Coding template is an agent, a shell and Fang"
    );

    let details = details_of(
        ui.ask(Request::GetSession {
            session_id: session.id.clone(),
        })
        .await,
    );
    let panes = details.layout.panes();
    assert_eq!(panes.len(), 3);

    // The navigation tree is not duplicated into the layout. Claude and the shell
    // always have processes; Fang does too when it is installed on the test machine.
    let with_process: Vec<_> = panes.iter().filter(|pane| pane.node_id.is_some()).collect();
    assert!(with_process.len() >= 2, "panes with a process: {panes:#?}");
    let files_pane = panes
        .iter()
        .find(|pane| pane.kind == PaneKind::Tui)
        .expect("the Fang pane");
    assert_eq!(files_pane.command.as_deref(), Some("fang"));
    assert!(
        panes.iter().all(|pane| pane.kind != PaneKind::AgentTree),
        "the persistent left tree must never be duplicated as a Pane"
    );

    // Not "the daemon says it is running": the kernel says so. Every pane's own process
    // has a pid from the moment it is spawned.
    assert!(details.tree.len() >= 2);
    for node in details.tree.iter().filter(|node| !node.is_agentic) {
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
    // The agent's process was forked by its pane's shell, so Turn learns its pid from
    // the process table rather than from the launch. It is never claimed before it is
    // known, and once it is known the kernel agrees with it too.
    let agent_pid = common::agent::wait_for_agent_pid(&mut ui, &session.id, &agent.node_id).await;
    assert!(pid_is_alive(agent_pid));
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
async fn template_lease_conflict_alternatives_keep_a_custom_layout_authoritative_and_isolated() {
    let daemon = TestDaemon::start().await;
    let repository = daemon.data_dir().join("template-conflict-repository");
    std::fs::create_dir_all(repository.join("project")).unwrap();
    let run_git = |args: &[&str]| {
        let output = SystemCommand::new("git")
            .arg("-C")
            .arg(&repository)
            .args(args)
            .output()
            .expect("Git must run");
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    run_git(&["init"]);
    run_git(&["config", "user.email", "turn@example.invalid"]);
    run_git(&["config", "user.name", "Turn Test"]);
    std::fs::write(repository.join("README.md"), "turn\n").unwrap();
    std::fs::write(repository.join("project/.keep"), "tracked\n").unwrap();
    run_git(&["add", "README.md", "project/.keep"]);
    run_git(&["commit", "-m", "initial"]);

    let mut ui = daemon.connect().await;
    let workspace = workspace_of(
        ui.ask(Request::CreateWorkspace {
            name: "turn".into(),
            root: repository.to_string_lossy().into_owned(),
        })
        .await,
    );
    let coding = save_custom_coding_template(&mut ui).await;
    let writer = session_of(
        ui.ask(Request::CreateSession {
            workspace_id: workspace.id.clone(),
            name: "Primary writer".into(),
            cwd: None,
            panes: Some(vec![NewPane::new(PaneKind::AgentTree)]),
            note: None,
            tags: Vec::new(),
        })
        .await,
    );
    let requested_cwd = repository.join("project").to_string_lossy().into_owned();

    for name in ["Read-only Coding", "Isolated Coding"] {
        let error = ui
            .try_ask(Request::CreateSessionFromTemplate {
                workspace_id: workspace.id.clone(),
                template_id: coding.id.clone(),
                name: Some(name.into()),
                cwd: Some(requested_cwd.clone()),
                branch: None,
                task: Some("keep the complete template".into()),
            })
            .await
            .expect_err("the primary writer must keep its lease");
        assert_eq!(error.code, ErrorCode::Conflict);
        assert!(matches!(
            error.context.as_deref(),
            Some(ProtoErrorContext::WorkspaceWriteLeaseConflict { .. })
        ));
    }

    let read_only = session_of(
        ui.ask(Request::CreateReadOnlySessionFromTemplate {
            workspace_id: workspace.id.clone(),
            template_id: coding.id.clone(),
            name: Some("Read-only Coding".into()),
            cwd: Some(requested_cwd.clone()),
            branch: None,
            task: Some("keep the complete template".into()),
        })
        .await,
    );
    let read_only = details_of(
        ui.ask(Request::GetSession {
            session_id: read_only.id,
        })
        .await,
    );
    assert_eq!(read_only.summary.name, "Read-only Coding");
    assert_eq!(read_only.summary.mode, SessionMode::ReadOnly);
    assert_eq!(read_only.summary.template_id.as_ref(), Some(&coding.id));
    assert_eq!(
        Path::new(&read_only.summary.cwd),
        std::fs::canonicalize(&requested_cwd).unwrap()
    );
    assert!(read_only.tree.is_empty(), "read-only commands stay guarded");

    let isolated = session_of(
        ui.ask(Request::CreateWorktreeSessionFromTemplate {
            workspace_id: workspace.id.clone(),
            template_id: coding.id.clone(),
            name: Some("Isolated Coding".into()),
            cwd: Some(requested_cwd),
            template_branch: None,
            task: Some("keep the complete template".into()),
            branch: "turn/isolated-coding".into(),
            worktree_path: None,
        })
        .await,
    );
    let isolated = details_of(
        ui.ask(Request::GetSession {
            session_id: isolated.id,
        })
        .await,
    );
    assert_eq!(isolated.summary.name, "Isolated Coding");
    assert_eq!(isolated.summary.mode, SessionMode::IsolatedWorktree);
    assert_eq!(isolated.summary.template_id.as_ref(), Some(&coding.id));
    let worktree = isolated
        .summary
        .worktree_path
        .as_ref()
        .expect("the daemon records its isolated checkout");
    assert_ne!(Path::new(worktree), repository.as_path());
    assert!(isolated.summary.cwd.starts_with(worktree));
    assert!(isolated.summary.cwd.ends_with("project"));
    assert!(
        isolated.tree.len() >= 2,
        "Coding must start its agent and shell in the isolated checkout: {:#?}",
        isolated.tree
    );
    assert!(isolated
        .tree
        .iter()
        .all(|node| node.cwd.starts_with(worktree)));

    let pane_shape = |details: &turn_proto::SessionDetails| {
        details
            .layout
            .panes()
            .into_iter()
            .map(|pane| {
                (
                    pane.kind,
                    pane.title.clone(),
                    pane.command.clone(),
                    pane.args.clone(),
                    pane.env.clone(),
                    pane.restore,
                )
            })
            .collect::<Vec<_>>()
    };
    let expected = vec![
        (
            PaneKind::Agent,
            Some("claude".into()),
            Some("claude".into()),
            Vec::new(),
            Vec::new(),
            RestoreBehaviour::ReattachOnly,
        ),
        (
            PaneKind::Shell,
            Some("shell".into()),
            None,
            Vec::new(),
            Vec::new(),
            RestoreBehaviour::Relaunch,
        ),
        (
            PaneKind::Tui,
            Some("fang (files)".into()),
            Some("fang".into()),
            Vec::new(),
            Vec::new(),
            RestoreBehaviour::Relaunch,
        ),
    ];
    assert_eq!(pane_shape(&read_only), expected);
    assert_eq!(pane_shape(&isolated), expected);

    let lease = match ui
        .ask(Request::GetWorkspaceWriteLease {
            workspace_id: workspace.id,
        })
        .await
    {
        Response::WorkspaceWriteLease {
            lease: Some(lease), ..
        } => lease,
        other => panic!("expected the original lease, got {other:?}"),
    };
    assert_eq!(lease.session_id, writer.id);
    let status = SystemCommand::new("git")
        .arg("-C")
        .arg(&repository)
        .args(["status", "--porcelain"])
        .output()
        .unwrap();
    assert!(status.status.success());
    assert!(
        status.stdout.is_empty(),
        "the primary checkout was modified: {}",
        String::from_utf8_lossy(&status.stdout)
    );

    daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn writing_to_an_attached_pane_produces_the_processs_output() {
    let daemon = TestDaemon::start().await;
    let mut ui = daemon.connect().await;
    let session = custom_coding_session(&daemon, &mut ui).await;

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
    let session = custom_coding_session(&daemon, &mut first).await;

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
    let session = custom_coding_session(&daemon, &mut ui).await;

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

    // Reusing a Layout does not waive checkout exclusivity: the owning run has to be finished
    // before another main-checkout Session can be made from the same Template. The original
    // details above remain available for the shape comparison.
    ui.ask(Request::CloseSession {
        session_id: session.id.clone(),
        disposition: CloseDisposition::Terminate,
    })
    .await;
    // Ending it is the whole of finishing it. It used to leave the write lease held, so the
    // user had to find "Release write lease" in a menu before they could start work in their
    // own Workspace again — a second step for a Session that had already stopped and left the
    // tree, and one nothing was writing through.
    let lease = match ui
        .ask(Request::GetWorkspaceWriteLease {
            workspace_id: session.workspace_id.clone(),
        })
        .await
    {
        Response::WorkspaceWriteLease { lease, .. } => lease,
        other => panic!("expected a lease answer, got {other:?}"),
    };
    assert!(
        lease.is_none(),
        "ending a Session lets go of the checkout it was holding: {lease:?}"
    );

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
    let session = custom_coding_session(&daemon, &mut ui).await;

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
    let session = custom_coding_session(&daemon, &mut ui).await;
    let details = details_of(
        ui.ask(Request::GetSession {
            session_id: session.id.clone(),
        })
        .await,
    );
    let pids: Vec<u32> = details.tree.iter().filter_map(|node| node.pid).collect();
    assert!(
        pids.len() >= 2,
        "Claude and the shell must run; Fang also runs when installed"
    );

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
    // Generous: this waits on the operating system reaping every process, which a loaded
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
    // Archived, not paused: ending takes the row out of the tree. Leaving it listed as though
    // it were still work in progress is what made the verb look like it did nothing.
    assert_eq!(
        after.summary.status,
        turn_core::model::SessionStatus::Archived
    );

    daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_daemon_refuses_the_things_it_should_and_says_why() {
    let daemon = TestDaemon::start().await;
    let mut ui = daemon.connect().await;
    let session = custom_coding_session(&daemon, &mut ui).await;
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
    // This check needs another Layout, not another writer. Use the explicit read-only
    // alternative so the fixture cannot normalize the pre-upgrade multi-writer model.
    let single = session_of(
        ui.ask(Request::CreateReadOnlySession {
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

/// A process sets its own pane title through a real pty, and the daemon reports it.
///
/// This is the reproducible test the issue asks for: no fixture, no injected event —
/// a shell emits `ESC ] 2 ; … BEL` and the title arrives in the tree projection.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_process_sets_its_own_pane_title_through_a_real_pty() {
    let daemon = TestDaemon::start().await;
    let mut ui = daemon.connect().await;
    let session = custom_coding_session(&daemon, &mut ui).await;

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

    // Attach first: the title is noticed when output arrives, and attaching is what
    // makes the pump run.
    ui.attach_cells(&session.id, &shell.id, PtySize::new(30, 100))
        .await;

    ui.ask(Request::WritePty {
        session_id: session.id.clone(),
        node_id: node.clone(),
        data: turn_proto::TerminalBytes::new(
            // `sleep` after the title, on one line: a shell rewrites its title from
            // its prompt, so a test that returns to the prompt races its own set-up.
            // An agent mid-task does not return to a prompt either.
            b"printf '\\033]2;fixing the climbing bug\\007'; sleep 10\n".to_vec(),
        ),
    })
    .await;

    let title = ui
        .wait_for_node_title(&session.id, &node, "fixing the climbing bug")
        .await;
    assert_eq!(title.title, "fixing the climbing bug");
    assert!(
        title.title_is_provisional,
        "a title the process printed is the program's word about itself, \
         and must never be presented with the authority of a reported name"
    );

    daemon.shutdown().await;
}

/// Two shells in one session keep separate titles, and neither disturbs the other.
///
/// The isolation is structural — each pty has its own buffer — but a test is what
/// keeps a future change from routing titles through anything shared.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_processes_in_one_session_keep_independent_titles() {
    let daemon = TestDaemon::start().await;
    let mut ui = daemon.connect().await;
    let workspace = workspace_of(
        ui.ask(Request::CreateWorkspace {
            name: "turn".to_string(),
            root: daemon.data_dir().display().to_string(),
        })
        .await,
    );

    // Two shells of its own: the shipped templates carry one, and an agent pane is
    // named by its adapter, which deliberately outranks a process title.
    let session = session_of(
        ui.ask(Request::CreateSession {
            workspace_id: workspace.id,
            name: "two titled shells".into(),
            cwd: None,
            panes: Some(vec![
                NewPane::new(PaneKind::Shell).with_command("sh"),
                NewPane::new(PaneKind::Shell).with_command("sh"),
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
        .into_iter()
        .filter(|pane| pane.node_id.is_some())
        .cloned()
        .collect();
    assert_eq!(panes.len(), 2, "two shells were asked for");

    for (index, pane) in panes.iter().enumerate() {
        let node = pane.node_id.clone().unwrap();
        ui.attach_cells(&session.id, &pane.id, PtySize::new(24, 80))
            .await;
        ui.ask(Request::WritePty {
            session_id: session.id.clone(),
            node_id: node,
            data: turn_proto::TerminalBytes::new(
                format!("printf '\\033]2;instance {index}\\007'; sleep 10\n").into_bytes(),
            ),
        })
        .await;
    }

    for (index, pane) in panes.iter().enumerate() {
        let node = pane.node_id.clone().unwrap();
        let expected = format!("instance {index}");
        let view = ui.wait_for_node_title(&session.id, &node, &expected).await;
        assert_eq!(view.title, expected, "titles must not bleed between panes");
        assert!(view.title_is_provisional);
    }

    daemon.shutdown().await;
}

/// A title never opens a pane, moves focus, changes the layout or raises attention.
///
/// The issue asks for this explicitly, and it is the difference between a label and
/// an event: a shell rewriting its title on every prompt must not be able to pull the
/// user anywhere.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_title_change_moves_nothing_and_raises_nothing() {
    let daemon = TestDaemon::start().await;
    let mut ui = daemon.connect().await;
    let session = custom_coding_session(&daemon, &mut ui).await;

    let before = details_of(
        ui.ask(Request::GetSession {
            session_id: session.id.clone(),
        })
        .await,
    );
    let shell = before
        .layout
        .panes()
        .into_iter()
        .find(|pane| pane.kind == PaneKind::Shell)
        .cloned()
        .expect("the shell pane");
    let node = shell.node_id.clone().expect("the shell has a process");
    let panes_before = before.layout.panes().len();

    ui.attach_cells(&session.id, &shell.id, PtySize::new(24, 80))
        .await;
    ui.ask(Request::WritePty {
        session_id: session.id.clone(),
        node_id: node.clone(),
        data: turn_proto::TerminalBytes::new(b"printf '\\033]2;busy\\007'; sleep 10\n".to_vec()),
    })
    .await;
    ui.wait_for_node_title(&session.id, &node, "busy").await;

    let after = details_of(
        ui.ask(Request::GetSession {
            session_id: session.id.clone(),
        })
        .await,
    );
    assert_eq!(
        after.layout.panes().len(),
        panes_before,
        "a title opened or closed a pane"
    );
    assert_eq!(
        after.layout.active, before.layout.active,
        "a title moved the focused pane"
    );
    assert!(
        !after.summary.needs_user,
        "a title raised an attention demand"
    );
    assert_eq!(after.summary.badge_count, 0, "a title produced a badge");

    match ui
        .ask(Request::ListAttention {
            session_id: Some(session.id.clone()),
        })
        .await
    {
        Response::AttentionList { entries } => {
            assert!(entries.is_empty(), "a title queued something: {entries:?}");
        }
        other => panic!("expected the attention list, got {other:?}"),
    }

    daemon.shutdown().await;
}

/// The third verb the tree had no way to reach: getting rid of a Session for good.
///
/// Archiving hides it and stops nothing. Closing stops it and keeps the record — the Session
/// comes back as `Paused`, and the next time archived rows are shown it is on screen again.
/// Neither answers "I am done with this, take it away", which is what this checks: the
/// processes stop, the record goes, the tree no longer lists it, and asking again is not an
/// error. And the checkout it pointed at is still on disk, because that is the user's, not
/// Turn's.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deleting_a_session_stops_its_work_and_removes_every_trace_of_it() {
    let daemon = TestDaemon::start().await;
    let mut ui = daemon.connect().await;
    let session = custom_coding_session(&daemon, &mut ui).await;
    let details = details_of(
        ui.ask(Request::GetSession {
            session_id: session.id.clone(),
        })
        .await,
    );
    let pids: Vec<u32> = details.tree.iter().filter_map(|node| node.pid).collect();
    assert!(!pids.is_empty(), "the session must have real processes");
    let checkout = daemon.data_dir().to_path_buf();
    assert!(checkout.exists(), "the workspace root exists to begin with");

    // Keeping the processes is refused, and the refusal says what to do instead: nothing
    // would name those processes once the Session is gone.
    let refusal = ui
        .try_ask(Request::DeleteSession {
            session_id: session.id.clone(),
            disposition: CloseDisposition::KeepProcesses,
        })
        .await;
    let error = refusal.expect_err("deleting while keeping the processes must be refused");
    assert_eq!(error.code, turn_proto::ErrorCode::Refused, "{error:?}");

    ui.ask(Request::DeleteSession {
        session_id: session.id.clone(),
        disposition: CloseDisposition::Terminate,
    })
    .await;

    // The work stopped. Generous, because this waits on the operating system reaping.
    let mut stopped = false;
    for _ in 0..200 {
        if pids.iter().all(|pid| !pid_is_alive(*pid)) {
            stopped = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(stopped, "deleting must actually stop the processes");

    // The record is gone rather than parked. `close_session` leaves a Paused Session here.
    let gone = ui
        .try_ask(Request::GetSession {
            session_id: session.id.clone(),
        })
        .await;
    let error = gone.expect_err("the Session must no longer exist");
    assert_eq!(error.code, turn_proto::ErrorCode::NotFound, "{error:?}");

    // And it is not in the tree, including with archived rows shown — which is where a
    // Session that had merely been archived would reappear.
    let hierarchy = match ui
        .ask(Request::GetHierarchy {
            surface_id: "window-1".to_string(),
            include_archived: true,
        })
        .await
    {
        Response::Hierarchy { snapshot } => *snapshot,
        other => panic!("expected the tree, got {other:?}"),
    };
    let listed: Vec<&str> = hierarchy
        .workspaces
        .iter()
        .flat_map(|workspace| &workspace.sessions)
        .map(|session| session.session.name.as_str())
        .collect();
    assert!(
        listed.is_empty(),
        "the deleted Session is still in the tree: {listed:?}"
    );

    // Asking again is not an error, so a client that lost the reply can retry.
    ui.ask(Request::DeleteSession {
        session_id: session.id.clone(),
        disposition: CloseDisposition::Terminate,
    })
    .await;

    // Turn deleted its own record. The directory the Workspace pointed at is untouched.
    assert!(
        checkout.exists(),
        "deleting a Session must not remove anything from the user's disk"
    );

    daemon.shutdown().await;
}

/// A Workspace goes the same way, and takes its Sessions with it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deleting_a_workspace_takes_its_sessions_and_leaves_the_checkout_alone() {
    let daemon = TestDaemon::start().await;
    let mut ui = daemon.connect().await;
    let session = custom_coding_session(&daemon, &mut ui).await;
    let details = details_of(
        ui.ask(Request::GetSession {
            session_id: session.id.clone(),
        })
        .await,
    );
    let pids: Vec<u32> = details.tree.iter().filter_map(|node| node.pid).collect();
    let workspace_id = session.workspace_id.clone();
    let checkout = daemon.data_dir().to_path_buf();

    ui.ask(Request::DeleteWorkspace {
        workspace_id: workspace_id.clone(),
        disposition: CloseDisposition::Terminate,
    })
    .await;

    let mut stopped = false;
    for _ in 0..200 {
        if pids.iter().all(|pid| !pid_is_alive(*pid)) {
            stopped = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(stopped, "the Sessions' processes must be stopped");

    let hierarchy = match ui
        .ask(Request::GetHierarchy {
            surface_id: "window-1".to_string(),
            include_archived: true,
        })
        .await
    {
        Response::Hierarchy { snapshot } => *snapshot,
        other => panic!("expected the tree, got {other:?}"),
    };
    assert!(
        hierarchy.workspaces.is_empty(),
        "the Workspace and its Sessions must be gone: {:?}",
        hierarchy
            .workspaces
            .iter()
            .map(|w| w.workspace.name.as_str())
            .collect::<Vec<_>>()
    );
    assert!(
        checkout.exists(),
        "the checkout is the user's directory and must survive"
    );

    daemon.shutdown().await;
}

/// The tree as one surface sees it, with or without archived rows.
async fn hierarchy_now(ui: &mut Client, include_archived: bool) -> turn_proto::HierarchySnapshot {
    match ui
        .ask(Request::GetHierarchy {
            surface_id: "window-1".to_string(),
            include_archived,
        })
        .await
    {
        Response::Hierarchy { snapshot } => *snapshot,
        other => panic!("expected the tree, got {other:?}"),
    }
}

/// The reported defect, in the words it was reported in: the Sessions were "there laughing at
/// me every time I end them".
///
/// Ending a Session stopped its processes and left the row in the tree as `Paused` — stopped,
/// but still listed exactly like work in progress. So the verb looked like it had done nothing,
/// and the tree filled up with rows the user had already finished with. Ending something has to
/// look like ending it.
///
/// What this checks is the whole of that: the row leaves the tree, it is *archived* rather than
/// deleted so it can be brought back, and the same holds one level up for a Workspace.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ending_a_session_takes_its_row_out_of_the_tree() {
    let daemon = TestDaemon::start().await;
    let mut ui = daemon.connect().await;
    let session = custom_coding_session(&daemon, &mut ui).await;
    let workspace_id = session.workspace_id.clone();

    let listed = |snapshot: &turn_proto::HierarchySnapshot| -> Vec<String> {
        snapshot
            .workspaces
            .iter()
            .flat_map(|workspace| &workspace.sessions)
            .map(|session| session.session.name.clone())
            .collect()
    };

    assert_eq!(
        listed(&hierarchy_now(&mut ui, false).await),
        vec!["Fix the flaky test".to_string()],
        "the Session is in the tree while it is being worked on"
    );

    ui.ask(Request::CloseSession {
        session_id: session.id.clone(),
        disposition: CloseDisposition::Terminate,
    })
    .await;

    // The row is gone from the tree the user is looking at.
    assert!(
        listed(&hierarchy_now(&mut ui, false).await).is_empty(),
        "an ended Session must not still be listed: {:?}",
        listed(&hierarchy_now(&mut ui, false).await)
    );
    // And it is archived rather than deleted, so it can be brought back — turning archived rows
    // on shows it, stopped.
    let with_archived = hierarchy_now(&mut ui, true).await;
    assert_eq!(
        listed(&with_archived),
        vec!["Fix the flaky test".to_string()],
        "ending is reversible: the row is archived, not forgotten"
    );
    let ended = with_archived
        .workspaces
        .iter()
        .flat_map(|workspace| &workspace.sessions)
        .next()
        .expect("the archived Session");
    assert_eq!(
        ended.session.running_count, 0,
        "and nothing is running in it"
    );

    // One level up the answer is deliberately different, and the difference is the point.
    //
    // Stopping every Session in a Workspace takes every *Session* row out of the tree and leaves
    // the Workspace's own row where it is. A Session is a task: finishing it means it is over. A
    // Workspace is a project — a directory the user comes back to — and filing the project away
    // because its last task stopped would mean restoring it before starting the next one. Which
    // is why the control is called "Stop all sessions" and not "Close workspace": getting the
    // project itself out of the tree is Archive, or Delete.
    // The template the first Session was made from is already saved; a second Session of the
    // same shape reuses it rather than saving it twice.
    let template = match ui.ask(Request::ListTemplates).await {
        Response::Templates { templates } => templates
            .into_iter()
            .find(|template| !template.built_in)
            .expect("the custom template saved at the start"),
        other => panic!("expected templates, got {other:?}"),
    };
    session_of(
        ui.ask(Request::CreateSessionFromTemplate {
            workspace_id: workspace_id.clone(),
            template_id: template.id.clone(),
            name: Some("Another task".to_string()),
            cwd: None,
            branch: None,
            task: None,
        })
        .await,
    );
    assert_eq!(
        listed(&hierarchy_now(&mut ui, false).await),
        vec!["Another task".to_string()],
        "the ended Session stays out of the tree and the new one is in it"
    );

    ui.ask(Request::CloseWorkspace {
        workspace_id: workspace_id.clone(),
        disposition: CloseDisposition::Terminate,
    })
    .await;
    let after = hierarchy_now(&mut ui, false).await;
    assert!(
        listed(&after).is_empty(),
        "every Session in the Workspace has ended, so no Session row is left: {:?}",
        listed(&after)
    );
    assert_eq!(
        after.workspaces.len(),
        1,
        "and the Workspace is still there, because stopping its work is not closing the project"
    );

    daemon.shutdown().await;
}
