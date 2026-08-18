//! Turning keystrokes into the bytes a pty expects.
//!
//! This is the least glamorous module in the window and the one users notice first
//! when it is wrong: an arrow key that inserts a letter, a Backspace that prints
//! `^H`, an Escape that takes half a second to reach `vim`.
//!
//! Three rules it follows:
//!
//! * **The program's modes decide the encoding.** Arrow keys are `ESC [ A` normally
//!   and `ESC O A` once an application has asked for application cursor mode. The
//!   modes arrive with the grid ([`crate::cells::Modes`]) because the daemon parsed
//!   them; guessing here would break arrows inside every full-screen tool.
//! * **Text is not keys.** A character reaches the pty through
//!   [`encode_text`] from egui's `Text` event, which has already applied the keyboard
//!   layout, dead keys and IME. Deriving `a` from `Key::A` would work on a US
//!   keyboard and fail on every other one.
//! * **A paste cannot escape its own brackets.** [`encode_paste`] strips the
//!   terminator out of the pasted text, because a paste containing `ESC [ 201 ~`
//!   would otherwise end the bracket early and the rest would arrive as commands.

use crate::cells::Modes;
use egui::{Key, Modifiers};

/// The `ESC [ 200 ~` … `ESC [ 201 ~` wrapper an application asks for so it can tell
/// a paste from typing.
const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

/// The parameter that carries modifier state in a CSI sequence.
///
/// `1` is no modifiers, and each modifier adds its own bit, which is the encoding
/// every terminal has used since xterm settled it.
fn modifier_parameter(modifiers: &Modifiers) -> u8 {
    let mut value = 1;
    if modifiers.shift {
        value += 1;
    }
    if modifiers.alt {
        value += 2;
    }
    if modifiers.ctrl {
        value += 4;
    }
    value
}

/// A cursor-style key: `ESC [ x`, or `ESC O x` in application mode, or
/// `ESC [ 1 ; m x` when a modifier is held.
///
/// Modifiers always use the CSI form even in application mode, which is what xterm
/// does and therefore what every application expects.
fn cursor_key(final_byte: u8, modifiers: &Modifiers, modes: &Modes) -> Vec<u8> {
    let parameter = modifier_parameter(modifiers);
    if parameter > 1 {
        return format!("\x1b[1;{parameter}{}", final_byte as char).into_bytes();
    }
    if modes.application_cursor {
        vec![0x1b, b'O', final_byte]
    } else {
        vec![0x1b, b'[', final_byte]
    }
}

/// A `ESC [ n ~` key, with its modifier parameter when one is held.
fn tilde_key(number: u8, modifiers: &Modifiers) -> Vec<u8> {
    let parameter = modifier_parameter(modifiers);
    if parameter > 1 {
        format!("\x1b[{number};{parameter}~").into_bytes()
    } else {
        format!("\x1b[{number}~").into_bytes()
    }
}

/// The control character Control produces with this key, if any.
///
/// The letters map to 1..=26 and the punctuation into the rest of the C0 range. This
/// is the table the keymap's reserved-key list exists to protect: every byte here is
/// one a running program is entitled to receive.
fn control_character(key: Key) -> Option<u8> {
    let byte = match key {
        Key::A => 0x01,
        Key::B => 0x02,
        Key::C => 0x03,
        Key::D => 0x04,
        Key::E => 0x05,
        Key::F => 0x06,
        Key::G => 0x07,
        Key::H => 0x08,
        Key::I => 0x09,
        Key::J => 0x0a,
        Key::K => 0x0b,
        Key::L => 0x0c,
        Key::M => 0x0d,
        Key::N => 0x0e,
        Key::O => 0x0f,
        Key::P => 0x10,
        Key::Q => 0x11,
        Key::R => 0x12,
        Key::S => 0x13,
        Key::T => 0x14,
        Key::U => 0x15,
        Key::V => 0x16,
        Key::W => 0x17,
        Key::X => 0x18,
        Key::Y => 0x19,
        Key::Z => 0x1a,
        Key::OpenBracket => 0x1b,
        Key::Backslash => 0x1c,
        Key::CloseBracket => 0x1d,
        Key::Backtick => 0x1e,
        // Both spellings of the same byte: `Ctrl+/` and `Ctrl+-` are US in practice.
        Key::Slash | Key::Minus => 0x1f,
        Key::Space => 0x00,
        _ => return None,
    };
    Some(byte)
}

/// The bytes a key press sends, or `None` when the key sends nothing of its own.
///
/// `None` is the normal answer for a plain letter: it will arrive as text, through
/// [`encode_text`], with the keyboard layout already applied.
pub fn encode_key(key: Key, modifiers: &Modifiers, modes: &Modes) -> Option<Vec<u8>> {
    // Alt on a key that produces bytes is sent as an ESC prefix, which is how every
    // terminal expresses Meta. Handled first so it composes with everything below.
    let alt_prefix = modifiers.alt && !modifiers.ctrl;

    let bytes = match key {
        Key::Enter
            if modifiers.shift
                && !modifiers.alt
                && !modifiers.ctrl
                && !modifiers.mac_cmd
                && !modifiers.command =>
        {
            // A bare carriage return cannot carry Shift, so sending the ordinary
            // Enter byte here makes an agent submit instead of inserting a line.
            // Meta-Enter is the portable multiline fallback understood by agent
            // composers and survives an ordinary pty without enhanced-key
            // negotiation.
            vec![0x1b, b'\r']
        }
        Key::Enter => {
            // Carriage return, not newline. A pty in canonical mode turns CR into
            // whatever the line discipline wants; sending LF instead makes a shell
            // prompt accept the line but leaves editors inserting a raw newline.
            vec![b'\r']
        }
        Key::Tab if modifiers.shift => b"\x1b[Z".to_vec(),
        Key::Tab => vec![b'\t'],
        // DEL rather than BS. Every modern terminal sends DEL for Backspace, and a
        // shell that receives BS prints `^H` instead of erasing.
        Key::Backspace if modifiers.ctrl => vec![0x08],
        Key::Backspace => vec![0x7f],
        Key::Escape => vec![0x1b],

        Key::ArrowUp => cursor_key(b'A', modifiers, modes),
        Key::ArrowDown => cursor_key(b'B', modifiers, modes),
        Key::ArrowRight => cursor_key(b'C', modifiers, modes),
        Key::ArrowLeft => cursor_key(b'D', modifiers, modes),
        Key::Home => cursor_key(b'H', modifiers, modes),
        Key::End => cursor_key(b'F', modifiers, modes),

        Key::Insert => tilde_key(2, modifiers),
        Key::Delete => tilde_key(3, modifiers),
        Key::PageUp => tilde_key(5, modifiers),
        Key::PageDown => tilde_key(6, modifiers),

        // F1 to F4 are SS3 sequences; F5 upwards are CSI with a number.
        Key::F1 => function_key(b'P', 11, modifiers),
        Key::F2 => function_key(b'Q', 12, modifiers),
        Key::F3 => function_key(b'R', 13, modifiers),
        Key::F4 => function_key(b'S', 14, modifiers),
        Key::F5 => tilde_key(15, modifiers),
        Key::F6 => tilde_key(17, modifiers),
        Key::F7 => tilde_key(18, modifiers),
        Key::F8 => tilde_key(19, modifiers),
        Key::F9 => tilde_key(20, modifiers),
        Key::F10 => tilde_key(21, modifiers),
        Key::F11 => tilde_key(23, modifiers),
        Key::F12 => tilde_key(24, modifiers),

        other if modifiers.ctrl => {
            // A control character, which egui will not deliver as text.
            vec![control_character(other)?]
        }
        // Everything else arrives as text, with the layout already applied.
        _ => return None,
    };

    // Only the keys whose sequence has no modifier parameter of its own take the
    // prefix. An arrow with Alt held is already `ESC [ 1 ; 3 A`, and adding an ESC in
    // front of that would send Meta *and* Alt — which readline reads as two separate
    // keystrokes, so `Alt+Up` would move the cursor and then insert.
    if alt_prefix && !encodes_modifiers_itself(key) {
        let mut prefixed = Vec::with_capacity(bytes.len() + 1);
        prefixed.push(0x1b);
        prefixed.extend_from_slice(&bytes);
        return Some(prefixed);
    }
    Some(bytes)
}

/// Whether this key's sequence carries its own modifier parameter.
///
/// These are the CSI and SS3 keys, where Alt arrives inside the sequence. Everything
/// else — Enter, Tab, Backspace, Escape, a control character — is a bare byte, and Alt
/// on one of those is expressed as an ESC in front of it.
fn encodes_modifiers_itself(key: Key) -> bool {
    matches!(
        key,
        Key::ArrowUp
            | Key::ArrowDown
            | Key::ArrowLeft
            | Key::ArrowRight
            | Key::Home
            | Key::End
            | Key::Insert
            | Key::Delete
            | Key::PageUp
            | Key::PageDown
            | Key::F1
            | Key::F2
            | Key::F3
            | Key::F4
            | Key::F5
            | Key::F6
            | Key::F7
            | Key::F8
            | Key::F9
            | Key::F10
            | Key::F11
            | Key::F12
    )
}

/// F1 to F4: `ESC O x` plain, and the CSI form once a modifier is held.
fn function_key(ss3: u8, csi_number: u8, modifiers: &Modifiers) -> Vec<u8> {
    let parameter = modifier_parameter(modifiers);
    if parameter > 1 {
        format!("\x1b[{csi_number};{parameter}~").into_bytes()
    } else {
        vec![0x1b, b'O', ss3]
    }
}

/// Typed text, as bytes.
///
/// egui has already resolved the keyboard layout, dead keys and any IME composition,
/// so this really is only an encoding step. It exists as a function so the one thing
/// worth deciding is decided in one place: text arrives as UTF-8 and is not
/// translated, because a terminal is a byte stream and the program on the other end
/// owns the interpretation.
pub fn encode_text(text: &str) -> Vec<u8> {
    text.as_bytes().to_vec()
}

/// Pasted text, wrapped if the program asked for bracketed paste.
///
/// The terminator is removed from the pasted text rather than escaped, because there
/// is no escaping in this protocol: a paste containing `ESC [ 201 ~` would end the
/// bracket early, and everything after it would arrive as though the user had typed
/// it. Newlines become carriage returns for the same reason `Enter` does.
pub fn encode_paste(text: &str, modes: &Modes) -> Vec<u8> {
    let cleaned = text.replace("\r\n", "\r").replace('\n', "\r");
    let cleaned = strip_all(&cleaned, "\x1b[201~");
    if !modes.bracketed_paste {
        return cleaned.into_bytes();
    }
    let mut out = Vec::with_capacity(cleaned.len() + PASTE_START.len() + PASTE_END.len());
    out.extend_from_slice(PASTE_START);
    out.extend_from_slice(cleaned.as_bytes());
    out.extend_from_slice(PASTE_END);
    out
}

/// Removes every occurrence, including ones that only appear once earlier ones are
/// gone — `ESC [ 2ESC [ 201 ~01 ~` must not reassemble itself.
fn strip_all(text: &str, needle: &str) -> String {
    let mut out = text.to_string();
    while out.contains(needle) {
        out = out.replace(needle, "");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain() -> Modifiers {
        Modifiers::default()
    }

    fn ctrl() -> Modifiers {
        Modifiers {
            ctrl: true,
            command: true,
            ..Modifiers::default()
        }
    }

    fn alt() -> Modifiers {
        Modifiers {
            alt: true,
            ..Modifiers::default()
        }
    }

    fn shift() -> Modifiers {
        Modifiers {
            shift: true,
            ..Modifiers::default()
        }
    }

    fn normal() -> Modes {
        Modes::default()
    }

    fn application() -> Modes {
        Modes {
            application_cursor: true,
            ..Modes::default()
        }
    }

    #[test]
    fn the_keys_every_shell_needs_send_the_bytes_every_shell_expects() {
        assert_eq!(
            encode_key(Key::Enter, &plain(), &normal()),
            Some(vec![b'\r'])
        );
        assert_eq!(encode_key(Key::Tab, &plain(), &normal()), Some(vec![b'\t']));
        assert_eq!(
            encode_key(Key::Escape, &plain(), &normal()),
            Some(vec![0x1b])
        );
        assert_eq!(
            encode_key(Key::Backspace, &plain(), &normal()),
            Some(vec![0x7f]),
            "Backspace is DEL; sending BS makes a shell print ^H instead of erasing"
        );
        assert_eq!(
            encode_key(Key::Tab, &shift(), &normal()),
            Some(b"\x1b[Z".to_vec()),
            "Shift+Tab is the back-tab sequence, which every completion menu reads"
        );
    }

    #[test]
    fn shift_enter_uses_the_portable_multiline_sequence() {
        assert_eq!(
            encode_key(Key::Enter, &shift(), &normal()),
            Some(b"\x1b\r".to_vec()),
            "Shift+Enter must remain distinct from the carriage return that submits"
        );

        let alt_shift = Modifiers {
            alt: true,
            shift: true,
            ..Modifiers::default()
        };
        assert_eq!(
            encode_key(Key::Enter, &alt_shift, &normal()),
            Some(b"\x1b\r".to_vec()),
            "composing Shift with Alt must not add a second escape"
        );
    }

    /// The whole point of carrying the modes with the grid: in application cursor
    /// mode an arrow is an SS3 sequence, and sending the CSI form makes arrows insert
    /// letters inside a full-screen tool.
    #[test]
    fn arrow_keys_follow_the_mode_the_program_asked_for() {
        assert_eq!(
            encode_key(Key::ArrowUp, &plain(), &normal()),
            Some(b"\x1b[A".to_vec())
        );
        assert_eq!(
            encode_key(Key::ArrowUp, &plain(), &application()),
            Some(b"\x1bOA".to_vec())
        );
        for (key, csi, ss3) in [
            (Key::ArrowDown, "\x1b[B", "\x1bOB"),
            (Key::ArrowRight, "\x1b[C", "\x1bOC"),
            (Key::ArrowLeft, "\x1b[D", "\x1bOD"),
            (Key::Home, "\x1b[H", "\x1bOH"),
            (Key::End, "\x1b[F", "\x1bOF"),
        ] {
            assert_eq!(
                encode_key(key, &plain(), &normal()),
                Some(csi.as_bytes().to_vec()),
                "{key:?} in normal mode"
            );
            assert_eq!(
                encode_key(key, &plain(), &application()),
                Some(ss3.as_bytes().to_vec()),
                "{key:?} in application mode"
            );
        }
    }

    /// A modified arrow always uses the CSI form, even in application mode, which is
    /// what xterm does and therefore what every program was written against.
    #[test]
    fn a_modified_arrow_uses_the_csi_form_in_either_mode() {
        assert_eq!(
            encode_key(Key::ArrowRight, &ctrl(), &normal()),
            Some(b"\x1b[1;5C".to_vec()),
            "Ctrl+Right is word-forward in readline"
        );
        assert_eq!(
            encode_key(Key::ArrowRight, &ctrl(), &application()),
            Some(b"\x1b[1;5C".to_vec()),
            "application mode must not change the modified form"
        );
        assert_eq!(
            encode_key(Key::ArrowLeft, &shift(), &normal()),
            Some(b"\x1b[1;2D".to_vec())
        );
        assert_eq!(
            encode_key(Key::ArrowUp, &alt(), &normal()),
            Some(b"\x1b[1;3A".to_vec()),
            "Alt is in the parameter; prefixing an ESC as well would send two keystrokes"
        );
        for key in [
            Key::ArrowUp,
            Key::Home,
            Key::End,
            Key::PageUp,
            Key::Delete,
            Key::F5,
        ] {
            let bytes = encode_key(key, &alt(), &normal())
                .unwrap_or_else(|| panic!("{key:?} must send something"));
            assert!(
                !bytes.starts_with(b"\x1b\x1b"),
                "{key:?} with Alt was double-escaped: {bytes:?}"
            );
        }
    }

    /// Every control character the keymap protects has to actually be produced here,
    /// or protecting it in the keymap was pointless.
    #[test]
    fn every_control_character_the_keymap_protects_is_produced_here() {
        for key in crate::keymap::CONTROL_CHARACTER_KEYS {
            let bytes = encode_key(*key, &ctrl(), &normal())
                .unwrap_or_else(|| panic!("Ctrl+{} produced nothing", key.name()));
            assert_eq!(
                bytes.len(),
                1,
                "Ctrl+{} must be one byte, got {bytes:?}",
                key.name()
            );
            assert!(
                bytes[0] < 0x20,
                "Ctrl+{} must land in the C0 range, got {:#04x}",
                key.name(),
                bytes[0]
            );
        }
    }

    #[test]
    fn the_control_characters_a_user_would_notice_are_the_right_bytes() {
        assert_eq!(encode_key(Key::C, &ctrl(), &normal()), Some(vec![0x03]));
        assert_eq!(encode_key(Key::D, &ctrl(), &normal()), Some(vec![0x04]));
        assert_eq!(
            encode_key(Key::OpenBracket, &ctrl(), &normal()),
            Some(vec![0x1b]),
            "Ctrl+[ is ESC, which is how vi-mode leaves insert"
        );
        assert_eq!(encode_key(Key::Space, &ctrl(), &normal()), Some(vec![0x00]));
        assert_eq!(encode_key(Key::Slash, &ctrl(), &normal()), Some(vec![0x1f]));
    }

    #[test]
    fn alt_is_sent_as_an_escape_prefix_which_is_how_meta_is_expressed() {
        assert_eq!(
            encode_key(Key::Enter, &alt(), &normal()),
            Some(vec![0x1b, b'\r'])
        );
        assert_eq!(
            encode_key(Key::Backspace, &alt(), &normal()),
            Some(vec![0x1b, 0x7f]),
            "Alt+Backspace is delete-previous-word"
        );
    }

    #[test]
    fn the_function_keys_use_ss3_below_five_and_csi_above_it() {
        assert_eq!(
            encode_key(Key::F1, &plain(), &normal()),
            Some(b"\x1bOP".to_vec())
        );
        assert_eq!(
            encode_key(Key::F4, &plain(), &normal()),
            Some(b"\x1bOS".to_vec())
        );
        assert_eq!(
            encode_key(Key::F5, &plain(), &normal()),
            Some(b"\x1b[15~".to_vec())
        );
        assert_eq!(
            encode_key(Key::F12, &plain(), &normal()),
            Some(b"\x1b[24~".to_vec())
        );
        assert_eq!(
            encode_key(Key::F1, &shift(), &normal()),
            Some(b"\x1b[11;2~".to_vec()),
            "a modified F1 has no SS3 form"
        );
    }

    #[test]
    fn the_navigation_keys_use_their_numbered_sequences() {
        for (key, expected) in [
            (Key::Insert, "\x1b[2~"),
            (Key::Delete, "\x1b[3~"),
            (Key::PageUp, "\x1b[5~"),
            (Key::PageDown, "\x1b[6~"),
        ] {
            assert_eq!(
                encode_key(key, &plain(), &normal()),
                Some(expected.as_bytes().to_vec()),
                "{key:?}"
            );
        }
    }

    /// A plain letter is not encoded here at all. Deriving `a` from `Key::A` would
    /// work on a US keyboard and produce the wrong character on every other layout.
    #[test]
    fn a_plain_letter_is_left_to_arrive_as_text_with_its_layout_applied() {
        assert_eq!(encode_key(Key::A, &plain(), &normal()), None);
        assert_eq!(encode_key(Key::Num1, &plain(), &normal()), None);
        assert_eq!(encode_key(Key::Slash, &plain(), &normal()), None);
        assert_eq!(encode_key(Key::A, &shift(), &normal()), None);
        // And text goes through as the bytes it is.
        assert_eq!(encode_text("ä"), "ä".as_bytes().to_vec());
        assert_eq!(encode_text("漢"), "漢".as_bytes().to_vec());
    }

    #[test]
    fn a_paste_is_bracketed_only_when_the_program_asked_for_it() {
        let bracketed = Modes {
            bracketed_paste: true,
            ..Modes::default()
        };
        assert_eq!(encode_paste("hello", &normal()), b"hello".to_vec());
        assert_eq!(
            encode_paste("hello", &bracketed),
            b"\x1b[200~hello\x1b[201~".to_vec()
        );
    }

    #[test]
    fn a_pasted_newline_becomes_a_carriage_return_like_the_enter_key() {
        assert_eq!(encode_paste("a\nb", &normal()), b"a\rb".to_vec());
        assert_eq!(encode_paste("a\r\nb", &normal()), b"a\rb".to_vec());
    }

    /// A paste cannot be allowed to close its own bracket. Otherwise text copied off
    /// a web page could end the paste early and have the rest treated as typing —
    /// which, at a shell prompt, means running it.
    #[test]
    fn a_paste_cannot_escape_its_own_brackets() {
        let bracketed = Modes {
            bracketed_paste: true,
            ..Modes::default()
        };
        let hostile = "safe\x1b[201~rm -rf /\r";
        let bytes = encode_paste(hostile, &bracketed);
        let text = String::from_utf8_lossy(&bytes);
        assert_eq!(
            text.matches("\x1b[201~").count(),
            1,
            "only the terminator this function added may be present: {text:?}"
        );
        assert!(text.ends_with("\x1b[201~"));
        assert!(text.contains("rm -rf /"), "the text itself is still pasted");

        // And a terminator that only appears once an earlier one is removed must not
        // survive either.
        let nested = "a\x1b[2\x1b[201~01~b";
        let bytes = encode_paste(nested, &bracketed);
        let text = String::from_utf8_lossy(&bytes);
        assert_eq!(text.matches("\x1b[201~").count(), 1, "{text:?}");
    }
}
