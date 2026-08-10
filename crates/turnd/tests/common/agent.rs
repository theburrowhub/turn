//! An agent session with a callback URL, shared by the tests that need one.
//!
//! Inside `common/` so cargo treats it as part of the harness rather than as a test
//! binary of its own.

#![allow(dead_code)]

use super::*;
use turn_core::model::PaneKind;
use turn_core::state::DisplayState;
use turn_proto::{NewPane, Request, ServerEvent};

/// An agent session, with the callback URL its adapter was handed.
pub struct Agent {
    pub session: turn_core::ids::SessionId,
    /// The agent's own node, which is not the pane's process: the pane runs the user's
    /// shell and the agent runs inside it, so the agent is the shell's child.
    pub node: turn_core::ids::NodeId,
    /// The pane's process — the shell hosting the agent. This is what a keystroke goes
    /// to and what owns the screen.
    pub shell: turn_core::ids::NodeId,
    pub hook: String,
}

pub async fn agent_session(daemon: &TestDaemon, ui: &mut Client, name: &str) -> Agent {
    // Every fixture gets a distinct canonical checkout identity. Reusing the daemon
    // data root would correctly trip the production lease arbiter when attention
    // tests need more than one simultaneous writing Session.
    let root = daemon
        .data_dir()
        .join("workspaces")
        .join(turn_core::ids::WorkspaceId::new().as_str());
    std::fs::create_dir_all(&root).expect("agent test Workspace root");
    let workspace = workspace_of(
        ui.ask(Request::CreateWorkspace {
            name: format!("{name}-workspace"),
            root: root.display().to_string(),
        })
        .await,
    );
    let session = session_of(
        ui.ask(Request::CreateSession {
            workspace_id: workspace.id,
            name: name.to_string(),
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
    // The pane's process is the shell; the agent is what Turn started in it. The tree
    // is depth-first from the roots, so the shell is the first row and its agent is the
    // second — asserted rather than assumed, because getting these the wrong way round
    // would make every test that follows assert about the wrong process.
    let shell = details.tree[0].node_id.clone();
    assert!(
        !details.tree[0].is_agentic,
        "the pane's process is a shell: {:#?}",
        details.tree[0]
    );
    let agent = details
        .tree
        .iter()
        .find(|view| view.is_agentic)
        .unwrap_or_else(|| {
            panic!(
                "a structured adapter gives its node a turn axis: {:#?}",
                details.tree
            )
        });
    assert_eq!(
        agent.parent.as_ref(),
        Some(&shell),
        "the agent hangs off the shell Turn started it in"
    );
    let node = agent.node_id.clone();
    let hook = hook_url(daemon.data_dir(), &session.id, &node);
    Agent {
        session: session.id,
        node,
        shell,
        hook,
    }
}

/// Waits until Turn has identified an agent's own process, and answers with its pid.
///
/// An agent started in a pane's shell is forked by that shell, so Turn learns its pid
/// from the process table rather than from the launch — see
/// `supervise::identify_hosted_process`. Everything that addresses the process itself,
/// rather than the terminal it draws on, needs that to have happened.
pub async fn wait_for_agent_pid(
    ui: &mut Client,
    session: &turn_core::ids::SessionId,
    node: &turn_core::ids::NodeId,
) -> u32 {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let details = details_of(
            ui.ask(Request::GetSession {
                session_id: session.clone(),
            })
            .await,
        );
        let found = details
            .tree
            .iter()
            .find(|view| &view.node_id == node)
            .and_then(|view| view.pid);
        if let Some(pid) = found {
            return pid;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "Turn never identified {node}'s process: {:#?}",
            details.tree
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Waits for the session's own summary to reach a display state.
pub async fn wait_for_state(
    ui: &mut Client,
    session: &turn_core::ids::SessionId,
    want: DisplayState,
) {
    let got = ui
        .wait_for(
            &format!("the session to read as {want}"),
            |event| match event {
                ServerEvent::SessionStateChanged { session: summary }
                    if &summary.id == session && summary.display_state == want =>
                {
                    Some(summary.clone())
                }
                _ => None,
            },
        )
        .await;
    assert_eq!(got.display_state, want);
    assert_eq!(
        got.state_label,
        if want.demands_user() {
            "YOUR TURN"
        } else {
            want.label()
        }
    );
}
