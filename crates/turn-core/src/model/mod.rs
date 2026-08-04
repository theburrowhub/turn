//! Domain entities.
//!
//! The hierarchy is Workspace → Session → ProcessNode, with Layout/Pane
//! describing what the user sees and Template describing how to make more of it.

pub mod hierarchy;
pub mod layout;
pub mod node;
pub mod session;
pub mod template;
pub mod workspace;

pub use hierarchy::{
    ActivityPreview, AgentName, HierarchyNodeKind, LeaseMode, LeaseState, NameSource,
    PaneNodeBinding, PreviewSource, PreviewVisibility, Relationship, RelationshipKind, SessionMode,
    TreeUiState, WorkspaceCheckout, WorkspaceWriteLease,
};
pub use layout::{Child, Direction, Layout, LayoutNode, Pane, PaneKind, RestoreBehaviour, Split};
pub use node::{AgentInfo, NodeKind, PendingPermission, ProcessNode, Relation, SessionTree};
pub use session::{RestoreState, Session, SessionStatus};
pub use template::Template;
pub use workspace::Workspace;
