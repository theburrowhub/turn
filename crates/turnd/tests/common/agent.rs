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
    pub node: turn_core::ids::NodeId,
    pub hook: String,
}

pub async fn agent_session(daemon: &TestDaemon, ui: &mut Client, name: &str) -> Agent {
    let workspace = workspace_of(
        ui.ask(Request::CreateWorkspace {
            name: format!("{name}-workspace"),
            root: daemon.data_dir().display().to_string(),
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
    let node = details.tree[0].node_id.clone();
    assert!(
        details.tree[0].is_agentic,
        "a structured adapter gives the node a turn axis"
    );
    let hook = hook_url(daemon.data_dir(), &session.id, &node);
    Agent {
        session: session.id,
        node,
        hook,
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
    assert_eq!(got.state_label, want.label());
}
