//! Domain entities.
//!
//! The hierarchy is Workspace → Session → ProcessNode, with Layout/Pane
//! describing what the user sees and Template describing how to make more of it.

pub mod agent_runtime;
pub mod handoff;
pub mod hierarchy;
pub mod layout;
pub mod node;
pub mod session;
pub mod template;
pub mod workspace;

pub use agent_runtime::{
    AgentLaunchFacts, AgentRuntimeMetadata, ContextTokenUsage, ContextUsageSnapshot,
    LaunchConfiguration, Observable, ObservationSource, ObservationSourceKind, QuotaSnapshot,
    QuotaWindow, UsageMeasurement, UsageMeasurementKind, UsageUnit,
};
pub use handoff::{ContextHandoffMode, ContextHandoffOutcome};
pub use hierarchy::{
    ActivityPreview, AgentName, HierarchyNodeKind, LeaseMode, LeaseState, NameSource,
    PaneNodeBinding, PreviewSource, PreviewVisibility, Relationship, RelationshipKind, SessionMode,
    TreeFilter, TreeSurfacePreferences, TreeUiState, TreeVisibilityMode, WorkspaceCheckout,
    WorkspaceWriteLease,
};
pub use layout::{
    AgentLaunchProfileRef, Child, Direction, DropZone, FloatingPane, Layout, LayoutNode,
    LayoutPreset, Pane, PaneGeometry, PaneKind, PanePlacement, RestoreBehaviour, Split,
};
pub use node::{
    AgentIdentityAlias, AgentIdentitySource, AgentInfo, NodeKind, PendingPermission, ProcessNode,
    Relation, SessionTree,
};
pub use session::{RestoreState, Session, SessionStatus};
pub use template::Template;
pub use workspace::Workspace;
