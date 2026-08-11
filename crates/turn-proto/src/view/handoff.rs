//! A bounded, reviewable context transfer between two Agents.
//!
//! This is deliberately a *draft*, not a transcript. The daemon composes it from
//! stable/redacted semantic activity and the user reviews the exact body before a
//! second request writes it to the destination PTY.

use serde::{Deserialize, Serialize};
use std::fmt;
use turn_core::ids::{HandoffId, NodeId, SessionId};
pub use turn_core::model::ContextHandoffMode;

/// Sensitive free text carried over the local protocol without exposing it through
/// derived `Debug` output, tracing fields or assertion diagnostics.
#[derive(Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ContextHandoffText(String);

impl ContextHandoffText {
    pub fn new(text: impl Into<String>) -> Self {
        Self(text.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for ContextHandoffText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ContextHandoffText(<{} bytes>)", self.0.len())
    }
}

/// The exact payload Turn proposes to type into a destination Agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ContextHandoffView {
    /// Opaque, one-shot capability for delivering this exact daemon-held draft.
    pub handoff_id: HandoffId,
    pub session_id: SessionId,
    pub source_node_id: NodeId,
    pub target_node_id: NodeId,
    /// The exact user-selected purpose represented in the reviewed body.
    #[serde(default)]
    pub mode: ContextHandoffMode,
    pub source_label: String,
    pub target_label: String,
    /// The complete, already sanitised and redacted text awaiting confirmation.
    pub body: ContextHandoffText,
    /// Number of stable activity facts represented in `body`.
    pub preview_count: usize,
    /// Number of prior metadata-only handoffs represented in `body`.
    #[serde(default)]
    pub history_count: usize,
    /// True when branch/HEAD/status/diff were read from the real checkout.
    #[serde(default)]
    pub repository_included: bool,
    /// True when at least one secret-shaped value was replaced.
    pub redacted: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sensitive_handoff_text_is_opaque_in_debug_output() {
        let secret = "context-that-must-not-appear";
        let text = ContextHandoffText::new(secret);
        let debug = format!("{text:?}");
        assert!(!debug.contains(secret));
        assert!(debug.contains(&secret.len().to_string()));
    }
}
