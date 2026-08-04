//! Mouse reporting: telling the program in the pane where the pointer is.
//!
//! Only when it asked. A program that has not enabled mouse tracking must never
//! receive these sequences — at a shell prompt they would arrive as typed garbage,
//! and the user would see `[<0;12;4M` appear in their command line. That is why every
//! function here takes the [`MouseMode`] from the grid and answers `None` when the
//! mode says the program is not listening.
//!
//! The SGR encoding (`ESC [ < b ; x ; y M`) is used rather than the original X10 one.
//! X10 encodes a coordinate as a single byte offset by 32, so it cannot express a
//! column past 223 — and a maximised terminal on a wide display is routinely wider
//! than that. Every terminal emulator and every mouse-aware program has supported SGR
//! for over a decade.

use crate::cells::MouseMode;

/// Which button an event is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
}

impl Button {
    /// The button's own code, before modifiers are added.
    fn code(self) -> u32 {
        match self {
            Button::Left => 0,
            Button::Middle => 1,
            Button::Right => 2,
            // The wheel is reported as buttons 64 and 65 rather than as a button
            // press, which is why a wheel event has no release.
            Button::WheelUp => 64,
            Button::WheelDown => 65,
        }
    }

    fn is_wheel(self) -> bool {
        matches!(self, Button::WheelUp | Button::WheelDown)
    }
}

/// What happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseEvent {
    Press(Button),
    Release(Button),
    /// The pointer moved with a button held.
    Drag(Button),
    /// The pointer moved with nothing held.
    Move,
    /// A wheel notch. Reported as a press with no matching release.
    Wheel(Button),
}

/// Modifier state, as the mouse protocol carries it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MouseModifiers {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
}

impl MouseModifiers {
    fn bits(&self) -> u32 {
        let mut value = 0;
        if self.shift {
            value += 4;
        }
        if self.alt {
            value += 8;
        }
        if self.ctrl {
            value += 16;
        }
        value
    }
}

/// The motion bit, set for any event that reports movement rather than a click.
const MOTION: u32 = 32;

/// The bytes to send for a mouse event at a cell, or `None` when the program is not
/// listening for it.
///
/// `row` and `col` are zero-based cell coordinates; the protocol is one-based, and
/// converting in one place is what stops an off-by-one making every click land a
/// column to the left.
pub fn encode(
    event: MouseEvent,
    row: u16,
    col: u16,
    modifiers: MouseModifiers,
    mode: MouseMode,
) -> Option<Vec<u8>> {
    if !mode.reports() {
        return None;
    }
    let (button, motion, release) = match event {
        MouseEvent::Press(button) if button.is_wheel() => (button, false, false),
        MouseEvent::Press(button) => (button, false, false),
        MouseEvent::Wheel(button) => (button, false, false),
        MouseEvent::Release(button) => {
            if button.is_wheel() {
                // The wheel has no release. Sending one would look to a program like
                // an extra scroll in the other direction.
                return None;
            }
            (button, false, true)
        }
        MouseEvent::Drag(button) => {
            if !mode.reports_drag() {
                return None;
            }
            (button, true, false)
        }
        MouseEvent::Move => {
            if !mode.reports_hover() {
                return None;
            }
            // Motion with no button held is reported as button 3 — "no button" — plus
            // the motion bit.
            let code = 3 + MOTION + modifiers.bits();
            return Some(
                format!("\x1b[<{code};{};{}M", col as u32 + 1, row as u32 + 1).into_bytes(),
            );
        }
    };

    let mut code = button.code() + modifiers.bits();
    if motion {
        code += MOTION;
    }
    let final_byte = if release { 'm' } else { 'M' };
    Some(
        format!(
            "\x1b[<{code};{};{}{final_byte}",
            col as u32 + 1,
            row as u32 + 1
        )
        .into_bytes(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none() -> MouseModifiers {
        MouseModifiers::default()
    }

    /// The rule that keeps a shell prompt usable: a program that did not ask for the
    /// mouse never receives a single byte of it.
    #[test]
    fn nothing_is_reported_to_a_program_that_did_not_ask_for_the_mouse() {
        for event in [
            MouseEvent::Press(Button::Left),
            MouseEvent::Release(Button::Left),
            MouseEvent::Drag(Button::Left),
            MouseEvent::Move,
            MouseEvent::Wheel(Button::WheelUp),
        ] {
            assert_eq!(
                encode(event, 3, 9, none(), MouseMode::None),
                None,
                "{event:?} must not reach a program that is not listening"
            );
        }
    }

    #[test]
    fn a_click_is_reported_with_one_based_coordinates() {
        assert_eq!(
            encode(
                MouseEvent::Press(Button::Left),
                0,
                0,
                none(),
                MouseMode::Press
            ),
            Some(b"\x1b[<0;1;1M".to_vec()),
            "the top left cell is column one, row one"
        );
        assert_eq!(
            encode(
                MouseEvent::Press(Button::Left),
                23,
                79,
                none(),
                MouseMode::Press
            ),
            Some(b"\x1b[<0;80;24M".to_vec())
        );
    }

    /// The reason SGR is used at all: a column past 223 cannot be expressed in the
    /// original encoding, and a maximised terminal is routinely wider than that.
    #[test]
    fn a_column_past_the_old_limit_is_still_reported_correctly() {
        let bytes = encode(
            MouseEvent::Press(Button::Left),
            10,
            299,
            none(),
            MouseMode::Press,
        )
        .expect("a wide terminal still reports");
        assert_eq!(String::from_utf8_lossy(&bytes), "\x1b[<0;300;11M");
    }

    #[test]
    fn the_three_buttons_have_their_own_codes_and_a_release_is_a_lower_case_m() {
        assert_eq!(
            encode(
                MouseEvent::Press(Button::Middle),
                0,
                0,
                none(),
                MouseMode::Press
            ),
            Some(b"\x1b[<1;1;1M".to_vec())
        );
        assert_eq!(
            encode(
                MouseEvent::Press(Button::Right),
                0,
                0,
                none(),
                MouseMode::Press
            ),
            Some(b"\x1b[<2;1;1M".to_vec())
        );
        assert_eq!(
            encode(
                MouseEvent::Release(Button::Left),
                0,
                0,
                none(),
                MouseMode::Press
            ),
            Some(b"\x1b[<0;1;1m".to_vec())
        );
    }

    #[test]
    fn modifiers_ride_along_with_the_button() {
        let with_ctrl = MouseModifiers {
            ctrl: true,
            ..none()
        };
        assert_eq!(
            encode(
                MouseEvent::Press(Button::Left),
                0,
                0,
                with_ctrl,
                MouseMode::Press
            ),
            Some(b"\x1b[<16;1;1M".to_vec())
        );
        let all = MouseModifiers {
            shift: true,
            alt: true,
            ctrl: true,
        };
        assert_eq!(
            encode(MouseEvent::Press(Button::Left), 0, 0, all, MouseMode::Press),
            Some(b"\x1b[<28;1;1M".to_vec())
        );
    }

    #[test]
    fn the_wheel_is_reported_as_its_own_buttons() {
        assert_eq!(
            encode(
                MouseEvent::Wheel(Button::WheelUp),
                4,
                4,
                none(),
                MouseMode::Press
            ),
            Some(b"\x1b[<64;5;5M".to_vec())
        );
        assert_eq!(
            encode(
                MouseEvent::Wheel(Button::WheelDown),
                4,
                4,
                none(),
                MouseMode::Press
            ),
            Some(b"\x1b[<65;5;5M".to_vec())
        );
    }

    /// A wheel notch has no release. Sending one would read to a program as an extra
    /// scroll in the other direction, so a list would jitter on every notch.
    #[test]
    fn a_wheel_notch_has_no_release_to_send() {
        assert_eq!(
            encode(
                MouseEvent::Release(Button::WheelUp),
                0,
                0,
                none(),
                MouseMode::AnyMotion
            ),
            None
        );
    }

    /// The three tracking modes differ only in which motion they want, and reporting
    /// motion to a mode that did not ask for it floods the pty.
    #[test]
    fn each_tracking_mode_gets_only_the_motion_it_asked_for() {
        let drag = MouseEvent::Drag(Button::Left);
        assert_eq!(
            encode(drag, 1, 1, none(), MouseMode::Press),
            None,
            "button-only tracking does not want drags"
        );
        assert_eq!(
            encode(drag, 1, 1, none(), MouseMode::ButtonMotion),
            Some(b"\x1b[<32;2;2M".to_vec()),
            "a drag sets the motion bit on top of the button"
        );
        assert_eq!(
            encode(drag, 1, 1, none(), MouseMode::AnyMotion),
            Some(b"\x1b[<32;2;2M".to_vec())
        );

        assert_eq!(
            encode(MouseEvent::Move, 1, 1, none(), MouseMode::ButtonMotion),
            None,
            "hovering with no button held is only wanted by any-motion tracking"
        );
        assert_eq!(
            encode(MouseEvent::Move, 1, 1, none(), MouseMode::AnyMotion),
            Some(b"\x1b[<35;2;2M".to_vec()),
            "motion with no button is button three plus the motion bit"
        );
    }

    #[test]
    fn a_press_is_always_wanted_by_every_mode_that_reports_at_all() {
        for mode in [
            MouseMode::Press,
            MouseMode::ButtonMotion,
            MouseMode::AnyMotion,
        ] {
            assert!(
                encode(MouseEvent::Press(Button::Left), 0, 0, none(), mode).is_some(),
                "{mode:?} must hear about a click"
            );
        }
    }
}
