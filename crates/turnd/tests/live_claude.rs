//! Opt-in acceptance against an authenticated, installed Claude Code and the daemon
//! shipped inside a local `Turn.app`.
//!
//! This is ignored in normal CI because it consumes a real account and needs a
//! foreground macOS application. See `docs/REVIEWER_ACCEPTANCE.md` for the exact
//! invocation and the required close/reopen boundary.

#![cfg(target_os = "macos")]

mod common;

use common::*;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use turn_core::event::Confidence;
use turn_core::model::{LeaseState, NodeKind, PaneKind, RelationshipKind, SessionMode};
use turn_core::state::Turn;
use turn_proto::{
    CloseDisposition, HierarchyKey, NewPane, PtySize, Request, Response, SessionDetails,
    TerminalBytes,
};

const LIVE_WAIT: Duration = Duration::from_secs(240);
const CLAUDE_STARTUP_WAIT: Duration = Duration::from_secs(45);
const CLAUDE_PERMISSION_MODE: &str = "default";
const SURFACE: &str = "live-claude-reviewer-acceptance";
const TRUST_PROMPT_HEADING: &str =
    "quick safety check: is this a project you created or one you trust?";
const TRUST_PROMPT_CONFIRM: &str = "yes, i trust this folder";

fn required_path(name: &str) -> PathBuf {
    let value = std::env::var_os(name)
        .unwrap_or_else(|| panic!("{name} must name an absolute path for live acceptance"));
    let path = PathBuf::from(value);
    assert!(
        path.is_absolute(),
        "{name} must be absolute: {}",
        path.display()
    );
    path
}

fn claude_version(binary: &Path) -> String {
    let output = Command::new(binary)
        .arg("--version")
        .output()
        .unwrap_or_else(|error| panic!("could not execute {}: {error}", binary.display()));
    assert!(
        output.status.success(),
        "Claude --version failed: {output:?}"
    );
    String::from_utf8(output.stdout)
        .expect("Claude --version is UTF-8")
        .trim()
        .to_string()
}

fn has_claude_editor_markers(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    text.contains("claude code v") && text.lines().any(|line| line.trim() == "❯")
}

fn claude_editor_is_ready(text: &str, claude_is_running: bool) -> bool {
    claude_is_running && has_claude_editor_markers(text)
}

#[test]
fn editor_readiness_requires_a_live_claude_banner_and_empty_prompt() {
    let editor = "Claude Code v2.1.226\n\n  ❯  \n";
    assert!(claude_editor_is_ready(editor, true));
    assert!(!claude_editor_is_ready(editor, false));
    assert!(!claude_editor_is_ready("Claude Code v2.1.226\n", true));
    assert!(!claude_editor_is_ready("shell\n❯\n", true));
}

async fn session_details(
    ui: &mut Client,
    session_id: &turn_core::ids::SessionId,
) -> SessionDetails {
    details_of(
        ui.ask(Request::GetSession {
            session_id: session_id.clone(),
        })
        .await,
    )
}

async fn wait_for_claude_editor(
    ui: &mut Client,
    session_id: &turn_core::ids::SessionId,
    runtime_node_id: &turn_core::ids::NodeId,
    claude_node_id: &turn_core::ids::NodeId,
    pane_id: &turn_core::ids::PaneId,
) {
    let deadline = tokio::time::Instant::now() + CLAUDE_STARTUP_WAIT;
    let mut accepted_trust = false;
    loop {
        ui.poll_screens().await;
        let (text, alternate_screen, bracketed_paste) = {
            let screen = ui.screen(session_id, pane_id);
            (
                screen.text().to_ascii_lowercase(),
                screen.alternate_screen,
                screen.modes.bracketed_paste,
            )
        };
        let trust_prompt =
            text.contains(TRUST_PROMPT_HEADING) && text.contains(TRUST_PROMPT_CONFIRM);
        // Screen modes are acceptance evidence, not the readiness signal: Claude
        // 2.1.226's initial editor paint can expose neither alternate screen nor
        // bracketed paste. The product banner and empty input prompt together
        // identify the editor without mistaking the hosting shell for it.
        let has_editor_markers = has_claude_editor_markers(&text);
        if trust_prompt {
            if !accepted_trust {
                ui.ask(Request::WritePty {
                    session_id: session_id.clone(),
                    node_id: runtime_node_id.clone(),
                    data: TerminalBytes::new(b"\r".to_vec()),
                })
                .await;
                accepted_trust = true;
            }
        } else if has_editor_markers {
            let details = session_details(ui, session_id).await;
            let claude = details
                .tree
                .iter()
                .find(|node| &node.node_id == claude_node_id)
                .expect("Claude's semantic node remains present at editor readiness");
            assert!(
                claude_editor_is_ready(&text, claude.lifecycle.is_running()),
                "Claude exited before editor readiness; the visible banner and prompt are residual terminal content"
            );
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "Claude did not reach its interactive editor within {CLAUDE_STARTUP_WAIT:?}; alternate_screen={alternate_screen}; bracketed_paste={bracketed_paste}; screen={text:?}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn wait_for_reviewer(
    ui: &mut Client,
    session_id: &turn_core::ids::SessionId,
) -> SessionDetails {
    let deadline = tokio::time::Instant::now() + LIVE_WAIT;
    loop {
        let details = session_details(ui, session_id).await;
        if details.tree.iter().any(|node| {
            node.kind == NodeKind::Subagent
                && node
                    .agent
                    .as_ref()
                    .is_some_and(|agent| agent.name.declared_name.as_deref() == Some("Reviewer"))
                && node.activity_preview.is_some()
        }) {
            return details;
        }
        if let Some(reason) = details.tree.iter().find_map(|node| match &node.turn {
            Some(Turn::Failed { reason }) => Some(reason),
            _ => None,
        }) {
            panic!("Claude failed before declaring Reviewer: {reason}");
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "Claude did not report a named Reviewer with a preview within {LIVE_WAIT:?}; tree={:#?}",
            details.tree
        );
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

fn write_evidence(path: &Path, value: &serde_json::Value) {
    std::fs::write(
        path,
        serde_json::to_vec_pretty(value).expect("evidence serialises"),
    )
    .unwrap_or_else(|error| panic!("could not write {}: {error}", path.display()));
}

/// Runs the paid/authenticated portion of the vertical against the daemon already
/// launched by the packaged app. The test leaves Claude and the daemon alive so the
/// caller can close and reopen only `Turn.app`, then run the restoration test below.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires TURN_LIVE_CLAUDE=1, an authenticated Claude Code, and a packaged Turn.app"]
async fn packaged_app_runs_authenticated_claude_reviewer_vertical() {
    assert_eq!(
        std::env::var("TURN_LIVE_CLAUDE").as_deref(),
        Ok("1"),
        "live account use must be explicit"
    );
    let socket = required_path("TURN_LIVE_CLAUDE_SOCKET");
    let project = std::fs::canonicalize(required_path("TURN_LIVE_CLAUDE_PROJECT"))
        .expect("the isolated live project must exist");
    let binary = required_path("TURN_LIVE_CLAUDE_BIN");
    assert!(
        std::fs::metadata(&binary).is_ok_and(|metadata| metadata.is_file()),
        "the Claude binary must resolve to a file: {}",
        binary.display()
    );
    let evidence_path = required_path("TURN_LIVE_CLAUDE_EVIDENCE");
    let debug_path = required_path("TURN_LIVE_CLAUDE_DEBUG");
    let version = claude_version(&binary);

    let mut ui = Client::connect(&socket).await;
    let workspace = workspace_of(
        ui.ask(Request::CreateWorkspace {
            name: format!("Claude Reviewer acceptance ({version})"),
            root: project.display().to_string(),
        })
        .await,
    );

    let mut pane = NewPane::new(PaneKind::Agent);
    pane.command = Some(binary.display().to_string());
    pane.args = vec![
        "--permission-mode".into(),
        CLAUDE_PERMISSION_MODE.into(),
        "--model".into(),
        "sonnet".into(),
        "--no-chrome".into(),
        "--setting-sources".into(),
        "project,local".into(),
        "--debug=hooks".into(),
        "--debug-file".into(),
        debug_path.display().to_string(),
    ];
    pane.cwd = Some(project.display().to_string());
    pane.env
        .push(("CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS".into(), "1".into()));

    let session = session_of(
        ui.ask(Request::CreateSession {
            workspace_id: workspace.id.clone(),
            name: "Live Claude → Reviewer".into(),
            cwd: None,
            panes: Some(vec![pane]),
            note: Some(format!("Authenticated acceptance with {version}")),
            tags: vec!["live-acceptance".into(), "claude-reviewer".into()],
        })
        .await,
    );
    assert_eq!(session.mode, SessionMode::MainCheckout);

    let lease = match ui
        .ask(Request::GetWorkspaceWriteLease {
            workspace_id: workspace.id.clone(),
        })
        .await
    {
        Response::WorkspaceWriteLease {
            lease: Some(lease), ..
        } => lease,
        other => panic!("expected an active write lease, got {other:?}"),
    };
    assert_eq!(lease.session_id, session.id);
    assert_eq!(lease.state, LeaseState::Active);

    let before = session_details(&mut ui, &session.id).await;
    let layout_before = before.layout.clone();
    let parent = before
        .tree
        .iter()
        .find(|node| node.kind == NodeKind::Agent)
        .expect("Claude is represented as an Agent")
        .clone();
    let pane = &layout_before.panes()[0];
    let pane_id = pane.id.clone();
    let runtime_node = pane
        .node_id
        .clone()
        .expect("the Pane is bound to its hosting shell");
    let parent_node = parent.node_id.clone();

    ui.attach_cells(&session.id, &pane_id, PtySize::new(40, 120))
        .await;
    wait_for_claude_editor(&mut ui, &session.id, &runtime_node, &parent_node, &pane_id).await;

    ui.ask(Request::ResizePty {
        session_id: session.id.clone(),
        node_id: runtime_node.clone(),
        size: PtySize::new(44, 132),
    })
    .await;
    tokio::time::sleep(Duration::from_millis(250)).await;
    ui.poll_screens().await;

    let prompt = "In this isolated repository, create an agent team and spawn one in-process teammate named Reviewer. Ask Reviewer to inspect README.md and report one concise observation. Do not edit files and do not open any terminal, pane, window, or external application. Wait for Reviewer to report back.";
    ui.ask(Request::WritePty {
        session_id: session.id.clone(),
        node_id: runtime_node.clone(),
        data: TerminalBytes::new(prompt.as_bytes().to_vec()),
    })
    .await;
    // Keep text insertion and submit as separate PTY writes, matching the two GUI
    // actions without making delivery depend on when Claude enables bracketed paste.
    tokio::time::sleep(Duration::from_millis(150)).await;
    ui.ask(Request::WritePty {
        session_id: session.id.clone(),
        node_id: runtime_node,
        data: TerminalBytes::new(b"\r".to_vec()),
    })
    .await;

    let details = wait_for_reviewer(&mut ui, &session.id).await;
    ui.poll_screens().await;
    let screen = ui.screen(&session.id, &pane_id).clone();
    let reviewer = details
        .tree
        .iter()
        .find(|node| {
            node.kind == NodeKind::Subagent
                && node
                    .agent
                    .as_ref()
                    .is_some_and(|agent| agent.name.declared_name.as_deref() == Some("Reviewer"))
        })
        .expect("Reviewer is a background subagent");
    assert_eq!(reviewer.parent.as_ref(), Some(&parent_node));
    assert_eq!(reviewer.relationship.kind, RelationshipKind::SpawnedBy);
    assert_eq!(reviewer.relationship.confidence, Confidence::Explicit);
    assert!(!reviewer.relationship_is_provisional);
    assert_eq!(
        reviewer.pid, None,
        "an in-process teammate has no invented pid"
    );
    assert!(reviewer.pane_bindings.is_empty());
    assert_eq!(
        details.layout, layout_before,
        "Reviewer cannot mutate layout"
    );

    let reviewer_key = HierarchyKey::process(reviewer.node_id.clone());
    let parent_key = HierarchyKey::process(parent_node.clone());
    ui.ask(Request::GetHierarchy {
        surface_id: SURFACE.into(),
        include_archived: false,
    })
    .await;
    ui.ask(Request::SetTreeExpanded {
        surface_id: SURFACE.into(),
        key: parent_key,
        expanded: true,
    })
    .await;
    ui.ask(Request::SelectTreeNode {
        surface_id: SURFACE.into(),
        selected: Some(reviewer_key),
    })
    .await;

    let preview = match ui
        .ask(Request::GetPreviewHistory {
            session_id: session.id.clone(),
            node_id: reviewer.node_id.clone(),
            limit: Some(5),
        })
        .await
    {
        Response::PreviewHistory { entries, .. } if !entries.is_empty() => entries,
        other => panic!("Quick Preview has no Reviewer history: {other:?}"),
    };

    let temporary = match ui
        .ask(Request::OpenNodeAsTemporaryPane {
            surface_id: SURFACE.into(),
            session_id: session.id.clone(),
            node_id: reviewer.node_id.clone(),
        })
        .await
    {
        Response::NodePane { pane } => pane,
        other => panic!("expected a temporary Reviewer pane, got {other:?}"),
    };
    assert!(temporary.binding.temporary);
    assert_eq!(
        session_details(&mut ui, &session.id).await.layout,
        layout_before
    );
    ui.ask(Request::ClosePane {
        session_id: session.id.clone(),
        pane_id: temporary.binding.pane_id,
        disposition: CloseDisposition::KeepProcesses,
    })
    .await;
    let after_close = session_details(&mut ui, &session.id).await;
    let reviewer_after = after_close
        .tree
        .iter()
        .find(|node| node.node_id == reviewer.node_id)
        .expect("closing the temporary pane keeps Reviewer");
    assert_eq!(after_close.layout, layout_before);
    assert!(reviewer_after.lifecycle.is_running());
    assert!(reviewer_after.pane_bindings.is_empty());

    write_evidence(
        &evidence_path,
        &serde_json::json!({
            "claude_version": version,
            "claude_permission_mode": CLAUDE_PERMISSION_MODE,
            "daemon_version": ui.welcome.daemon_version,
            "protocol_version": ui.welcome.protocol_version,
            "workspace_root": project,
            "session": {
                "id": session.id.to_string(),
                "mode": format!("{:?}", session.mode),
                "lease_state": format!("{:?}", lease.state),
                "layout_panes": layout_before.pane_count(),
            },
            "terminal": {
                "rows": screen.rows,
                "cols": screen.cols,
                "alternate_screen": screen.alternate_screen,
                "bracketed_paste": screen.modes.bracketed_paste,
                "mouse_mode": format!("{:?}", screen.modes.mouse),
                "styled_cells": screen.cells.iter().filter(|cell| cell.fg.is_some() || cell.bg.is_some()).count(),
            },
            "reviewer": {
                "node_id": reviewer.node_id,
                "parent_node_id": parent_node,
                "declared_name": reviewer.agent.as_ref().and_then(|agent| agent.name.declared_name.clone()),
                "relationship": format!("{:?}", reviewer.relationship.kind),
                "confidence": format!("{:?}", reviewer.relationship.confidence),
                "preview": preview[0].normalized_text,
                "pid": reviewer.pid,
                "pane_bindings": reviewer.pane_bindings.len(),
                "lifecycle_after_temporary_close": format!("{:?}", reviewer_after.lifecycle),
            }
        }),
    );
}

/// Run after terminating and reopening only the packaged GUI. The original daemon
/// and Claude process must still own the socket and PTY.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "run after the packaged Turn.app has been closed and reopened"]
async fn reopened_packaged_app_restores_live_claude_reviewer_vertical() {
    assert_eq!(std::env::var("TURN_LIVE_CLAUDE").as_deref(), Ok("1"));
    let socket = required_path("TURN_LIVE_CLAUDE_SOCKET");
    let evidence_path = required_path("TURN_LIVE_CLAUDE_EVIDENCE");
    let evidence: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&evidence_path)
            .unwrap_or_else(|error| panic!("could not read {}: {error}", evidence_path.display())),
    )
    .expect("live evidence is valid JSON");
    let expected_session_id = evidence["session"]["id"]
        .as_str()
        .expect("live evidence names the exact Session");
    let mut ui = Client::connect(&socket).await;
    let snapshot = match ui
        .ask(Request::GetHierarchy {
            surface_id: SURFACE.into(),
            include_archived: false,
        })
        .await
    {
        Response::Hierarchy { snapshot } => *snapshot,
        other => panic!("expected a restored hierarchy, got {other:?}"),
    };
    let branch = snapshot
        .workspaces
        .iter()
        .flat_map(|workspace| &workspace.sessions)
        .find(|session| session.session.id.to_string() == expected_session_id)
        .expect("the live acceptance Session survives the UI restart");
    let parent = branch
        .nodes
        .iter()
        .find(|node| node.kind == NodeKind::Agent)
        .expect("Claude remains in the tree");
    let reviewer = branch
        .nodes
        .iter()
        .find(|node| {
            node.kind == NodeKind::Subagent
                && node
                    .agent
                    .as_ref()
                    .is_some_and(|agent| agent.name.declared_name.as_deref() == Some("Reviewer"))
        })
        .expect("Reviewer remains in the tree");
    assert!(parent.lifecycle.is_running());
    assert_eq!(reviewer.parent.as_ref(), Some(&parent.node_id));
    assert_eq!(reviewer.relationship.kind, RelationshipKind::SpawnedBy);
    assert_eq!(reviewer.relationship.confidence, Confidence::Explicit);
    assert_eq!(
        reviewer
            .agent
            .as_ref()
            .and_then(|agent| agent.name.declared_name.as_deref()),
        Some("Reviewer")
    );
    assert!(reviewer.activity_preview.is_some());
    assert!(reviewer.pane_bindings.is_empty());
    let session_id = branch.session.id.clone();
    assert_eq!(
        session_details(&mut ui, &session_id)
            .await
            .layout
            .pane_count(),
        1
    );

    let workspace = snapshot
        .workspaces
        .iter()
        .find(|workspace| workspace.workspace.id == branch.session.workspace_id)
        .expect("the owning Workspace");
    let lease = workspace
        .write_lease
        .as_ref()
        .expect("the live write lease");
    assert_eq!(lease.session_id, session_id);
    assert_eq!(lease.state, LeaseState::Active);
}
