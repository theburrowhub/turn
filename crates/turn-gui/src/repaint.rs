//! When the window should draw again.
//!
//! egui is immediate-mode. By default an integration repaints continuously, which for
//! a tool somebody leaves open all day on a desk of thirty sessions means burning a
//! core to show a screen that has not changed. Turn's most explicit performance
//! criterion is that an idle window costs nothing, and this module is where that is
//! decided.
//!
//! The window therefore repaints for exactly three reasons:
//!
//! 1. **Something arrived.** The transport calls its waker when a frame comes off the
//!    socket, which is an `egui` repaint request from outside the frame loop. That is
//!    the whole mechanism for keeping up with a build: no polling.
//! 2. **The user did something.** egui handles that itself — input implies a frame.
//! 3. **A deadline passed.** A few things change by the clock rather than by an event:
//!    the cursor blinks, "blocked 47s" counts up, and a burst of typing expires while
//!    the focus governor may be waiting for it.
//!    Each of those names a time, [`RepaintPlan`] takes the earliest, and the frame
//!    happens then and not before.
//!
//! ## How this is checked
//!
//! [`RepaintPlan`] is a value, so the arithmetic is unit-tested. The behaviour that
//! matters, though, is that the composed window actually *settles* — and that is
//! asserted in `tests/snapshots.rs`, where `egui_kittest`'s `run()` returns the number
//! of frames it took before no more immediate repaints were requested. An idle window
//! settles in a couple of frames; one that repainted in a loop would run to the
//! harness's step limit and fail.

use std::time::Duration;

/// The longest a window will go without drawing.
///
/// Not a poll: with nothing on screen that changes by the clock there is no deadline
/// at all and the window sleeps until something happens. This is the ceiling for the
/// cases that do have one, so a mistake in a deadline calculation degrades to "once a
/// second" rather than to "never again".
pub const MAX_IDLE: Duration = Duration::from_secs(1);

/// How often a cursor changes phase.
///
/// Half a second on and half a second off is the convention every terminal follows.
pub const CURSOR_BLINK: Duration = Duration::from_millis(530);

/// How often a visible elapsed time is redrawn.
///
/// "blocked 47s" only needs a frame a second; anything faster is a repaint that
/// changes nothing.
pub const ELAPSED_TICK: Duration = Duration::from_secs(1);

/// When to draw next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepaintPlan {
    /// Nothing on screen changes by itself. Sleep until an event or a frame from the
    /// daemon.
    WhenSomethingHappens,
    /// Draw again after this long.
    After(Duration),
}

impl RepaintPlan {
    /// The plan for a frame, from the deadlines that apply to it.
    ///
    /// `now_ms` and the deadlines are wall-clock milliseconds so they compose with the
    /// rest of the crate, where time is always a parameter. A deadline in the past
    /// becomes a zero-length wait rather than a negative one, which is how a frame that
    /// arrived late catches up instead of stalling.
    pub fn from_deadlines(now_ms: i64, deadlines: &[Option<i64>]) -> RepaintPlan {
        let soonest = deadlines.iter().flatten().copied().min();
        match soonest {
            None => RepaintPlan::WhenSomethingHappens,
            Some(at) => {
                let wait = at.saturating_sub(now_ms).max(0) as u64;
                RepaintPlan::After(Duration::from_millis(wait).min(MAX_IDLE))
            }
        }
    }

    /// Whether the window is asking to be woken at all.
    pub fn is_idle(&self) -> bool {
        matches!(self, RepaintPlan::WhenSomethingHappens)
    }

    /// Applies the plan to a context.
    ///
    /// The idle case asks for nothing, which is what makes the window sleep: `egui`
    /// only draws again when an event or an outside repaint request arrives.
    pub fn apply(&self, ctx: &egui::Context) {
        if let RepaintPlan::After(delay) = self {
            ctx.request_repaint_after(*delay);
        }
    }
}

/// What the window has on screen that changes by the clock.
///
/// Grouped into a struct rather than passed as five booleans so that adding a reason
/// to repaint means naming it here, where the cost is visible, rather than dropping a
/// `request_repaint` into a draw function where nobody will find it again.
#[derive(Debug, Clone, Copy, Default)]
pub struct Deadlines {
    /// A visible cursor in a focused pane, which blinks.
    pub cursor_blink_at: Option<i64>,
    /// An elapsed time on screen — "blocked 47s", "12m idle".
    pub elapsed_tick_at: Option<i64>,
    /// A burst of typing that will expire, which the focus governor may be waiting
    /// for. See [`crate::activity::ActivityTracker::wake_at`].
    pub typing_expires_at: Option<i64>,
    /// The next reconnect attempt, so the status line's countdown stays honest.
    pub reconnect_at: Option<i64>,
}

impl Deadlines {
    /// The plan these deadlines imply.
    pub fn plan(&self, now_ms: i64) -> RepaintPlan {
        RepaintPlan::from_deadlines(
            now_ms,
            &[
                self.cursor_blink_at,
                self.elapsed_tick_at,
                self.typing_expires_at,
                self.reconnect_at,
            ],
        )
    }
}

/// Whether a blinking cursor is currently in its visible phase.
///
/// A function of the clock rather than of a stored flag, so it cannot get stuck on or
/// off after a frame is missed — and so it is the same in a snapshot test as in the
/// window.
pub fn cursor_visible(now_ms: i64, blink: bool) -> bool {
    if !blink {
        return true;
    }
    let period = CURSOR_BLINK.as_millis().max(1) as i64;
    // `rem_euclid` rather than `%` so a timestamp before the epoch does not invert the
    // phase.
    (now_ms.rem_euclid(period * 2)) < period
}

/// The next moment a blinking cursor changes phase.
pub fn next_cursor_phase(now_ms: i64) -> i64 {
    let period = CURSOR_BLINK.as_millis().max(1) as i64;
    let into_phase = now_ms.rem_euclid(period);
    now_ms + (period - into_phase)
}

/// The next whole-second boundary, for an elapsed time on screen.
pub fn next_elapsed_tick(now_ms: i64) -> i64 {
    let period = ELAPSED_TICK.as_millis().max(1) as i64;
    let into_phase = now_ms.rem_euclid(period);
    now_ms + (period - into_phase)
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: i64 = 1_700_000_000_000;

    /// The criterion: with nothing on screen that changes by itself, the window asks
    /// for nothing at all.
    #[test]
    fn an_idle_window_asks_for_no_repaint_at_all() {
        let plan = Deadlines::default().plan(T0);
        assert_eq!(plan, RepaintPlan::WhenSomethingHappens);
        assert!(plan.is_idle());
    }

    #[test]
    fn the_soonest_deadline_is_the_one_that_decides() {
        let deadlines = Deadlines {
            cursor_blink_at: Some(T0 + 500),
            elapsed_tick_at: Some(T0 + 1_000),
            reconnect_at: Some(T0 + 30_000),
            ..Deadlines::default()
        };
        assert_eq!(
            deadlines.plan(T0),
            RepaintPlan::After(Duration::from_millis(500))
        );
    }

    /// A deadline that has already passed means "draw now", not a negative wait.
    #[test]
    fn a_deadline_in_the_past_becomes_an_immediate_frame_rather_than_a_negative_wait() {
        let deadlines = Deadlines {
            elapsed_tick_at: Some(T0 - 5_000),
            ..Deadlines::default()
        };
        assert_eq!(deadlines.plan(T0), RepaintPlan::After(Duration::ZERO));
    }

    /// The ceiling exists so a mistake in a deadline degrades to a frame a second
    /// rather than to a window that never draws again.
    #[test]
    fn a_deadline_far_in_the_future_is_capped_rather_than_waited_out() {
        let deadlines = Deadlines {
            reconnect_at: Some(T0 + 3_600_000),
            ..Deadlines::default()
        };
        assert_eq!(deadlines.plan(T0), RepaintPlan::After(MAX_IDLE));
        assert!(!deadlines.plan(T0).is_idle());
    }

    #[test]
    fn a_cursor_blinks_on_and_off_and_comes_back() {
        let period = CURSOR_BLINK.as_millis() as i64;
        // Anchored on a phase boundary so the two halves are unambiguous.
        let start = T0 - T0.rem_euclid(period * 2);
        assert!(cursor_visible(start, true));
        assert!(cursor_visible(start + period - 1, true));
        assert!(!cursor_visible(start + period, true));
        assert!(!cursor_visible(start + period * 2 - 1, true));
        assert!(cursor_visible(start + period * 2, true));
    }

    /// A pane that is not focused shows a steady cursor: a grid of thirty blinking
    /// cursors would be both distracting and a repaint every half second per pane.
    #[test]
    fn a_cursor_that_does_not_blink_is_always_visible() {
        for offset in [0, 100, 600, 1_500, 10_000] {
            assert!(cursor_visible(T0 + offset, false));
        }
    }

    #[test]
    fn the_next_cursor_phase_is_always_ahead_and_within_one_period() {
        let period = CURSOR_BLINK.as_millis() as i64;
        for offset in [0, 1, 100, 529, 530, 1_059] {
            let next = next_cursor_phase(T0 + offset);
            assert!(next > T0 + offset, "a deadline must be in the future");
            assert!(next - (T0 + offset) <= period);
            assert_ne!(
                cursor_visible(T0 + offset, true),
                cursor_visible(next, true),
                "the deadline must be the moment the phase actually changes"
            );
        }
    }

    #[test]
    fn an_elapsed_counter_is_woken_on_the_second_and_not_more_often() {
        assert_eq!(next_elapsed_tick(T0), T0 + 1_000);
        assert_eq!(next_elapsed_tick(T0 + 1), T0 + 1_000);
        assert_eq!(next_elapsed_tick(T0 + 999), T0 + 1_000);
        assert_eq!(next_elapsed_tick(T0 + 1_000), T0 + 2_000);
        for offset in [0, 1, 250, 999, 1_000] {
            let next = next_elapsed_tick(T0 + offset);
            assert!(next > T0 + offset);
            assert!(next - (T0 + offset) <= 1_000);
        }
    }

    /// A blinking cursor alone is not enough to make the window busy: it asks for one
    /// frame per phase, which is under two a second, not sixty.
    #[test]
    fn a_blinking_cursor_costs_one_frame_per_phase_rather_than_a_continuous_repaint() {
        let deadlines = Deadlines {
            cursor_blink_at: Some(next_cursor_phase(T0)),
            ..Deadlines::default()
        };
        match deadlines.plan(T0) {
            RepaintPlan::After(delay) => {
                assert!(delay > Duration::from_millis(1), "got {delay:?}");
                assert!(delay <= CURSOR_BLINK);
            }
            other => panic!("a visible cursor has a deadline: {other:?}"),
        }
    }
}
