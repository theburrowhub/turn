//! Contextual, on-demand detail for one row of the unified hierarchy.
//!
//! Inspector payloads are deliberately separate from [`HierarchySnapshot`]: logs,
//! configuration and handoff history are useful only while the optional panel is
//! open. Keeping them out of every tree refresh makes inspecting one Process cheap
//! without making a 100-Process Workspace expensive forever.

use serde::{Deserialize, Serialize};
use turn_core::attention::AttentionPolicy;
use turn_core::event::{Confidence, Severity};
use turn_core::ids::NodeId;
use turn_core::model::{Relationship, WorkspaceCheckout, WorkspaceWriteLease};

use super::{HierarchyKey, SessionSummary, TreeNodeView, WorkspaceSummary};

/// One safe, bounded event-log row. Raw adapter/PTY payloads never enter this view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct InspectorEventView {
    pub timestamp_ms: i64,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub source: String,
    pub confidence: Confidence,
    pub severity: Severity,
}

/// A readable parent link and the exact tree row it leads to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct InspectorParentView {
    pub key: HierarchyKey,
    pub name: String,
    pub relationship: Relationship,
    /// Explicit even though confidence also travels: clients must never silently
    /// promote an inferred edge when choosing colours or accessible wording.
    pub provisional: bool,
}

/// How Turn learned about a Process. Confidence qualifies the claim, not its row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct InspectorOriginView {
    pub label: String,
    pub confidence: Confidence,
}

/// Metadata-only continuity history. Handoff bodies remain ephemeral.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct InspectorHandoffView {
    pub timestamp_ms: i64,
    pub direction: String,
    pub peer_node_id: NodeId,
    pub peer_name: String,
    pub mode: String,
    pub outcome: String,
}

/// The optional right panel for any selected hierarchy row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InspectorDetails {
    Workspace {
        workspace: Box<WorkspaceSummary>,
        checkouts: Vec<WorkspaceCheckout>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        write_lease: Option<WorkspaceWriteLease>,
        /// Names only. Values can contain credentials and have no diagnostic value here.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        environment_keys: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        init_commands: Vec<String>,
        attention: AttentionPolicy,
    },
    Session {
        workspace_name: String,
        session: Box<SessionSummary>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        checkout: Option<WorkspaceCheckout>,
        attention: AttentionPolicy,
        /// Names only, for the same reason as the Workspace projection.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        environment_keys: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        history: Vec<InspectorEventView>,
    },
    Agent {
        session_name: String,
        node: Box<TreeNodeView>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent: Option<InspectorParentView>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        process_group: Option<u32>,
        origin: InspectorOriginView,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        history: Vec<InspectorEventView>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        handoffs: Vec<InspectorHandoffView>,
    },
    Process {
        session_name: String,
        node: Box<TreeNodeView>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent: Option<InspectorParentView>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        process_group: Option<u32>,
        origin: InspectorOriginView,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        history: Vec<InspectorEventView>,
    },
}

impl InspectorDetails {
    /// Stable identity used to reject a late response after selection changed.
    pub fn key(&self) -> HierarchyKey {
        match self {
            Self::Workspace { workspace, .. } => HierarchyKey::workspace(workspace.id.clone()),
            Self::Session { session, .. } => HierarchyKey::session(session.id.clone()),
            Self::Agent { node, .. } | Self::Process { node, .. } => {
                HierarchyKey::process(node.node_id.clone())
            }
        }
    }
}
