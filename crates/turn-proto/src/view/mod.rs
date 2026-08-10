//! Serialisable projections of the domain, for a UI that only renders.
//!
//! The daemon owns every product rule. If the UI had to call
//! [`DisplayState::derive`](turn_core::state::DisplayState::derive) itself, or
//! decide whether a parent link is a guess, or work out which of thirty sessions
//! is shouting loudest, then those rules would exist twice — and the second copy
//! would be written in TypeScript by someone reading a screenshot. Every view
//! model here answers a question the UI would otherwise have to answer wrongly.
//!
//! Two rules the projections keep:
//!
//! * **Derive, never duplicate.** Anything already modelled in `turn-core` is
//!   embedded as that type ([`Lifecycle`](turn_core::state::Lifecycle),
//!   [`Turn`](turn_core::state::Turn), [`Layout`](turn_core::model::Layout),
//!   [`AttentionEntry`](turn_core::attention::AttentionEntry)). The extra fields
//!   are strictly *derived* values the UI would need a copy of the rules to
//!   compute.
//! * **Provisional stays visible.** A guessed parent link and an inferred state
//!   both carry their uncertainty into the view model, so the UI can render a
//!   guess as a guess instead of promoting it to a fact.

mod attention;
mod handoff;
mod hierarchy;
mod session;
mod settings;
mod tree;
mod workspace;

pub use attention::AttentionView;
pub use handoff::{ContextHandoffText, ContextHandoffView};
pub use hierarchy::{
    HierarchyKey, HierarchySnapshot, NodePaneCapability, NodePaneView, PaneFocusView,
    SessionTreeView, TreeSurfaceState, WorkspaceTreeView,
};
pub use session::{AgentSummary, SessionDetails, SessionSummary};
pub use settings::{SettingsEntry, SettingsLevel, SettingsView};
pub use tree::TreeNodeView;
pub use workspace::{TemplateSummary, WorkspaceSummary};
