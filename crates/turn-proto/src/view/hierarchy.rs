//! The one navigation projection: Workspace -> Session -> ProcessNode.
//!
//! This is a full, revisioned snapshot rather than a patch language. A client
//! that misses a revision asks for [`Request::GetHierarchy`](crate::Request::GetHierarchy)
//! again; it never guesses how to repair a parent edge, a checkout lease or a
//! Pane binding. Tree interaction is scoped to `surface_id`, so two windows may
//! expand and select independently without changing domain state.

use serde::{Deserialize, Serialize};
use turn_core::ids::{NodeId, PaneId, SessionId, WorkspaceId};
use turn_core::model::{PaneNodeBinding, WorkspaceCheckout, WorkspaceWriteLease};

use crate::screen::PaneStream;

use super::{SessionSummary, TreeNodeView, WorkspaceSummary};

/// A stable key for every item that may appear in the unified tree.
///
/// The tagged shape keeps ids strongly typed on both sides of the boundary. A
/// plain string plus a separate kind is too easy to combine incorrectly when a
/// client restores its per-window selection.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HierarchyKey {
    Workspace { workspace_id: WorkspaceId },
    Session { session_id: SessionId },
    Process { node_id: NodeId },
}

impl HierarchyKey {
    pub fn workspace(workspace_id: WorkspaceId) -> Self {
        Self::Workspace { workspace_id }
    }

    pub fn session(session_id: SessionId) -> Self {
        Self::Session { session_id }
    }

    pub fn process(node_id: NodeId) -> Self {
        Self::Process { node_id }
    }
}

/// Persisted interaction state for one window/surface.
///
/// This is deliberately separate from the snapshot's hierarchy records:
/// selecting a Process does not focus a Pane, resolve Attention, or mutate the
/// Process. `expanded` is a complete replacement, not a set of deltas.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct TreeSurfaceState {
    pub surface_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected: Option<HierarchyKey>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expanded: Vec<HierarchyKey>,
}

impl TreeSurfaceState {
    pub fn empty(surface_id: impl Into<String>) -> Self {
        Self {
            surface_id: surface_id.into(),
            selected: None,
            expanded: Vec::new(),
        }
    }
}

/// One Session and all of its runtime children in draw order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SessionTreeView {
    pub session: SessionSummary,
    /// Roots followed by their descendants, with each row carrying its depth.
    pub nodes: Vec<TreeNodeView>,
}

/// One Workspace branch of the unified hierarchy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct WorkspaceTreeView {
    pub workspace: WorkspaceSummary,
    /// Every known primary/worktree checkout, including declared shared
    /// resources the UI must surface before creating concurrent writers.
    pub checkouts: Vec<WorkspaceCheckout>,
    /// The current primary-checkout writer. `None` means no active lease; it does
    /// not prove that legacy state is safe when reconciliation is required.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub write_lease: Option<WorkspaceWriteLease>,
    pub sessions: Vec<SessionTreeView>,
}

/// The complete navigation truth for one UI surface at one revision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HierarchySnapshot {
    /// Monotonic for hierarchy-visible daemon state. A gap requires a full
    /// `get_hierarchy` resync rather than application of a partial guess.
    pub revision: u64,
    pub tree_state: TreeSurfaceState,
    pub workspaces: Vec<WorkspaceTreeView>,
}

impl HierarchySnapshot {
    pub fn empty(surface_id: impl Into<String>, revision: u64) -> Self {
        let surface_id = surface_id.into();
        Self {
            revision,
            tree_state: TreeSurfaceState::empty(surface_id),
            workspaces: Vec::new(),
        }
    }
}

/// What an explicit "open this node" action can render.
///
/// A semantic-only subagent is `PreviewDetails`; the protocol must not invent a
/// terminal for it. `Terminal` means the integration has a real attachable
/// stream and lists the representations it can serve.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NodePaneCapability {
    /// Safe baseline when no integration can vouch for an attachable stream.
    #[default]
    PreviewDetails,
    Terminal {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        streams: Vec<PaneStream>,
    },
}

/// The daemon-authoritative result of explicitly opening a node as a Pane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct NodePaneView {
    pub binding: PaneNodeBinding,
    pub capability: NodePaneCapability,
}

/// The exact existing Pane selected by `focus_pane_for_node`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PaneFocusView {
    pub surface_id: String,
    pub session_id: SessionId,
    pub node_id: NodeId,
    pub pane_id: PaneId,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use turn_core::event::Confidence;
    use turn_core::ids::CheckoutId;
    use turn_core::model::{
        ActivityPreview, AgentName, Layout, NodeKind, Pane, PaneKind, PreviewSource, ProcessNode,
        Relation, Session, SessionMode, Workspace,
    };

    const T0: i64 = 1_700_000_000_000;

    #[test]
    fn hierarchy_keys_keep_entity_ids_strongly_typed_on_the_wire() {
        let key = HierarchyKey::process(NodeId::from_stored("proc_reviewer"));
        let json = serde_json::to_string(&key).unwrap();
        assert_eq!(json, "{\"kind\":\"process\",\"node_id\":\"proc_reviewer\"}");
        assert_eq!(serde_json::from_str::<HierarchyKey>(&json).unwrap(), key);
    }

    #[test]
    fn an_empty_snapshot_still_names_its_surface_and_revision() {
        let snapshot = HierarchySnapshot::empty("window-a", 41);
        assert_eq!(snapshot.tree_state.surface_id, "window-a");
        assert_eq!(snapshot.revision, 41);
        assert!(snapshot.workspaces.is_empty());
    }

    #[test]
    fn semantic_only_nodes_do_not_default_to_an_invented_terminal() {
        assert_eq!(
            NodePaneCapability::default(),
            NodePaneCapability::PreviewDetails
        );
    }

    #[test]
    fn a_full_snapshot_round_trips_checkout_name_relationship_preview_and_binding_facts() {
        let workspace = Workspace::new("turn", "/repo", T0);
        let checkout_id = CheckoutId::primary_for(&workspace.id);
        let mut session = Session::new(
            workspace.id.clone(),
            "Review auth",
            "/repo",
            Layout::single(Pane::new(PaneKind::Agent).with_command("claude")),
            T0,
        );
        session.mode = SessionMode::MainCheckout;
        session.checkout_id = checkout_id.clone();

        let root = session.tree.insert(ProcessNode::agent(
            session.id.clone(),
            "claude",
            "/repo",
            T0,
        ));
        let mut reviewer =
            ProcessNode::agent(session.id.clone(), "claude --subagent", "/repo", T0 + 1);
        reviewer.kind = NodeKind::Subagent;
        reviewer.link_to(root, Relation::Confirmed);
        reviewer.agent.as_mut().unwrap().name = AgentName::declared("Reviewer");
        reviewer.activity_preview = Some(ActivityPreview {
            node_id: reviewer.id.clone(),
            raw_source_sequence: Some(9),
            normalized_text: "Reviewing auth.rs".into(),
            source: PreviewSource::SemanticEvent,
            confidence: Confidence::Explicit,
            stable: true,
            contains_sensitive_data: true,
            redacted: true,
            updated_ms: T0 + 2,
        });
        let reviewer_id = session.tree.insert(reviewer);

        let binding = PaneNodeBinding {
            pane_id: PaneId::from_stored("pane_reviewer"),
            session_id: session.id.clone(),
            node_id: reviewer_id.clone(),
            temporary: true,
            surface_id: Some("window-a".into()),
            opened_ms: T0 + 3,
        };
        let nodes = TreeNodeView::for_session_with_panes(
            &session,
            std::slice::from_ref(&binding),
            &HashMap::new(),
            T0 + 4,
        );
        let session_summary = SessionSummary::from_session(&session, 0, false, T0 + 4);
        let workspace_summary =
            WorkspaceSummary::from_workspace(&workspace, std::slice::from_ref(&session_summary));
        let lease = WorkspaceWriteLease::active(
            workspace.id.clone(),
            session.id.clone(),
            checkout_id.clone(),
            T0,
        );
        let snapshot = HierarchySnapshot {
            revision: 12,
            tree_state: TreeSurfaceState {
                surface_id: "window-a".into(),
                selected: Some(HierarchyKey::process(reviewer_id)),
                expanded: vec![
                    HierarchyKey::workspace(workspace.id.clone()),
                    HierarchyKey::session(session.id.clone()),
                ],
            },
            workspaces: vec![WorkspaceTreeView {
                workspace: workspace_summary,
                checkouts: vec![WorkspaceCheckout {
                    id: checkout_id,
                    workspace_id: workspace.id,
                    path: "/repo".into(),
                    canonical_path: "/repo".into(),
                    branch: Some("main".into()),
                    primary: true,
                    shared_resources: vec!["port:3000".into()],
                    created_ms: T0,
                }],
                write_lease: Some(lease),
                sessions: vec![SessionTreeView {
                    session: session_summary,
                    nodes,
                }],
            }],
        };

        let json = serde_json::to_string(&snapshot).unwrap();
        assert!(json.contains("\"mode\":\"main_checkout\""), "got {json}");
        assert!(json.contains("\"declared_name\":\"Reviewer\""));
        assert!(json.contains("\"kind\":\"spawned_by\""));
        assert!(json.contains("\"normalized_text\":\"Reviewing auth.rs\""));
        assert!(json.contains("\"temporary\":true"));
        assert!(json.contains("\"pane_capability\":{\"kind\":\"preview_details\"}"));
        assert!(json.contains("\"write_lease\""));
        assert_eq!(
            serde_json::from_str::<HierarchySnapshot>(&json).unwrap(),
            snapshot
        );
    }
}
