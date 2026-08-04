//! Attention coordination: the product's reason for existing.
//!
//! Agents run in parallel; this module decides when that parallelism is allowed
//! to reach the user. It is split so each piece has one job:
//!
//! * [`policy`] — what a session wants to happen, per kind of moment.
//! * [`queue`] — ordering simultaneous demands into one obvious next item.
//! * [`focus`] — the guards no policy may bypass.
//! * [`manager`] — the seam that turns events into effects.

pub mod focus;
pub mod manager;
pub mod policy;
pub mod queue;

pub use focus::{DeferReason, FocusDecision, FocusDenial, FocusGovernor, UserContext};
pub use manager::{AttentionManager, Effect};
pub use policy::{Action, AttentionPolicy, Sound, Trigger};
pub use queue::{AttentionEntry, AttentionQueue, EntryState};
