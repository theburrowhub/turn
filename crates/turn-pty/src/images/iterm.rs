//! The iTerm2 protocol: `ESC ] 1337 ; File = <args> : <base64> ST`.
//!
//! The one most tools target, because it is the one `imgcat` uses and because the payload
//! is an ordinary image file rather than a format of its own. This module is only the
//! argument grammar; the framing is [`super::scan`] and the decoding is
//! [`super::decode`].
//!
//! ## `inline=1`, and what the other value means
//!
//! `inline=1` means "show this in the terminal". Its absence means **"save this to a file
//! in the user's download directory"**, which is a program asking the terminal to write to
//! the filesystem on its behalf. Turn refuses that, and refuses it visibly rather than
//! silently: it is the same class of request as OSC 52's "put this on the clipboard" and
//! "hand me the clipboard", both of which this crate already counts and refuses. A process
//! writing to its own terminal must not be able to write to the user's disk.
//!
//! ## `name`
//!
//! Base64 of the original filename. Kept only as a label — for the accessibility tree and
//! for the refusal notice — and it goes through [`crate::sanitise_label`] first, because it
//! is a process-supplied string that would otherwise reach Turn's chrome.

use super::layout::{BoxRequest, SizeSpec};

/// Longest filename kept from a `name` argument.
///
/// A label, not a payload. Long enough for a real filename and short enough that a hostile
/// one cannot become a memory cost per picture.
pub const MAX_NAME_CHARS: usize = 120;

/// What an `OSC 1337 File=` header asked for.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FileArgs {
    /// Whether the program asked for the picture to be shown. False means it asked for a
    /// download, which Turn refuses.
    pub inline: bool,
    pub size: BoxRequest,
    /// The original filename, sanitised and capped, when the program supplied one.
    pub name: Option<String>,
}

/// Parses the argument list between `File=` and the `:` that starts the payload.
///
/// Unknown arguments are ignored rather than refused: the protocol has grown fields over
/// the years — `type`, `doNotMoveCursor`, `size` — and a terminal that refused a picture
/// because it did not recognise a hint would be worse than one that showed it.
pub fn parse_args(args: &str) -> FileArgs {
    let mut out = FileArgs {
        inline: false,
        size: BoxRequest::default(),
        name: None,
    };
    for field in args.split(';') {
        let (key, value) = match field.split_once('=') {
            Some(pair) => pair,
            // A bare word carries nothing; `File=` itself arrives as one when there are no
            // arguments at all.
            None => continue,
        };
        match key.trim() {
            "inline" => out.inline = value.trim() == "1",
            "width" => out.size.width = parse_size(value.trim()),
            "height" => out.size.height = parse_size(value.trim()),
            "preserveAspectRatio" => out.size.preserve_aspect = value.trim() != "0",
            "name" => out.name = decode_name(value.trim()),
            _ => {}
        }
    }
    out
}

/// One of `N`, `Npx`, `N%` or `auto`.
///
/// Anything else is `auto`, which is the protocol's own behaviour for a value it cannot
/// read: the picture is shown at its natural size rather than not shown.
fn parse_size(value: &str) -> SizeSpec {
    if value.is_empty() || value.eq_ignore_ascii_case("auto") {
        return SizeSpec::Auto;
    }
    if let Some(digits) = value.strip_suffix('%') {
        return match digits.parse::<u32>() {
            Ok(percent) => SizeSpec::Percent(percent),
            Err(_) => SizeSpec::Auto,
        };
    }
    if let Some(digits) = value
        .strip_suffix("px")
        .or_else(|| value.strip_suffix("PX"))
    {
        return match digits.parse::<u32>() {
            Ok(pixels) => SizeSpec::Pixels(pixels),
            Err(_) => SizeSpec::Auto,
        };
    }
    match value.parse::<u32>() {
        Ok(cells) => SizeSpec::Cells(cells),
        Err(_) => SizeSpec::Auto,
    }
}

/// The filename, base64-decoded then sanitised.
///
/// Sanitised because it is a process-supplied string bound for a label, and a label must
/// not be able to render as something other than itself — the same rule, and the same
/// function, as a window title.
fn decode_name(encoded: &str) -> Option<String> {
    let bytes = turn_proto::decode_base64(encoded).ok()?;
    crate::sanitise_label(&String::from_utf8_lossy(&bytes), MAX_NAME_CHARS)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `imgcat` actually sends.
    #[test]
    fn the_header_imgcat_sends_asks_for_an_inline_picture() {
        let args = parse_args("inline=1;size=1234");
        assert!(args.inline);
        assert_eq!(args.size, BoxRequest::default());
        assert_eq!(args.name, None);
    }

    #[test]
    fn a_size_can_be_cells_pixels_a_percentage_or_automatic() {
        let args = parse_args("inline=1;width=40;height=200px");
        assert_eq!(args.size.width, SizeSpec::Cells(40));
        assert_eq!(args.size.height, SizeSpec::Pixels(200));

        let relative = parse_args("inline=1;width=50%;height=auto");
        assert_eq!(relative.size.width, SizeSpec::Percent(50));
        assert_eq!(relative.size.height, SizeSpec::Auto);
    }

    #[test]
    fn aspect_ratio_is_preserved_unless_the_program_says_otherwise() {
        assert!(parse_args("inline=1").size.preserve_aspect);
        assert!(
            parse_args("inline=1;preserveAspectRatio=1")
                .size
                .preserve_aspect
        );
        assert!(
            !parse_args("inline=1;preserveAspectRatio=0")
                .size
                .preserve_aspect
        );
    }

    /// A program asking the terminal to write a file on its behalf is refused, and the
    /// refusal starts here: without `inline=1` this is a download request.
    #[test]
    fn a_header_without_inline_is_a_download_request_and_is_not_marked_inline() {
        assert!(!parse_args("size=1234;name=Zm9v").inline);
        assert!(!parse_args("inline=0").inline);
        assert!(!parse_args("").inline);
    }

    #[test]
    fn a_name_is_decoded_and_then_sanitised_like_any_other_label() {
        let encoded = turn_proto::encode_base64(b"plot.png");
        assert_eq!(
            parse_args(&format!("inline=1;name={encoded}"))
                .name
                .as_deref(),
            Some("plot.png")
        );

        // A name that tries to carry escape sequences into Turn's chrome loses them.
        let hostile = turn_proto::encode_base64(b"safe\x1b[2Jname");
        assert_eq!(
            parse_args(&format!("inline=1;name={hostile}"))
                .name
                .as_deref(),
            Some("safename")
        );

        // And one that tries to reverse its own rendering.
        let bidi = turn_proto::encode_base64("\u{202e}gnp.exe".as_bytes());
        let name = parse_args(&format!("inline=1;name={bidi}"))
            .name
            .expect("something legible survives");
        assert!(!name.contains('\u{202e}'), "got {name:?}");

        // A name longer than a label is capped rather than retained per picture.
        let long = turn_proto::encode_base64(&vec![b'a'; 4_096]);
        assert_eq!(
            parse_args(&format!("inline=1;name={long}"))
                .name
                .expect("it survives")
                .chars()
                .count(),
            MAX_NAME_CHARS
        );
    }

    #[test]
    fn a_name_that_is_not_base64_is_dropped_rather_than_shown_as_gibberish() {
        assert_eq!(parse_args("inline=1;name=not base64!").name, None);
        assert_eq!(parse_args("inline=1;name=").name, None);
    }

    /// The protocol has grown arguments over the years and will grow more. An unknown one
    /// must not cost the user their picture.
    #[test]
    fn an_unknown_argument_is_ignored_rather_than_refusing_the_picture() {
        let args = parse_args("inline=1;doNotMoveCursor=1;type=image/png;width=10");
        assert!(args.inline);
        assert_eq!(args.size.width, SizeSpec::Cells(10));
    }

    /// Malformed headers are the ordinary case for a half-written escape sequence, and
    /// none of them may panic or produce a nonsense size.
    #[test]
    fn a_malformed_header_yields_a_natural_size_rather_than_a_nonsense_one() {
        for header in [
            "",
            ";;;;",
            "=",
            "width=",
            "width=abc",
            "width=99999999999999999999",
            "width=-4",
            "height=%",
            "height=px",
            "inline",
            "inline=1;width=10;",
        ] {
            let args = parse_args(header);
            // Whatever it parsed to, it must be a size that resolves to a real box.
            let placed = super::super::layout::resolve(
                args.size,
                (100, 100),
                super::super::layout::Viewport::new(24, 80, (8, 17), 80),
            );
            assert!(
                placed.rows >= 1 && placed.cols >= 1,
                "{header:?} -> {placed:?}"
            );
        }
        assert_eq!(parse_args("width=abc").size.width, SizeSpec::Auto);
        assert_eq!(parse_args("width=-4").size.width, SizeSpec::Auto);
    }
}
