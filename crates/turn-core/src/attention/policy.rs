//! Per-session attention policy: what should happen when an agent needs you.

use crate::event::{Confidence, EventKind};
use serde::{Deserialize, Serialize};

/// The classes of moment a policy can react to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    TurnComplete,
    Question,
    PermissionRequired,
    TaskComplete,
    Failure,
    WaitingForUser,
    SubagentAppeared,
}

impl Trigger {
    /// Maps an event to the trigger it fires, if any.
    pub fn from_event(kind: &EventKind) -> Option<Trigger> {
        match kind {
            EventKind::AgentTurnCompleted { .. } => Some(Trigger::TurnComplete),
            EventKind::AgentQuestionAsked { .. } => Some(Trigger::Question),
            EventKind::AgentPermissionRequired { .. } => Some(Trigger::PermissionRequired),
            EventKind::AgentTaskCompleted { .. } => Some(Trigger::TaskComplete),
            EventKind::AgentFailed { .. } | EventKind::ProcessFailed { .. } => {
                Some(Trigger::Failure)
            }
            EventKind::AgentWaitingForUser { .. } | EventKind::SessionNeedsAttention { .. } => {
                Some(Trigger::WaitingForUser)
            }
            EventKind::AgentSubagentStarted { .. } => Some(Trigger::SubagentAppeared),
            _ => None,
        }
    }
}

/// What Turn may do in response. A policy maps a trigger to a set of these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// Explicitly do nothing. Present as a variant so "silence" is a recorded
    /// decision rather than an empty list that might mean "unconfigured".
    Nothing,
    /// Numeric or dot badge on the session in the sidebar.
    Badge,
    /// Tint the session row.
    Highlight,
    /// Play the configured sound.
    Sound,
    /// Post an OS notification.
    Notify,
    /// Add to the attention queue so `next-attention` will reach it.
    Enqueue,
    /// Switch the active session to this one.
    Focus,
    /// Switch only if the user is not currently typing.
    FocusIfIdle,
    /// Switch only if the Turn window is not frontmost.
    FocusIfBackground,
    /// Run the session's configured custom command.
    Custom,
}

impl Action {
    /// Whether this action moves the user's viewport.
    pub fn is_focus(&self) -> bool {
        matches!(
            self,
            Action::Focus | Action::FocusIfIdle | Action::FocusIfBackground
        )
    }
}

/// Sound choice. Kept coarse on purpose; the point is subtlety, not a soundboard.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sound {
    #[default]
    None,
    Subtle,
    Alert,
}

/// A session's attention configuration.
///
/// Defaults are deliberately quiet: badge on turn completion, focus only for
/// things that actually block the agent, and never interrupt mid-keystroke.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttentionPolicy {
    pub on_turn_complete: Vec<Action>,
    pub on_question: Vec<Action>,
    pub on_permission_required: Vec<Action>,
    pub on_task_complete: Vec<Action>,
    pub on_failure: Vec<Action>,
    pub on_waiting_for_user: Vec<Action>,
    pub on_subagent_appeared: Vec<Action>,

    /// Never move focus while the user is typing, regardless of the actions above.
    pub do_not_interrupt_while_typing: bool,
    /// Only ever move focus when the user appears idle.
    pub focus_only_if_idle: bool,
    /// Minimum gap between two attention effects for this session.
    pub cooldown_seconds: u32,
    pub sound: Sound,
    /// Shell command for [`Action::Custom`].
    pub custom_command: Option<String>,
    /// Extra ranking weight in the queue. Signed, so a session can be pushed down.
    pub priority_boost: i16,
}

impl Default for AttentionPolicy {
    fn default() -> Self {
        Self {
            on_turn_complete: vec![Action::Badge, Action::Enqueue],
            on_question: vec![Action::Badge, Action::Enqueue, Action::Notify],
            // A blocked permission is the one case where the agent is burning
            // wall-clock waiting on us, so it earns a focus change by default —
            // but still governed by the typing and cooldown guards below.
            on_permission_required: vec![Action::Enqueue, Action::FocusIfIdle, Action::Sound],
            on_task_complete: vec![Action::Badge, Action::Enqueue, Action::Notify],
            on_failure: vec![
                Action::Badge,
                Action::Enqueue,
                Action::Notify,
                Action::Highlight,
            ],
            on_waiting_for_user: vec![Action::Badge, Action::Enqueue],
            // A new subagent must never move the user. Case D in the brief.
            on_subagent_appeared: vec![Action::Badge],
            do_not_interrupt_while_typing: true,
            focus_only_if_idle: false,
            cooldown_seconds: 10,
            sound: Sound::Subtle,
            custom_command: None,
            priority_boost: 0,
        }
    }
}

impl AttentionPolicy {
    /// A policy that stays completely silent. Useful for background sessions the
    /// user wants to check on manually.
    pub fn silent() -> Self {
        Self {
            on_turn_complete: vec![Action::Nothing],
            on_question: vec![Action::Nothing],
            on_permission_required: vec![Action::Badge],
            on_task_complete: vec![Action::Nothing],
            on_failure: vec![Action::Badge],
            on_waiting_for_user: vec![Action::Nothing],
            on_subagent_appeared: vec![Action::Nothing],
            do_not_interrupt_while_typing: true,
            focus_only_if_idle: true,
            cooldown_seconds: 60,
            sound: Sound::None,
            custom_command: None,
            priority_boost: -20,
        }
    }

    /// The actions configured for a trigger.
    pub fn actions_for(&self, trigger: Trigger) -> &[Action] {
        match trigger {
            Trigger::TurnComplete => &self.on_turn_complete,
            Trigger::Question => &self.on_question,
            Trigger::PermissionRequired => &self.on_permission_required,
            Trigger::TaskComplete => &self.on_task_complete,
            Trigger::Failure => &self.on_failure,
            Trigger::WaitingForUser => &self.on_waiting_for_user,
            Trigger::SubagentAppeared => &self.on_subagent_appeared,
        }
    }

    /// Resolves the actions for a trigger against the confidence of the event
    /// that fired it.
    ///
    /// A guessed state may badge and enqueue but must not move the user, so any
    /// focus action from a provisional event degrades to a badge. This is what
    /// makes the heuristic layer safe to ship.
    pub fn resolve(&self, trigger: Trigger, confidence: Confidence) -> Vec<Action> {
        let mut out = Vec::new();
        for action in self.actions_for(trigger) {
            let effective = if action.is_focus() && !confidence.may_steal_focus() {
                Action::Badge
            } else {
                *action
            };
            if effective == Action::Nothing {
                continue;
            }
            if !out.contains(&effective) {
                out.push(effective);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_guessed_permission_badges_instead_of_stealing_focus() {
        let policy = AttentionPolicy::default();
        let actions = policy.resolve(Trigger::PermissionRequired, Confidence::InferredHigh);
        assert!(
            !actions.iter().any(|a| a.is_focus()),
            "heuristics must not move the user: {actions:?}"
        );
        assert!(actions.contains(&Action::Badge));
        assert!(actions.contains(&Action::Enqueue));
    }

    #[test]
    fn an_explicit_permission_keeps_its_focus_action() {
        let policy = AttentionPolicy::default();
        let actions = policy.resolve(Trigger::PermissionRequired, Confidence::Explicit);
        assert!(actions.contains(&Action::FocusIfIdle));
    }

    #[test]
    fn a_new_subagent_never_moves_focus_even_when_explicit() {
        let policy = AttentionPolicy::default();
        let actions = policy.resolve(Trigger::SubagentAppeared, Confidence::Explicit);
        assert_eq!(actions, vec![Action::Badge]);
    }

    #[test]
    fn nothing_is_dropped_from_the_resolved_set() {
        let policy = AttentionPolicy::silent();
        let actions = policy.resolve(Trigger::TurnComplete, Confidence::Explicit);
        assert!(actions.is_empty());
    }

    #[test]
    fn resolve_deduplicates_after_degrading_focus_to_badge() {
        let policy = AttentionPolicy {
            on_question: vec![Action::Badge, Action::Focus, Action::FocusIfIdle],
            ..AttentionPolicy::default()
        };
        let actions = policy.resolve(Trigger::Question, Confidence::InferredLow);
        assert_eq!(actions, vec![Action::Badge]);
    }

    #[test]
    fn triggers_are_derived_from_events() {
        assert_eq!(
            Trigger::from_event(&EventKind::AgentIdle),
            None,
            "idle is not an attention moment"
        );
        assert_eq!(
            Trigger::from_event(&EventKind::AgentTurnCompleted {
                last_message: None,
                background_tasks: 0,
            }),
            Some(Trigger::TurnComplete)
        );
        assert_eq!(
            Trigger::from_event(&EventKind::ProcessFailed {
                code: Some(1),
                signal: None
            }),
            Some(Trigger::Failure)
        );
    }

    #[test]
    fn the_silent_policy_still_surfaces_failures_and_permissions_quietly() {
        let policy = AttentionPolicy::silent();
        assert_eq!(
            policy.resolve(Trigger::Failure, Confidence::Explicit),
            vec![Action::Badge]
        );
        assert_eq!(
            policy.resolve(Trigger::PermissionRequired, Confidence::Explicit),
            vec![Action::Badge]
        );
    }
}
