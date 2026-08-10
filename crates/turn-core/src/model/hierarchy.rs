//! Domain values introduced by the unified Workspace hierarchy.
//!
//! These are deliberately independent of GUI state. A write lease protects a
//! checkout, not focus; an activity preview describes a runtime node, not a pane.

use crate::event::Confidence;
use crate::ids::{CheckoutId, LeaseId, NodeId, PaneId, SessionId, WorkspaceId};
use serde::{Deserialize, Serialize};

/// How a Session may use its checkout.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionMode {
    /// The sole writer against the Workspace's primary checkout.
    #[default]
    MainCheckout,
    /// Review/research with a technical write guard where the platform permits it.
    ReadOnly,
    /// An independently rooted Git worktree and branch.
    IsolatedWorktree,
}

impl SessionMode {
    pub fn can_own_primary_lease(self) -> bool {
        matches!(self, Self::MainCheckout)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::MainCheckout => "MAIN",
            Self::ReadOnly => "READ ONLY",
            Self::IsolatedWorktree => "WORKTREE",
        }
    }
}

/// One checkout known to a Workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceCheckout {
    pub id: CheckoutId,
    pub workspace_id: WorkspaceId,
    pub path: String,
    /// Canonical filesystem identity used for global collision prevention.
    pub canonical_path: String,
    pub branch: Option<String>,
    pub primary: bool,
    /// Names of non-filesystem resources this checkout may collide on.
    pub shared_resources: Vec<String>,
    pub created_ms: i64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseMode {
    #[default]
    ExclusiveWrite,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseState {
    #[default]
    Active,
    Released,
    Stale,
    /// The previous owner may still be alive; only explicit reconciliation may
    /// leave this state.
    RecoveryRequired,
}

/// Daemon-owned exclusivity for a Workspace's primary checkout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceWriteLease {
    pub id: LeaseId,
    pub workspace_id: WorkspaceId,
    pub session_id: SessionId,
    pub checkout_id: CheckoutId,
    pub mode: LeaseMode,
    pub state: LeaseState,
    pub acquired_ms: i64,
    pub heartbeat_ms: i64,
    pub released_ms: Option<i64>,
    /// Monotonic fencing generation for helpers that can enforce it.
    pub generation: u64,
}

impl WorkspaceWriteLease {
    pub fn active(
        workspace_id: WorkspaceId,
        session_id: SessionId,
        checkout_id: CheckoutId,
        now_ms: i64,
    ) -> Self {
        Self {
            id: LeaseId::new(),
            workspace_id,
            session_id,
            checkout_id,
            mode: LeaseMode::ExclusiveWrite,
            state: LeaseState::Active,
            acquired_ms: now_ms,
            heartbeat_ms: now_ms,
            released_ms: None,
            generation: 1,
        }
    }

    pub fn release(&mut self, now_ms: i64) {
        self.state = LeaseState::Released;
        self.heartbeat_ms = now_ms;
        self.released_ms = Some(now_ms);
    }
}

/// What a relationship means; certainty is stored separately.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationshipKind {
    SpawnedBy,
    OwnsProcess,
    Related,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Relationship {
    pub kind: RelationshipKind,
    pub confidence: Confidence,
}

impl Default for Relationship {
    fn default() -> Self {
        Self {
            kind: RelationshipKind::Unknown,
            confidence: Confidence::Unknown,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NameSource {
    ExplicitParentEvent,
    Integration,
    StructuredTask,
    ProcessTitle,
    Inferred,
    #[default]
    Fallback,
}

impl NameSource {
    /// How much authority this source has. Lower wins.
    ///
    /// The order is the one the product cares about: what a tool told us about
    /// itself outranks what we read off its terminal, because a title is free text
    /// the process writes and a hook payload is a documented contract. Written as
    /// a number rather than relying on the declaration order so that adding a
    /// variant cannot silently re-rank the existing ones.
    pub fn rank(&self) -> u8 {
        match self {
            NameSource::ExplicitParentEvent => 0,
            NameSource::Integration => 1,
            NameSource::StructuredTask => 2,
            NameSource::ProcessTitle => 3,
            NameSource::Inferred => 4,
            NameSource::Fallback => 5,
        }
    }

    /// Whether a name from this source outranks one from `other`.
    pub fn outranks(&self, other: NameSource) -> bool {
        self.rank() < other.rank()
    }

    /// Whether the UI should mark a name from this source as something Turn read
    /// rather than was told.
    ///
    /// A process title is the interesting case. It cannot be sanitised into
    /// trustworthiness: `✓ tests passed` or the name of another of the user's
    /// sessions are both perfectly valid text. The defence is not filtering, it is
    /// never presenting such a name with the same authority as one a tool reported
    /// through a contract.
    pub fn is_provisional(&self) -> bool {
        matches!(
            self,
            NameSource::ProcessTitle | NameSource::Inferred | NameSource::Fallback
        )
    }
}

/// Lossless agent naming: a user rename never destroys what the parent declared.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentName {
    pub declared_name: Option<String>,
    pub display_name: String,
    pub source: NameSource,
    pub confidence: Confidence,
    pub user_renamed: bool,
}

impl Default for AgentName {
    fn default() -> Self {
        Self {
            declared_name: None,
            display_name: String::new(),
            source: NameSource::Fallback,
            confidence: Confidence::Unknown,
            user_renamed: false,
        }
    }
}

impl AgentName {
    pub fn declared(name: impl Into<String>) -> Self {
        let name = name.into();
        Self {
            declared_name: Some(name.clone()),
            display_name: name,
            source: NameSource::ExplicitParentEvent,
            confidence: Confidence::Explicit,
            user_renamed: false,
        }
    }

    pub fn fallback(name: impl Into<String>) -> Self {
        Self {
            display_name: name.into(),
            ..Self::default()
        }
    }

    pub fn rename(&mut self, name: impl Into<String>) {
        self.display_name = name.into();
        self.user_renamed = true;
    }

    /// Offers a title the process wrote about itself, and reports whether it was
    /// taken.
    ///
    /// Refused in two cases, both deliberate:
    ///
    /// * The user renamed this node. Their choice is the one thing nothing else
    ///   may override — a shell that rewrites its title on every prompt would
    ///   otherwise erase a name someone typed on purpose.
    /// * Something with more authority already named it. A `SubagentStart` hook
    ///   that said "Reviewer" is worth more than whatever the program prints, so a
    ///   later title cannot demote it.
    ///
    /// `declared_name` is never touched, so a title can never destroy what a
    /// parent reported even while it is being displayed.
    pub fn apply_process_title(&mut self, title: impl Into<String>) -> bool {
        if self.user_renamed {
            return false;
        }
        if self.source.outranks(NameSource::ProcessTitle) {
            return false;
        }
        let title = title.into();
        if title.is_empty() || self.display_name == title {
            return false;
        }
        self.display_name = title;
        self.source = NameSource::ProcessTitle;
        // A title is read, not reported. `InferredHigh` rather than `Explicit`:
        // the sequence really did arrive, but what it claims is the process's word
        // about itself and nothing corroborates it.
        self.confidence = Confidence::InferredHigh;
        true
    }

    /// Drops a name that came from a process title, falling back to whatever was
    /// declared or to `fallback`.
    ///
    /// Called when a process ends. Without it a dead process leaves its last words
    /// on screen — a header still announcing "compiling…" for something that
    /// exited ten minutes ago is worse than no title at all.
    pub fn clear_process_title(&mut self, fallback: impl Into<String>) -> bool {
        if self.source != NameSource::ProcessTitle {
            return false;
        }
        match &self.declared_name {
            Some(declared) => {
                self.display_name = declared.clone();
                self.source = NameSource::ExplicitParentEvent;
                self.confidence = Confidence::Explicit;
            }
            None => {
                self.display_name = fallback.into();
                self.source = NameSource::Fallback;
                self.confidence = Confidence::Unknown;
            }
        }
        true
    }

    /// Whether the displayed name should be rendered as provisional.
    pub fn is_provisional(&self) -> bool {
        !self.user_renamed && self.source.is_provisional()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewSource {
    SemanticEvent,
    AdapterState,
    RelevantAction,
    StableScreenLine,
    #[default]
    ProcessFallback,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewVisibility {
    #[default]
    Inherit,
    Show,
    Hide,
}

/// A compact, stable representation for navigation; never raw PTY bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityPreview {
    pub node_id: NodeId,
    pub raw_source_sequence: Option<u64>,
    pub normalized_text: String,
    pub source: PreviewSource,
    pub confidence: Confidence,
    pub stable: bool,
    pub contains_sensitive_data: bool,
    pub redacted: bool,
    pub updated_ms: i64,
}

/// Normalised one-to-many view binding. Process identity never points back to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneNodeBinding {
    pub pane_id: PaneId,
    pub session_id: SessionId,
    pub node_id: NodeId,
    pub temporary: bool,
    /// Temporary panes are owned by one UI surface; durable Layout panes use None.
    pub surface_id: Option<String>,
    pub opened_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HierarchyNodeKind {
    Workspace,
    Session,
    Process,
}

/// Per-window tree interaction. It is not broadcast and is not a TurnEvent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TreeUiState {
    pub surface_id: String,
    pub node_kind: HierarchyNodeKind,
    pub node_id: String,
    pub expanded: bool,
    pub selected: bool,
    pub manual_order: Option<i32>,
    pub visibility_mode: Option<String>,
    pub updated_ms: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_declared_name_survives_a_user_rename() {
        let mut name = AgentName::declared("code-reviewer");
        name.rename("Reviewer");
        assert_eq!(name.declared_name.as_deref(), Some("code-reviewer"));
        assert_eq!(name.display_name, "Reviewer");
        assert!(name.user_renamed);
    }

    #[test]
    fn releasing_a_lease_is_explicit_and_timestamped() {
        let mut lease = WorkspaceWriteLease::active(
            WorkspaceId::from_stored("ws_a"),
            SessionId::from_stored("sess_a"),
            CheckoutId::from_stored("checkout_a"),
            10,
        );
        lease.release(20);
        assert_eq!(lease.state, LeaseState::Released);
        assert_eq!(lease.released_ms, Some(20));
    }
}
