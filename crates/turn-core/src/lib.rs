//! # turn-core
//!
//! Turn's domain layer: entities, state model, event vocabulary and attention
//! coordination. It has no I/O, no pty, no database and no UI — which is why the
//! rules that matter can be tested exhaustively without spawning a process.
//!
//! The two ideas worth knowing before reading further:
//!
//! 1. **Process state and agent turn state are separate axes** ([`state`]).
//!    Collapsing them is what makes other tools report "done" while a test run
//!    is still going.
//! 2. **Every event carries a [`event::Confidence`]** and a heuristic can never
//!    claim to be a fact. Focus changes require an honest signal.

pub mod attention;
pub mod event;
pub mod ids;
pub mod model;
pub mod state;

pub use attention::{AttentionManager, AttentionPolicy, Effect, UserContext};
pub use event::{Confidence, EventKind, EventSource, Severity, TurnEvent};
pub use ids::{
    AttentionId, EventId, HandoffId, NodeId, PaneId, SessionId, TemplateId, WorkspaceId,
};
pub use model::{
    AgentInfo, Layout, LayoutNode, Pane, PaneKind, ProcessNode, Session, SessionTree, Split,
    Template, Workspace,
};
pub use state::{AwaitingReason, DisplayState, Lifecycle, Turn};

/// Wall-clock milliseconds since the Unix epoch.
///
/// Every function that needs the time takes it as a parameter instead of reading
/// the clock, so the attention rules are deterministic under test.
pub fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
