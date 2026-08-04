//! Focus governance.
//!
//! Moving the user's viewport is the most disruptive thing Turn can do, so every
//! focus request passes through here regardless of which policy asked for it.
//! The governor owns the guards that policies must not be able to opt out of:
//! no interrupting a keystroke, no ping-pong between two chatty sessions, and a
//! hard ceiling on focus changes per minute.

use crate::attention::policy::{Action, AttentionPolicy};
use crate::ids::SessionId;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// How long after the last keystroke the user still counts as typing.
pub const TYPING_GRACE_MS: i64 = 1_500;
/// Minimum gap between any two focus changes, whatever the policies say.
pub const MIN_FOCUS_INTERVAL_MS: i64 = 2_000;
/// Ceiling on focus changes inside [`FOCUS_WINDOW_MS`].
pub const MAX_FOCUS_CHANGES_PER_WINDOW: usize = 3;
pub const FOCUS_WINDOW_MS: i64 = 10_000;
/// A session Turn just moved the user away from cannot pull them back for this
/// long. Without it, two agents finishing together bounce the user endlessly.
pub const PING_PONG_GUARD_MS: i64 = 5_000;

/// What the user is doing right now, as far as the UI can tell.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserContext {
    /// Timestamp of the most recent keystroke, if any.
    pub last_keystroke_ms: Option<i64>,
    /// Whether the Turn window is frontmost.
    pub app_foreground: bool,
    /// The session the user is looking at.
    pub active_session: Option<SessionId>,
    /// Set by the UI while something must not be interrupted: a permission
    /// dialog being read, a paste in flight, a modal open.
    pub sensitive_operation: bool,
}

impl UserContext {
    pub fn is_typing(&self, now_ms: i64) -> bool {
        match self.last_keystroke_ms {
            Some(last) => now_ms.saturating_sub(last) < TYPING_GRACE_MS,
            None => false,
        }
    }

    /// When the user will stop counting as typing.
    pub fn typing_clears_at(&self, now_ms: i64) -> i64 {
        match self.last_keystroke_ms {
            Some(last) => (last + TYPING_GRACE_MS).max(now_ms),
            None => now_ms,
        }
    }
}

/// Why a focus request was turned down.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FocusDenial {
    /// Already looking at it.
    AlreadyFocused,
    /// A modal or paste is in flight.
    SensitiveOperation,
    /// The request was `FocusIfBackground` and the window is frontmost.
    AppInForeground,
    /// Too many focus changes recently. The badge still appears.
    RateLimited,
    /// This session just lost focus; it may not immediately reclaim it.
    PingPongGuard,
}

/// The governor's verdict on a focus request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FocusDecision {
    /// Move the user now.
    Grant,
    /// Do not move now; re-evaluate at this timestamp. The session is badged in
    /// the meantime, so the signal is never lost — only delayed.
    Defer { until_ms: i64, reason: DeferReason },
    /// Do not move at all.
    Deny { reason: FocusDenial },
}

/// Why a focus change was postponed rather than refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeferReason {
    /// The user is mid-keystroke. Case B in the brief.
    UserTyping,
    /// The policy only allows focus when idle.
    RequiresIdle,
    /// The session's own cooldown has not elapsed.
    SessionCooldown,
    /// The global minimum interval between focus changes.
    GlobalInterval,
}

/// Tracks recent focus activity and rules on new requests.
#[derive(Debug, Clone, Default)]
pub struct FocusGovernor {
    /// Timestamps of granted focus changes, newest last.
    recent_grants: VecDeque<i64>,
    /// The session focus was last moved away from, and when.
    last_yielded: Option<(SessionId, i64)>,
    last_grant_ms: Option<i64>,
}

impl FocusGovernor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Rules on a focus request for `target`.
    ///
    /// `action` must be one of the focus actions; anything else is a programming
    /// error and is denied rather than silently granted.
    pub fn evaluate(
        &self,
        action: Action,
        target: &SessionId,
        policy: &AttentionPolicy,
        ctx: &UserContext,
        session_last_effect_ms: Option<i64>,
        now_ms: i64,
    ) -> FocusDecision {
        if !action.is_focus() {
            return FocusDecision::Deny {
                reason: FocusDenial::AlreadyFocused,
            };
        }

        // Already there: nothing to do, and reporting it as granted would burn a
        // slot in the rate limiter for a no-op.
        if ctx.active_session.as_ref() == Some(target) {
            return FocusDecision::Deny {
                reason: FocusDenial::AlreadyFocused,
            };
        }

        if ctx.sensitive_operation {
            return FocusDecision::Deny {
                reason: FocusDenial::SensitiveOperation,
            };
        }

        if action == Action::FocusIfBackground && ctx.app_foreground {
            return FocusDecision::Deny {
                reason: FocusDenial::AppInForeground,
            };
        }

        // A session Turn just moved away from must not immediately drag the user
        // back. This is the anti-loop guard.
        if let Some((ref yielded, at)) = self.last_yielded {
            if yielded == target && now_ms.saturating_sub(at) < PING_PONG_GUARD_MS {
                return FocusDecision::Deny {
                    reason: FocusDenial::PingPongGuard,
                };
            }
        }

        // Hard ceiling on how often the viewport may move.
        let recent = self
            .recent_grants
            .iter()
            .filter(|t| now_ms.saturating_sub(**t) < FOCUS_WINDOW_MS)
            .count();
        if recent >= MAX_FOCUS_CHANGES_PER_WINDOW {
            return FocusDecision::Deny {
                reason: FocusDenial::RateLimited,
            };
        }

        // Typing wins over every policy. Deferred, not denied: the user still
        // gets taken there once their hands stop.
        let typing = ctx.is_typing(now_ms);
        if typing && policy.do_not_interrupt_while_typing {
            return FocusDecision::Defer {
                until_ms: ctx.typing_clears_at(now_ms),
                reason: DeferReason::UserTyping,
            };
        }
        if typing && (policy.focus_only_if_idle || action == Action::FocusIfIdle) {
            return FocusDecision::Defer {
                until_ms: ctx.typing_clears_at(now_ms),
                reason: DeferReason::RequiresIdle,
            };
        }

        if let Some(last) = session_last_effect_ms {
            let cooldown_ms = policy.cooldown_seconds as i64 * 1_000;
            if now_ms.saturating_sub(last) < cooldown_ms {
                return FocusDecision::Defer {
                    until_ms: last + cooldown_ms,
                    reason: DeferReason::SessionCooldown,
                };
            }
        }

        if let Some(last) = self.last_grant_ms {
            if now_ms.saturating_sub(last) < MIN_FOCUS_INTERVAL_MS {
                return FocusDecision::Defer {
                    until_ms: last + MIN_FOCUS_INTERVAL_MS,
                    reason: DeferReason::GlobalInterval,
                };
            }
        }

        FocusDecision::Grant
    }

    /// Records that focus actually moved. Must be called by whoever applies a
    /// [`FocusDecision::Grant`], or the guards have nothing to work with.
    pub fn record_grant(&mut self, from: Option<SessionId>, now_ms: i64) {
        self.recent_grants.push_back(now_ms);
        while let Some(front) = self.recent_grants.front() {
            if now_ms.saturating_sub(*front) >= FOCUS_WINDOW_MS {
                self.recent_grants.pop_front();
            } else {
                break;
            }
        }
        self.last_grant_ms = Some(now_ms);
        if let Some(previous) = from {
            self.last_yielded = Some((previous, now_ms));
        }
    }

    /// Focus changes granted inside the current window.
    pub fn recent_grant_count(&self, now_ms: i64) -> usize {
        self.recent_grants
            .iter()
            .filter(|t| now_ms.saturating_sub(**t) < FOCUS_WINDOW_MS)
            .count()
    }

    /// Clears the rate-limit history. Called when the user navigates by hand,
    /// since their own actions are not interruptions.
    pub fn reset(&mut self) {
        self.recent_grants.clear();
        self.last_grant_ms = None;
        self.last_yielded = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: i64 = 1_700_000_000_000;

    fn target() -> SessionId {
        SessionId::from_stored("sess_target01")
    }

    fn ctx_idle() -> UserContext {
        UserContext {
            last_keystroke_ms: None,
            app_foreground: true,
            active_session: Some(SessionId::from_stored("sess_current1")),
            sensitive_operation: false,
        }
    }

    /// Case B from the brief: the user is typing, so focus waits.
    #[test]
    fn typing_defers_focus_rather_than_dropping_the_signal() {
        let gov = FocusGovernor::new();
        let ctx = UserContext {
            last_keystroke_ms: Some(T0 - 200),
            ..ctx_idle()
        };
        let decision = gov.evaluate(
            Action::Focus,
            &target(),
            &AttentionPolicy::default(),
            &ctx,
            None,
            T0,
        );
        match decision {
            FocusDecision::Defer { until_ms, reason } => {
                assert_eq!(reason, DeferReason::UserTyping);
                assert_eq!(until_ms, T0 - 200 + TYPING_GRACE_MS);
            }
            other => panic!("expected a deferral, got {other:?}"),
        }
    }

    #[test]
    fn focus_is_granted_once_the_user_stops_typing() {
        let gov = FocusGovernor::new();
        let ctx = UserContext {
            last_keystroke_ms: Some(T0 - TYPING_GRACE_MS - 1),
            ..ctx_idle()
        };
        assert_eq!(
            gov.evaluate(
                Action::Focus,
                &target(),
                &AttentionPolicy::default(),
                &ctx,
                None,
                T0
            ),
            FocusDecision::Grant
        );
    }

    #[test]
    fn a_policy_cannot_opt_out_of_the_typing_guard() {
        // Even a policy that asks for unconditional focus is held back, because
        // the guard lives in the governor, not the policy.
        let gov = FocusGovernor::new();
        let policy = AttentionPolicy {
            do_not_interrupt_while_typing: true,
            focus_only_if_idle: false,
            cooldown_seconds: 0,
            ..AttentionPolicy::default()
        };
        let ctx = UserContext {
            last_keystroke_ms: Some(T0),
            ..ctx_idle()
        };
        assert!(matches!(
            gov.evaluate(Action::Focus, &target(), &policy, &ctx, None, T0),
            FocusDecision::Defer { .. }
        ));
    }

    #[test]
    fn a_user_who_disabled_the_typing_guard_gets_focus_mid_keystroke() {
        let gov = FocusGovernor::new();
        let policy = AttentionPolicy {
            do_not_interrupt_while_typing: false,
            focus_only_if_idle: false,
            cooldown_seconds: 0,
            ..AttentionPolicy::default()
        };
        let ctx = UserContext {
            last_keystroke_ms: Some(T0),
            ..ctx_idle()
        };
        assert_eq!(
            gov.evaluate(Action::Focus, &target(), &policy, &ctx, None, T0),
            FocusDecision::Grant
        );
    }

    /// Case A: several agents finish at the same instant. Only one of them can
    /// take the user; the rest are held back rather than fighting over focus.
    #[test]
    fn agents_finishing_at_the_same_instant_produce_one_focus_change() {
        let mut gov = FocusGovernor::new();
        let policy = AttentionPolicy {
            cooldown_seconds: 0,
            ..AttentionPolicy::default()
        };
        let mut granted = 0;
        let mut current = Some(SessionId::from_stored("sess_start001"));

        for i in 0..6 {
            let s = SessionId::from_stored(format!("sess_{i:08}"));
            let ctx = UserContext {
                active_session: current.clone(),
                ..ctx_idle()
            };
            if gov.evaluate(Action::Focus, &s, &policy, &ctx, None, T0) == FocusDecision::Grant {
                gov.record_grant(current.clone(), T0);
                current = Some(s);
                granted += 1;
            }
        }
        assert_eq!(
            granted, 1,
            "a simultaneous burst moves the user exactly once"
        );
    }

    /// Spread out over time, focus changes are capped per sliding window rather
    /// than in total — the user may legitimately be pulled around over an hour,
    /// just never three times in ten seconds.
    #[test]
    fn focus_changes_never_exceed_the_ceiling_within_any_single_window() {
        let mut gov = FocusGovernor::new();
        let policy = AttentionPolicy {
            cooldown_seconds: 0,
            ..AttentionPolicy::default()
        };
        let mut grants = Vec::new();
        let mut current = Some(SessionId::from_stored("sess_start001"));

        for i in 0..20 {
            let s = SessionId::from_stored(format!("sess_{i:08}"));
            let now = T0 + i as i64 * MIN_FOCUS_INTERVAL_MS;
            let ctx = UserContext {
                active_session: current.clone(),
                ..ctx_idle()
            };
            if gov.evaluate(Action::Focus, &s, &policy, &ctx, None, now) == FocusDecision::Grant {
                gov.record_grant(current.clone(), now);
                current = Some(s);
                grants.push(now);
            }
        }

        assert!(!grants.is_empty(), "some focus changes must get through");
        for &start in &grants {
            let in_window = grants
                .iter()
                .filter(|t| **t >= start && **t - start < FOCUS_WINDOW_MS)
                .count();
            assert!(
                in_window <= MAX_FOCUS_CHANGES_PER_WINDOW,
                "{in_window} focus changes inside the window starting at {start}"
            );
        }
    }

    #[test]
    fn a_session_cannot_immediately_reclaim_focus_it_just_lost() {
        let mut gov = FocusGovernor::new();
        let noisy = SessionId::from_stored("sess_noisy001");
        let other = SessionId::from_stored("sess_other001");

        // Focus moves away from `noisy` toward `other`.
        gov.record_grant(Some(noisy.clone()), T0);

        let ctx = UserContext {
            active_session: Some(other),
            ..ctx_idle()
        };
        let policy = AttentionPolicy {
            cooldown_seconds: 0,
            ..AttentionPolicy::default()
        };
        assert_eq!(
            gov.evaluate(
                Action::Focus,
                &noisy,
                &policy,
                &ctx,
                None,
                T0 + PING_PONG_GUARD_MS - 1
            ),
            FocusDecision::Deny {
                reason: FocusDenial::PingPongGuard
            }
        );
        // Once the guard expires it may pull the user back.
        assert_eq!(
            gov.evaluate(
                Action::Focus,
                &noisy,
                &policy,
                &ctx,
                None,
                T0 + PING_PONG_GUARD_MS + MIN_FOCUS_INTERVAL_MS
            ),
            FocusDecision::Grant
        );
    }

    #[test]
    fn focusing_the_session_already_on_screen_is_a_no_op() {
        let gov = FocusGovernor::new();
        let ctx = UserContext {
            active_session: Some(target()),
            ..ctx_idle()
        };
        assert_eq!(
            gov.evaluate(
                Action::Focus,
                &target(),
                &AttentionPolicy::default(),
                &ctx,
                None,
                T0
            ),
            FocusDecision::Deny {
                reason: FocusDenial::AlreadyFocused
            }
        );
    }

    #[test]
    fn a_sensitive_operation_blocks_focus_outright() {
        let gov = FocusGovernor::new();
        let ctx = UserContext {
            sensitive_operation: true,
            ..ctx_idle()
        };
        assert_eq!(
            gov.evaluate(
                Action::Focus,
                &target(),
                &AttentionPolicy::default(),
                &ctx,
                None,
                T0
            ),
            FocusDecision::Deny {
                reason: FocusDenial::SensitiveOperation
            }
        );
    }

    #[test]
    fn focus_if_background_stays_put_while_the_window_is_frontmost() {
        let gov = FocusGovernor::new();
        let ctx = ctx_idle();
        assert_eq!(
            gov.evaluate(
                Action::FocusIfBackground,
                &target(),
                &AttentionPolicy::default(),
                &ctx,
                None,
                T0
            ),
            FocusDecision::Deny {
                reason: FocusDenial::AppInForeground
            }
        );

        let background = UserContext {
            app_foreground: false,
            ..ctx_idle()
        };
        assert_eq!(
            gov.evaluate(
                Action::FocusIfBackground,
                &target(),
                &AttentionPolicy::default(),
                &background,
                None,
                T0
            ),
            FocusDecision::Grant
        );
    }

    #[test]
    fn the_session_cooldown_defers_repeat_effects() {
        let gov = FocusGovernor::new();
        let policy = AttentionPolicy {
            cooldown_seconds: 10,
            ..AttentionPolicy::default()
        };
        let decision = gov.evaluate(
            Action::Focus,
            &target(),
            &policy,
            &ctx_idle(),
            Some(T0 - 5_000),
            T0,
        );
        assert_eq!(
            decision,
            FocusDecision::Defer {
                until_ms: T0 - 5_000 + 10_000,
                reason: DeferReason::SessionCooldown
            }
        );
    }

    #[test]
    fn the_rate_limit_window_slides() {
        let mut gov = FocusGovernor::new();
        let last = T0 + (MAX_FOCUS_CHANGES_PER_WINDOW as i64 - 1) * 100;
        for i in 0..MAX_FOCUS_CHANGES_PER_WINDOW {
            gov.record_grant(None, T0 + i as i64 * 100);
        }
        assert_eq!(gov.recent_grant_count(T0), MAX_FOCUS_CHANGES_PER_WINDOW);

        // Partway along, the oldest grant has aged out but the newer ones stand.
        assert_eq!(
            gov.recent_grant_count(T0 + FOCUS_WINDOW_MS),
            MAX_FOCUS_CHANGES_PER_WINDOW - 1
        );
        // Past the window from the *newest* grant, history is clear.
        assert_eq!(gov.recent_grant_count(last + FOCUS_WINDOW_MS), 0);
    }

    #[test]
    fn manual_navigation_resets_the_guards() {
        let mut gov = FocusGovernor::new();
        for i in 0..MAX_FOCUS_CHANGES_PER_WINDOW {
            gov.record_grant(Some(target()), T0 + i as i64 * 100);
        }
        gov.reset();
        assert_eq!(gov.recent_grant_count(T0), 0);
        assert_eq!(
            gov.evaluate(
                Action::Focus,
                &target(),
                &AttentionPolicy {
                    cooldown_seconds: 0,
                    ..AttentionPolicy::default()
                },
                &ctx_idle(),
                None,
                T0
            ),
            FocusDecision::Grant
        );
    }

    #[test]
    fn non_focus_actions_are_rejected_rather_than_silently_granted() {
        let gov = FocusGovernor::new();
        assert!(matches!(
            gov.evaluate(
                Action::Badge,
                &target(),
                &AttentionPolicy::default(),
                &ctx_idle(),
                None,
                T0
            ),
            FocusDecision::Deny { .. }
        ));
    }
}
