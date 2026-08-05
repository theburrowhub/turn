//! Telling the daemon what the user is doing.
//!
//! The focus governor decides whether Turn may move somebody, and it cannot decide
//! that without knowing whether their hands are on the keyboard. Without this the
//! window would interrupt people mid-keystroke, which is the single behaviour that
//! would make the product unusable — so this is not telemetry, it is an input to a
//! product rule.
//!
//! ## On change, with a bounded heartbeat while typing
//!
//! The protocol is explicit: send on a transition. The interesting ones are the first
//! keystroke of a burst, the window losing or gaining focus, the active session
//! changing, and a modal opening or closing. One extra transition is necessary during a
//! long burst: before the timestamp the daemon already knows can age out, the newest
//! coalesced keystroke is sent as a heartbeat. That keeps the no-focus-while-typing
//! guarantee true without putting one request per character on the socket.
//!
//! The end of a burst is a transition too, and an important one: the governor may have
//! a focus jump deferred waiting for exactly that moment. It is not an event the window
//! receives, though — it is the *absence* of events for a while — so
//! [`ActivityTracker::wake_at`] tells the repaint planner when to bring the window back
//! for long enough to notice. That is why the two modules know about each other.
//!
//! ## `is_typing` is derived, not asserted
//!
//! What travels is `last_keystroke_ms`, and the daemon works out from it whether the
//! user is typing. A boolean would be a thing the window could forget to clear — after
//! a crash, after a dropped key-up — and the daemon would then never move anybody
//! again.

use turn_core::attention::focus::TYPING_GRACE_MS;
use turn_core::attention::UserContext;
use turn_core::ids::SessionId;

/// Refresh the daemon twice inside its typing grace window.
///
/// The margin matters because the daemon has its own clock-driven tick. Refreshing only
/// at the edge would let that tick observe an expired timestamp while the UI was still
/// delivering the update.
const TYPING_HEARTBEAT_MS: i64 = TYPING_GRACE_MS / 2;

/// Tracks what the user is doing and produces an update when it materially changes.
#[derive(Debug, Default)]
pub struct ActivityTracker {
    current: UserContext,
    /// The last context actually sent, so "changed" means changed since the daemon
    /// heard about it rather than since the last frame.
    sent: Option<UserContext>,
    /// Whether the last report said the user was typing.
    ///
    /// Stored rather than recomputed from `sent`. Recomputing it would compare the same
    /// timestamp against the same clock and always agree with itself, so the end of a
    /// burst — the transition the focus governor is waiting for — would never be
    /// reported at all.
    sent_typing: bool,
}

impl ActivityTracker {
    pub fn new() -> Self {
        Self {
            current: UserContext {
                last_keystroke_ms: None,
                app_foreground: true,
                active_session: None,
                sensitive_operation: false,
            },
            sent: None,
            sent_typing: false,
        }
    }

    /// The context as it stands, for a window that wants to show it.
    pub fn context(&self) -> &UserContext {
        &self.current
    }

    /// A key was pressed.
    pub fn keystroke(&mut self, now_ms: i64) {
        self.current.last_keystroke_ms = Some(now_ms);
    }

    /// The window gained or lost focus.
    pub fn window_focus(&mut self, focused: bool) {
        self.current.app_foreground = focused;
    }

    /// The user is looking at a different session.
    pub fn active_session(&mut self, session: Option<SessionId>) {
        self.current.active_session = session;
    }

    /// Something is on screen that must not be interrupted: a permission being read, a
    /// paste in flight, a modal open.
    pub fn sensitive_operation(&mut self, sensitive: bool) {
        self.current.sensitive_operation = sensitive;
    }

    /// The update to send, or `None` when nothing the governor cares about has moved.
    ///
    /// Takes `now_ms` because whether the user counts as typing is a function of the
    /// clock, and the end of a burst is one of the transitions worth reporting.
    pub fn take_update(&mut self, now_ms: i64) -> Option<UserContext> {
        let typing = self.current.is_typing(now_ms);
        let changed = match &self.sent {
            None => true,
            Some(sent) => {
                sent.app_foreground != self.current.app_foreground
                    || sent.active_session != self.current.active_session
                    || sent.sensitive_operation != self.current.sensitive_operation
                    || self.sent_typing != typing
                    // Coalesce keystrokes until the timestamp the daemon knows is halfway
                    // to expiring. This is a bounded heartbeat, not a per-key report.
                    || (typing
                        && newer_keystroke_is_due(
                            sent.last_keystroke_ms,
                            self.current.last_keystroke_ms,
                            now_ms,
                        ))
            }
        };
        if !changed {
            return None;
        }
        self.sent = Some(self.current.clone());
        self.sent_typing = typing;
        Some(self.current.clone())
    }

    /// When the window has to come back for a typing heartbeat or burst expiry.
    ///
    /// `None` when there is nothing pending. The governor may be holding a focus jump
    /// until the user's hands leave the keyboard, and that moment arrives by the clock
    /// rather than by an event, so somebody has to be awake for it.
    pub fn wake_at(&self, now_ms: i64) -> Option<i64> {
        let last = self.current.last_keystroke_ms?;
        if !self.current.is_typing(now_ms) {
            // The burst is already over. A wake-up is still owed when the last report
            // said otherwise, because that transition has not been sent yet.
            return if self.sent_typing { Some(now_ms) } else { None };
        }

        let expires_at = last + TYPING_GRACE_MS + 1;
        let Some(sent) = &self.sent else {
            return Some(now_ms);
        };
        let Some(sent_last) = sent.last_keystroke_ms else {
            return Some(now_ms);
        };
        if self.current.last_keystroke_ms != Some(sent_last) {
            // A newer key is waiting in the coalescing buffer. Wake before the
            // daemon's older timestamp can expire even if no more input arrives.
            let heartbeat_at = sent_last.saturating_add(TYPING_HEARTBEAT_MS);
            return Some(heartbeat_at.max(now_ms).min(expires_at));
        }

        // A moment past the grace period, so the wake-up lands on the far side of the
        // boundary rather than exactly on it.
        Some(expires_at)
    }
}

fn newer_keystroke_is_due(sent: Option<i64>, current: Option<i64>, now_ms: i64) -> bool {
    match (sent, current) {
        (Some(sent), Some(current)) if current != sent => {
            now_ms.saturating_sub(sent) >= TYPING_HEARTBEAT_MS
        }
        // The opening context contained no key, so the first one is a transition and
        // must travel immediately.
        (None, Some(_)) => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: i64 = 1_700_000_000_000;

    #[test]
    fn the_first_report_is_always_sent_because_the_daemon_knows_nothing_yet() {
        let mut tracker = ActivityTracker::new();
        let update = tracker.take_update(T0).expect("the opening report");
        assert!(update.app_foreground);
        assert_eq!(update.last_keystroke_ms, None);
        assert!(
            tracker.take_update(T0).is_none(),
            "and nothing has changed since"
        );
    }

    /// The rule that keeps the socket quiet: a burst of typing is one transition in,
    /// not one request per character.
    #[test]
    fn a_burst_of_typing_reports_once_rather_than_once_per_key() {
        let mut tracker = ActivityTracker::new();
        tracker.take_update(T0);

        tracker.keystroke(T0);
        let first = tracker.take_update(T0).expect("the burst starting");
        assert_eq!(first.last_keystroke_ms, Some(T0));

        for offset in [50, 120, 300, 700] {
            tracker.keystroke(T0 + offset);
            assert!(
                tracker.take_update(T0 + offset).is_none(),
                "still the same burst at +{offset}ms"
            );
        }
    }

    #[test]
    fn a_long_typing_burst_refreshes_before_the_daemons_grace_expires() {
        let mut tracker = ActivityTracker::new();
        tracker.take_update(T0);
        tracker.keystroke(T0);
        let mut daemon_context = tracker.take_update(T0).expect("the burst starting");
        let mut reports = 1;

        for offset in (100..=5_000).step_by(100) {
            let now = T0 + offset;
            tracker.keystroke(now);
            if let Some(update) = tracker.take_update(now) {
                daemon_context = update;
                reports += 1;
            }
            let reported_key = daemon_context
                .last_keystroke_ms
                .expect("the daemon knows about this burst");
            assert!(
                now.saturating_sub(reported_key) < TYPING_GRACE_MS,
                "the daemon's timestamp expired during a live burst at +{offset}ms"
            );
            assert!(daemon_context.is_typing(now));
        }

        assert!(reports > 1, "a five-second burst needs heartbeats");
    }

    #[test]
    fn typing_heartbeats_are_bounded_instead_of_being_sent_per_key() {
        let mut tracker = ActivityTracker::new();
        tracker.take_update(T0);
        tracker.keystroke(T0);
        let mut reports = usize::from(tracker.take_update(T0).is_some());
        let mut keys = 1;
        let duration_ms = 10_000;

        for offset in (10..=duration_ms).step_by(10) {
            let now = T0 + offset;
            tracker.keystroke(now);
            keys += 1;
            reports += usize::from(tracker.take_update(now).is_some());
        }

        let upper_bound = 2 + duration_ms as usize / TYPING_HEARTBEAT_MS as usize;
        assert!(
            reports <= upper_bound,
            "{keys} keys produced {reports} reports; the bound was {upper_bound}"
        );
        assert!(
            reports * 10 < keys,
            "heartbeats regressed to per-key traffic"
        );
    }

    /// And the other end of the burst, which is the transition the governor is waiting
    /// for: it may have a focus jump deferred until exactly this moment.
    #[test]
    fn the_end_of_a_burst_is_reported_so_a_deferred_jump_can_be_released() {
        let mut tracker = ActivityTracker::new();
        tracker.take_update(T0);
        tracker.keystroke(T0);
        tracker.take_update(T0);

        let after = T0 + TYPING_GRACE_MS + 1;
        let update = tracker
            .take_update(after)
            .expect("the user's hands have left the keyboard");
        assert!(!update.is_typing(after));
        assert!(
            tracker.take_update(after).is_none(),
            "and it is reported once"
        );
    }

    /// The end of a burst arrives by the clock, not by an event, so the window has to
    /// be woken for it.
    #[test]
    fn the_window_is_asked_to_wake_up_when_a_burst_will_expire() {
        let mut tracker = ActivityTracker::new();
        tracker.take_update(T0);
        assert_eq!(
            tracker.wake_at(T0),
            None,
            "nothing is pending before anything is typed"
        );

        tracker.keystroke(T0);
        tracker.take_update(T0);
        assert_eq!(
            tracker.wake_at(T0 + 100),
            Some(T0 + TYPING_GRACE_MS + 1),
            "the window must be awake just past the grace period"
        );

        // Once it has been reported as not typing, there is nothing left to wake for.
        let after = T0 + TYPING_GRACE_MS + 1;
        tracker.take_update(after);
        assert_eq!(tracker.wake_at(after), None);
    }

    #[test]
    fn an_unsent_newer_key_schedules_a_heartbeat_even_without_more_input() {
        let mut tracker = ActivityTracker::new();
        tracker.take_update(T0);
        tracker.keystroke(T0);
        tracker.take_update(T0);

        tracker.keystroke(T0 + 200);
        assert!(
            tracker.take_update(T0 + 200).is_none(),
            "the second key is coalesced"
        );
        assert_eq!(
            tracker.wake_at(T0 + 200),
            Some(T0 + TYPING_HEARTBEAT_MS),
            "the window wakes before the daemon's old timestamp expires"
        );

        let heartbeat = tracker
            .take_update(T0 + TYPING_HEARTBEAT_MS)
            .expect("the scheduled heartbeat");
        assert_eq!(heartbeat.last_keystroke_ms, Some(T0 + 200));
        assert!(heartbeat.is_typing(T0 + TYPING_HEARTBEAT_MS));
    }

    #[test]
    fn deferred_focus_never_lands_during_a_continuous_typing_burst() {
        use turn_core::attention::{AttentionManager, AttentionPolicy, DeferReason, Effect};
        use turn_core::event::{Confidence, EventKind, EventSource, Risk, TurnEvent};
        use turn_core::ids::NodeId;

        let mut tracker = ActivityTracker::new();
        tracker.take_update(T0);
        tracker.keystroke(T0);
        let mut daemon_context = tracker.take_update(T0).expect("the burst starting");

        let session = SessionId::from_stored("sess_typing_heartbeat");
        let permission = TurnEvent::new(
            session,
            EventKind::AgentPermissionRequired {
                summary: "run tests".into(),
                command: Some("cargo test".into()),
                tool_name: Some("Bash".into()),
                risk: Risk::Medium,
            },
            EventSource::Hook {
                tool: "claude-code".into(),
                event_name: "permission_prompt".into(),
            },
            Confidence::Explicit,
            T0,
        )
        .with_node(NodeId::from_stored("agent_typing_heartbeat"));
        let mut manager = AttentionManager::new();
        let initial = manager.ingest(
            &permission,
            &AttentionPolicy::default(),
            &daemon_context,
            T0,
        );
        assert!(initial.iter().any(|effect| matches!(
            effect,
            Effect::FocusDeferred {
                reason: DeferReason::UserTyping,
                ..
            }
        )));

        for offset in (500..=4_000).step_by(500) {
            let now = T0 + offset;
            tracker.keystroke(now);
            if let Some(update) = tracker.take_update(now) {
                daemon_context = update;
            }
            assert!(daemon_context.is_typing(now));
            let effects = manager.tick(&daemon_context, now);
            assert!(
                !effects
                    .iter()
                    .any(|effect| matches!(effect, Effect::Focus { .. })),
                "focus landed while the user was still typing at +{offset}ms: {effects:?}"
            );
        }

        let idle_at = T0 + 4_000 + TYPING_GRACE_MS + 1;
        daemon_context = tracker
            .take_update(idle_at)
            .expect("the end of the burst is reported");
        assert!(!daemon_context.is_typing(idle_at));
        assert!(manager
            .tick(&daemon_context, idle_at)
            .iter()
            .any(|effect| matches!(effect, Effect::Focus { .. })));
    }

    #[test]
    fn losing_the_window_is_reported_at_once_because_it_changes_what_may_interrupt() {
        let mut tracker = ActivityTracker::new();
        tracker.take_update(T0);
        tracker.window_focus(false);
        let update = tracker.take_update(T0).expect("the window went away");
        assert!(!update.app_foreground);
        assert!(tracker.take_update(T0).is_none());

        tracker.window_focus(true);
        assert!(
            tracker
                .take_update(T0)
                .expect("and coming back is a transition too")
                .app_foreground
        );
    }

    #[test]
    fn changing_session_is_reported_because_the_governor_will_not_move_you_to_where_you_are() {
        let mut tracker = ActivityTracker::new();
        tracker.take_update(T0);
        let session = SessionId::from_stored("sess_looking0001");
        tracker.active_session(Some(session.clone()));
        assert_eq!(
            tracker
                .take_update(T0)
                .expect("the active session moved")
                .active_session,
            Some(session)
        );
        assert!(tracker.take_update(T0).is_none());
    }

    /// The flag that stops Turn moving somebody who is reading a permission prompt.
    #[test]
    fn opening_and_closing_something_that_must_not_be_interrupted_is_reported_both_ways() {
        let mut tracker = ActivityTracker::new();
        tracker.take_update(T0);

        tracker.sensitive_operation(true);
        assert!(
            tracker
                .take_update(T0)
                .expect("a modal opened")
                .sensitive_operation
        );

        tracker.sensitive_operation(false);
        assert!(
            !tracker
                .take_update(T0)
                .expect("and closed")
                .sensitive_operation
        );
    }

    /// Two things changing between frames is one update, not two: the context travels
    /// whole.
    #[test]
    fn several_changes_between_frames_travel_as_one_report() {
        let mut tracker = ActivityTracker::new();
        tracker.take_update(T0);
        tracker.keystroke(T0);
        tracker.window_focus(false);
        tracker.sensitive_operation(true);

        let update = tracker.take_update(T0).expect("something changed");
        assert_eq!(update.last_keystroke_ms, Some(T0));
        assert!(!update.app_foreground);
        assert!(update.sensitive_operation);
        assert!(tracker.take_update(T0).is_none());
    }
}
