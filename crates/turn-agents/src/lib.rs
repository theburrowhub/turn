//! Agent adapters: turning heterogeneous tool signals into one event vocabulary.
//!
//! The crate is layered by how much a tool tells us about itself, and the layers
//! are not interchangeable — which is the whole point:
//!
//! * [`claude`] and [`codex`] are *structured*: the tool reports over a contract
//!   it owns, so their events are [`turn_core::Confidence::Explicit`].
//! * [`heuristic`] infers from terminal output and is capped at
//!   [`turn_core::Confidence::InferredHigh`] by [`turn_core::EventSource`],
//!   so a guess can badge a session but never move the user's focus.
//! * [`registry`] chooses between them for a given command line, and always
//!   answers — an unrecognised command runs as a plain terminal rather than being
//!   refused.
//! * [`server`] is the loopback endpoint the structured adapters point their tools
//!   at, with a per-node token so nothing else on the machine can forge events.
//! * [`text`] treats every string lifted out of a payload as hostile, because the
//!   contents of a hook callback are written by a model, not by the tool.
//!
//! See `docs/SECURITY.md` for the trust boundaries these modules sit on.

pub mod adapter;
pub mod claude;
pub mod codex;
pub mod context;
pub mod gemini;
pub mod heuristic;
pub mod opencode;
pub mod quota;
pub mod registry;
pub mod risk;
pub mod server;
pub mod text;

pub use adapter::{
    AdapterError, AgentAdapter, Capabilities, EventContext, HookEndpoint, IntegrationLevel,
    LaunchContext, LaunchPermissionPosture, LaunchPlan, LaunchProfileDefinition, LaunchProfileRole,
    ResolvedLaunchProfile, AUTONOMOUS_PROFILE_ID, SAFE_PROFILE_ID,
};
pub use claude::{ClaudeCodeAdapter, HookTransport};
pub use codex::{CodexAdapter, CodexTransport};
pub use context::{
    parse_context_tail, read_context_tail, ContextObservation, ContextTailRead, TranscriptFormat,
    MAX_CONTEXT_TAIL_BYTES,
};
pub use gemini::GeminiCliAdapter;
pub use heuristic::{HeuristicAdapter, HeuristicConfig, Inference, OutputHeuristic};
pub use opencode::OpenCodeAdapter;
pub use quota::{
    account_quota_source_for_tool, parse_codex_account_quota_response, probe_codex_account_quota,
    AccountCredits, AccountQuotaBucket, AccountQuotaObservation, AccountQuotaParseError,
    AccountQuotaSource, AccountQuotaWindow, AccountSpendControl, QuotaProbeError,
    MAX_ACCOUNT_QUOTA_MESSAGE_BYTES, MAX_ACCOUNT_QUOTA_PROBE_TIMEOUT,
    MIN_ACCOUNT_QUOTA_REFRESH_INTERVAL,
};
pub use registry::{AdapterLaunchCatalogue, AdapterRegistry, GenericTerminalAdapter, Selection};
pub use server::{HookServer, HookStats, ServerError};
