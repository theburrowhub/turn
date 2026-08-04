//! Telling the daemon what the user is doing.
//!
//! The focus governor decides whether Turn may move somebody, and it cannot decide
//! that without knowing whether their hands are on the keyboard. Without this the
//! window would interrupt people mid-keystroke, which is the single behaviour that
//! would make the product unusable — so this is not telemetry, it is an input to a
//! product rule.
//!
//! ## On change, not on a timer
//!
//! The protocol is explicit: send on a transition. The interesting ones are the first
//! keystroke of a burst, the window losing or gaining focus, the active session
//! changing, and a modal opening or closing. Sending on a timer would put a request on
//! the socket every second per window for information that has not changed, and
//! sending per keystroke would put one there sixty times a second while somebody types
//! a commit message.
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
        let changed = match &self.sent {
            None => true,
            Some(sent) => {
                sent.app_foreground != self.current.app_foreground
                    || sent.active_session != self.current.active_session
                    || sent.sensitive_operation != self.current.sensitive_operation
                    // The keystroke timestamp itself changes on every key. What matters
                    // is whether the *state* changed: a burst is one transition in and
                    // one out, not one per character.
                    || self.sent_typing != self.current.is_typing(now_ms)
            }
        };
        if !changed {
            return None;
        }
        self.sent = Some(self.current.clone());
        self.sent_typing = self.current.is_typing(now_ms);
        Some(self.current.clone())
    }

    /// When the window has to come back to notice that typing stopped.
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
        // A moment past the grace period, so the wake-up lands on the far side of the
        // boundary rather than exactly on it.
        Some(last + TYPING_GRACE_MS + 1)
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
