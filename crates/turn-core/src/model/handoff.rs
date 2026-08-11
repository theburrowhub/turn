//! User-visible intent for an Agent-to-Agent continuity handoff.

use serde::{Deserialize, Serialize};

/// What the destination Agent is being asked to do with a reviewed package.
///
/// This is intent, never authority: every mode still requires the destination to
/// verify the repository and carries no permission decision from the source.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextHandoffMode {
    #[default]
    ContinueWith,
    ReviewHandoff,
    SecondOpinion,
    PromoteToMain,
}

/// Durable metadata about the only two honest outcomes after a PTY write was
/// attempted. `Uncertain` is fenced against retry because a prefix may have
/// reached the destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextHandoffOutcome {
    Submitted,
    Uncertain,
}

impl ContextHandoffMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::ContinueWith => "Continue with",
            Self::ReviewHandoff => "Review handoff",
            Self::SecondOpinion => "Ask for second opinion",
            Self::PromoteToMain => "Promote to main",
        }
    }

    pub fn destination_instruction(self) -> &'static str {
        match self {
            Self::ContinueWith => {
                "Continue the work from the verified repository state and the reviewed context below."
            }
            Self::ReviewHandoff => {
                "Review this handoff for correctness, omissions and risk before making changes."
            }
            Self::SecondOpinion => {
                "Give an independent second opinion. Do not modify the workspace unless the user asks."
            }
            Self::PromoteToMain => {
                "Verify branch, HEAD, diff and tests, then prepare promotion to main through the repository's normal protections."
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_handoff_mode_names_a_distinct_user_intent() {
        let modes = [
            ContextHandoffMode::ContinueWith,
            ContextHandoffMode::ReviewHandoff,
            ContextHandoffMode::SecondOpinion,
            ContextHandoffMode::PromoteToMain,
        ];
        let labels: std::collections::HashSet<_> =
            modes.into_iter().map(ContextHandoffMode::label).collect();
        assert_eq!(labels.len(), modes.len());
        assert!(ContextHandoffMode::PromoteToMain
            .destination_instruction()
            .contains("normal protections"));
    }
}
