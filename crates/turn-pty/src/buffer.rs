//! Terminal buffers: raw bytes for replay, a parsed screen for everything else.
//!
//! Turn keeps two representations of every terminal, and both earn their keep:
//!
//! * A **byte ring** the UI replays to rebuild a terminal exactly as it was when
//!   a pane is re-attached. Bytes, not text, because escape sequences carry the
//!   colours, cursor position and alternate-screen state.
//! * A **parsed screen** ([`vt100`]) so the daemon can answer "what does this
//!   look like right now" without a UI attached. Attached cell renderers,
//!   on-demand previews and output heuristics all depend on this.
//!
//! Both are bounded. An unbounded scrollback is a memory leak with a nice name.

use std::collections::VecDeque;
use std::iter::Peekable;
use std::str::Chars;

/// Default cap on retained raw bytes per pane, ~2 MiB.
pub const DEFAULT_BYTE_CAPACITY: usize = 2 * 1024 * 1024;
/// Default scrollback rows kept by the parser.
pub const DEFAULT_SCROLLBACK_ROWS: usize = 5_000;

/// Terminal dimensions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenSize {
    pub rows: u16,
    pub cols: u16,
}

impl Default for ScreenSize {
    fn default() -> Self {
        Self { rows: 24, cols: 80 }
    }
}

impl ScreenSize {
    pub fn new(rows: u16, cols: u16) -> Self {
        // A zero dimension makes the parser panic and makes no sense to the
        // kernel either, so clamp rather than propagate nonsense.
        Self {
            rows: rows.max(1),
            cols: cols.max(1),
        }
    }
}

/// A point-in-time view of a terminal, cheap enough for an on-demand preview and
/// small enough not to matter if it is dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenSnapshot {
    /// Visible rows, top to bottom, trailing blanks trimmed, and filtered through
    /// [`is_display_safe`] so a row cannot misrepresent itself in product chrome.
    pub lines: Vec<String>,
    pub cursor: (u16, u16),
    pub size: ScreenSize,
    /// Whether a full-screen application (vim, lazygit, fang) is in control.
    pub alternate_screen: bool,
    /// Title set by the process via OSC, already sanitised.
    pub title: Option<String>,
    /// Total bytes ever written, for staleness checks.
    pub bytes_seen: u64,
}

impl ScreenSnapshot {
    /// The last `n` non-empty lines, used by bounded previews and heuristics.
    pub fn tail(&self, n: usize) -> Vec<String> {
        let non_empty: Vec<&String> = self.lines.iter().filter(|l| !l.trim().is_empty()).collect();
        non_empty
            .iter()
            .rev()
            .take(n)
            .rev()
            .map(|s| s.to_string())
            .collect()
    }

    /// Flattened text, for pattern matching by the heuristic adapters.
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }
}

/// Side-channel escape sequences the parser hands us instead of drawing.
///
/// Three of these matter for security. A process-set **title** ends up in Turn's
/// sidebar, **OSC 52 with a payload** lets a process write to the system
/// clipboard, and **OSC 52 with `?`** asks the terminal to hand the clipboard
/// *back* to the process. None of them is acted on: the title is sanitised and
/// bounded the moment it arrives, and both clipboard directions are counted and
/// refused. The refusals are written out explicitly rather than left to
/// [`vt100::Callbacks`]'s empty defaults, because a dependency bump that gave one
/// of those defaults a real implementation would silently turn an agent's stdout
/// into clipboard access.
#[derive(Debug, Default)]
pub struct TerminalCallbacks {
    title: Option<String>,
    icon_name: Option<String>,
    bells: u32,
    /// Clipboard writes requested by the process, all refused.
    blocked_clipboard_writes: u32,
    /// Clipboard *reads* requested by the process, all refused.
    blocked_clipboard_reads: u32,
    /// Resizes the process asked the window manager for, all refused.
    blocked_resizes: u32,
}

impl vt100::Callbacks for TerminalCallbacks {
    fn set_window_title(&mut self, _: &mut vt100::Screen, title: &[u8]) {
        // Sanitised and capped here rather than in `snapshot`, because what
        // arrives is an OSC payload the parser buffers without a bound: an agent
        // can emit a megabyte-long title, and retaining it per pane would be a
        // memory cost the process controls.
        self.title = sanitise_label(&String::from_utf8_lossy(title), MAX_TITLE_CHARS);
    }

    fn set_window_icon_name(&mut self, _: &mut vt100::Screen, icon_name: &[u8]) {
        self.icon_name = sanitise_label(&String::from_utf8_lossy(icon_name), MAX_TITLE_CHARS);
    }

    fn audible_bell(&mut self, _: &mut vt100::Screen) {
        self.bells = self.bells.saturating_add(1);
    }

    fn copy_to_clipboard(&mut self, _: &mut vt100::Screen, _ty: &[u8], _data: &[u8]) {
        // Deliberately ignored. Recorded so the UI can tell the user a process
        // tried, which is useful signal rather than something to hide.
        self.blocked_clipboard_writes = self.blocked_clipboard_writes.saturating_add(1);
    }

    fn paste_from_clipboard(&mut self, _: &mut vt100::Screen, _ty: &[u8]) {
        // The exfiltration direction: answering this would write whatever the
        // user last copied — often a password — straight into the process's
        // stdin. Turn never answers it.
        self.blocked_clipboard_reads = self.blocked_clipboard_reads.saturating_add(1);
    }

    fn resize(&mut self, _: &mut vt100::Screen, _request: (u16, u16)) {
        // Geometry belongs to the user's window, not to the program running in
        // it. The pty size only ever changes because the UI said so.
        self.blocked_resizes = self.blocked_resizes.saturating_add(1);
    }
}

/// Raw bytes plus a parsed screen, both bounded.
pub struct TerminalBuffer {
    parser: vt100::Parser<TerminalCallbacks>,
    bytes: VecDeque<u8>,
    byte_capacity: usize,
    bytes_seen: u64,
    /// True once the ring has dropped data, so a replay is known to be partial.
    truncated: bool,
    size: ScreenSize,
}

impl TerminalBuffer {
    pub fn new(size: ScreenSize) -> Self {
        Self::with_capacity(size, DEFAULT_BYTE_CAPACITY, DEFAULT_SCROLLBACK_ROWS)
    }

    pub fn with_capacity(size: ScreenSize, byte_capacity: usize, scrollback_rows: usize) -> Self {
        Self {
            parser: vt100::Parser::new_with_callbacks(
                size.rows,
                size.cols,
                scrollback_rows,
                TerminalCallbacks::default(),
            ),
            bytes: VecDeque::with_capacity(byte_capacity.min(64 * 1024)),
            byte_capacity,
            bytes_seen: 0,
            truncated: false,
            size,
        }
    }

    /// How many clipboard writes this process has attempted and had refused.
    pub fn blocked_clipboard_writes(&self) -> u32 {
        self.parser.callbacks().blocked_clipboard_writes
    }

    /// How many times this process asked to be *given* the clipboard's contents.
    pub fn blocked_clipboard_reads(&self) -> u32 {
        self.parser.callbacks().blocked_clipboard_reads
    }

    /// How many times this process asked to resize the user's window.
    pub fn blocked_resizes(&self) -> u32 {
        self.parser.callbacks().blocked_resizes
    }

    /// Bells rung by the process.
    pub fn bells(&self) -> u32 {
        self.parser.callbacks().bells
    }

    /// Feeds output from the process into both representations.
    pub fn write(&mut self, data: &[u8]) {
        self.parser.process(data);
        self.bytes_seen += data.len() as u64;

        // A single write larger than the whole ring: keep only its tail.
        if data.len() >= self.byte_capacity {
            self.bytes.clear();
            let start = data.len() - self.byte_capacity;
            self.bytes.extend(&data[start..]);
            self.truncated = true;
            return;
        }

        let overflow = (self.bytes.len() + data.len()).saturating_sub(self.byte_capacity);
        if overflow > 0 {
            self.bytes.drain(..overflow);
            self.truncated = true;
        }
        self.bytes.extend(data);
    }

    /// Resizes the terminal. The caller is responsible for telling the kernel.
    pub fn resize(&mut self, size: ScreenSize) {
        self.size = size;
        self.parser.screen_mut().set_size(size.rows, size.cols);
    }

    pub fn size(&self) -> ScreenSize {
        self.size
    }

    /// The bytes needed to rebuild this terminal in a fresh renderer.
    ///
    /// Prefers the parser's own formatted contents over the raw ring: it is far
    /// smaller and always self-consistent, where a truncated ring can start
    /// mid-escape-sequence and corrupt the receiving terminal.
    pub fn replay(&self) -> Vec<u8> {
        self.parser.screen().contents_formatted()
    }

    /// The raw byte ring, for callers that want exactly what arrived.
    pub fn raw(&self) -> Vec<u8> {
        self.bytes.iter().copied().collect()
    }

    /// The parsed screen itself.
    ///
    /// Exposed for the one caller that needs more than [`Self::snapshot`] gives: the
    /// daemon builds the cell grid a client renders, which needs every cell's colours
    /// and attributes rather than the trimmed text a preview wants. Borrowed rather
    /// than converted here on purpose — this crate knows nothing about the protocol's
    /// grid types, and the conversion lives with them
    /// (`turn_proto::cells::from_screen`) so the daemon and the client cannot end up
    /// with two readings of the same screen.
    pub fn screen(&self) -> &vt100::Screen {
        self.parser.screen()
    }

    pub fn is_truncated(&self) -> bool {
        self.truncated
    }

    pub fn bytes_seen(&self) -> u64 {
        self.bytes_seen
    }

    pub fn retained_bytes(&self) -> usize {
        self.bytes.len()
    }

    /// A snapshot for on-demand previews and heuristics.
    pub fn snapshot(&self) -> ScreenSnapshot {
        let screen = self.parser.screen();
        let lines: Vec<String> = screen
            .rows(0, self.size.cols)
            .map(|row| sanitise_row(&row))
            .collect();

        ScreenSnapshot {
            lines,
            cursor: screen.cursor_position(),
            size: self.size,
            alternate_screen: screen.alternate_screen(),
            // Sanitised again on the way out, cheaply, so a future callback that
            // forgets to do it at ingest cannot leak into the sidebar.
            title: self
                .parser
                .callbacks()
                .title
                .as_deref()
                .and_then(sanitise_title),
            bytes_seen: self.bytes_seen,
        }
    }

    /// Whether a full-screen application is in control. Turn uses this to avoid
    /// treating a TUI's redraw as agent output worth pattern-matching.
    pub fn in_alternate_screen(&self) -> bool {
        self.parser.screen().alternate_screen()
    }
}

/// Longest process-supplied label Turn keeps. A title is a label, not a payload.
pub const MAX_TITLE_CHARS: usize = 200;

/// Whether a character is safe to put in something a human reads.
///
/// "Safe" here means: it renders as itself, in the order it was written, in a
/// native label, a notification, a log line or a stored field. That rules out
/// three families, and each one is a real attack rather than tidiness:
///
/// * **Control characters** (C0 and C1, so `ESC` and the 8-bit `CSI` at U+009B
///   both) — the raw material of escape-sequence injection.
/// * **Bidirectional formatting** (U+202E and friends) — a right-to-left override
///   makes `rm -rf /` render as something harmless, which matters most in exactly
///   the place Turn shows it: the text next to an approval button.
/// * **Invisible and out-of-band characters** — zero-width spaces and joiners,
///   Unicode tag characters (U+E0000..U+E007F, which can carry a whole hidden
///   ASCII sentence), the line and paragraph separators U+2028/U+2029, which are
///   not `char::is_control` and will happily inject a second line into a
///   single-line log record.
///
/// Legitimate text loses almost nothing: an emoji ZWJ sequence renders as its
/// parts and a flag renders as a flag without its region tags. That is an
/// acceptable price for a label that cannot lie about its own contents.
pub fn is_display_safe(c: char) -> bool {
    if c.is_control() {
        return false;
    }
    !matches!(
        c,
        // Soft hyphen and Mongolian vowel separator: invisible.
        '\u{00ad}' | '\u{180e}'
        // Arabic letter mark, zero-width space/joiners, LRM/RLM.
        | '\u{061c}' | '\u{200b}'..='\u{200f}'
        // Line and paragraph separators (Zl/Zp, not control characters).
        | '\u{2028}' | '\u{2029}'
        // Bidi embeddings and overrides.
        | '\u{202a}'..='\u{202e}'
        // Word joiner, invisible maths operators, deprecated format controls.
        | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{206f}'
        // Zero-width no-break space (BOM when it turns up mid-string).
        | '\u{feff}'
        // Interlinear annotation, which hides the text it wraps.
        | '\u{fff9}'..='\u{fffb}'
        // Unicode tag characters: an invisible channel for arbitrary ASCII.
        | '\u{e0000}'..='\u{e007f}'
    )
}

/// Strips a process-supplied string down to something safe to display, capped.
///
/// Titles, and every other string an agent controls, arrive from an untrusted
/// source and end up in Turn's chrome. Dropping the offending characters one by
/// one is not enough on its own: `ESC [ 2 J` would leave a visible `[2J` behind,
/// so whole escape sequences are consumed. What survives is filtered through
/// [`is_display_safe`].
///
/// Returns `None` when nothing legible is left, so a caller cannot end up
/// rendering an empty label where it expected a name.
pub fn sanitise_label(raw: &str, max_chars: usize) -> Option<String> {
    if raw.is_empty() || max_chars == 0 {
        return None;
    }

    let mut out = String::new();
    let mut kept = 0usize;
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // Consume the rest of the sequence rather than leaving its tail as text.
            match chars.peek() {
                // CSI: parameters and intermediates, then a final byte in @..~
                Some('[') => {
                    chars.next();
                    consume_csi(&mut chars);
                }
                // OSC and other string sequences: run to BEL or ESC \.
                Some(']') | Some('P') | Some('X') | Some('^') | Some('_') => {
                    chars.next();
                    consume_control_string(&mut chars);
                }
                // A lone two-character sequence.
                Some(_) => {
                    chars.next();
                }
                None => {}
            }
            continue;
        }
        // ECMA-48 also defines single-character C1 introducers. Treating U+009B
        // as just another control would drop it but leave `31m` behind, letting a
        // hostile process forge visible text out of the tail of a CSI sequence.
        if c == '\u{009b}' {
            consume_csi(&mut chars);
            continue;
        }
        if matches!(
            c,
            '\u{0090}' | '\u{0098}' | '\u{009d}' | '\u{009e}' | '\u{009f}'
        ) {
            consume_control_string(&mut chars);
            continue;
        }
        if is_display_safe(c) {
            out.push(c);
            // Counted as we go: `out.chars().count()` per iteration would make a
            // long hostile title quadratic.
            kept += 1;
        }
        if kept >= max_chars {
            break;
        }
    }

    let trimmed = out.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Consumes a CSI body after either `ESC [` or the single-character C1 CSI.
fn consume_csi(chars: &mut Peekable<Chars<'_>>) {
    for next in chars.by_ref() {
        if ('\x40'..='\x7e').contains(&next) {
            break;
        }
    }
}

/// Consumes an OSC/DCS/SOS/PM/APC body through BEL or either spelling of ST.
fn consume_control_string(chars: &mut Peekable<Chars<'_>>) {
    while let Some(next) = chars.next() {
        if matches!(next, '\x07' | '\u{009c}') {
            break;
        }
        if next == '\x1b' && chars.peek() == Some(&'\\') {
            chars.next();
            break;
        }
    }
}

/// A process-set window title, sanitised and capped.
fn sanitise_title(raw: &str) -> Option<String> {
    sanitise_label(raw, MAX_TITLE_CHARS)
}

/// A rendered screen row, with anything that could misrepresent it removed.
///
/// Screen rows are process output too. They can reach product chrome through
/// previews, so they get the same treatment as a title — the parser has already
/// consumed the escape sequences, but nothing stops a program printing a bidi
/// override or a run of tag characters as ordinary text.
fn sanitise_row(row: &str) -> String {
    let cleaned: String = row.chars().filter(|c| is_display_safe(*c)).collect();
    cleaned.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_output_lands_on_the_screen() {
        let mut buffer = TerminalBuffer::new(ScreenSize::new(5, 20));
        buffer.write(b"hello world");

        let snapshot = buffer.snapshot();
        assert_eq!(snapshot.lines[0], "hello world");
        assert_eq!(snapshot.cursor, (0, 11));
        assert_eq!(snapshot.bytes_seen, 11);
        assert!(!snapshot.alternate_screen);
    }

    #[test]
    fn ansi_colour_and_cursor_movement_are_interpreted_not_shown() {
        let mut buffer = TerminalBuffer::new(ScreenSize::new(5, 20));
        buffer.write(b"\x1b[31mred\x1b[0m\r\nsecond");

        let snapshot = buffer.snapshot();
        assert_eq!(snapshot.lines[0], "red", "escape codes must not be visible");
        assert_eq!(snapshot.lines[1], "second");
    }

    #[test]
    fn a_full_screen_app_is_detected_so_heuristics_can_stand_down() {
        let mut buffer = TerminalBuffer::new(ScreenSize::new(10, 40));
        assert!(!buffer.in_alternate_screen());

        // Enter the alternate screen, as vim or lazygit does.
        buffer.write(b"\x1b[?1049h");
        assert!(buffer.in_alternate_screen());
        assert!(buffer.snapshot().alternate_screen);

        buffer.write(b"\x1b[?1049l");
        assert!(!buffer.in_alternate_screen());
    }

    #[test]
    fn the_byte_ring_is_bounded_and_admits_when_it_dropped_data() {
        let mut buffer = TerminalBuffer::with_capacity(ScreenSize::new(5, 20), 1_024, 100);
        assert!(!buffer.is_truncated());

        for _ in 0..200 {
            buffer.write(b"0123456789");
        }

        assert!(
            buffer.retained_bytes() <= 1_024,
            "the ring must stay bounded"
        );
        assert!(
            buffer.is_truncated(),
            "and must admit that a replay would be partial"
        );
        assert_eq!(buffer.bytes_seen(), 2_000, "but the total is still counted");
    }

    #[test]
    fn a_single_write_larger_than_the_ring_keeps_its_tail() {
        let mut buffer = TerminalBuffer::with_capacity(ScreenSize::new(5, 20), 100, 50);
        let big: Vec<u8> = (0..500u32).map(|i| b'a' + (i % 26) as u8).collect();
        buffer.write(&big);

        assert_eq!(buffer.retained_bytes(), 100);
        assert!(buffer.is_truncated());
        let raw = buffer.raw();
        assert_eq!(raw, &big[400..], "the newest bytes are the ones kept");
    }

    #[test]
    fn replay_reconstructs_the_screen_without_replaying_the_whole_ring() {
        let mut buffer = TerminalBuffer::with_capacity(ScreenSize::new(4, 20), 512, 50);
        for i in 0..100 {
            buffer.write(format!("line {i}\r\n").as_bytes());
        }

        let replay = buffer.replay();
        assert!(!replay.is_empty());
        // Feeding the replay into a fresh terminal reproduces what we see now.
        let mut rebuilt = TerminalBuffer::new(ScreenSize::new(4, 20));
        rebuilt.write(&replay);
        assert_eq!(
            rebuilt.snapshot().lines,
            buffer.snapshot().lines,
            "a re-attached pane must look identical"
        );
    }

    /// A preview wants trimmed text; a rendered pane wants every cell's colours.
    /// Both come off the same parsed screen, which is what stops the daemon from
    /// needing a second terminal emulator to answer the second question.
    #[test]
    fn the_parsed_screen_is_reachable_with_its_colours_for_a_client_that_draws_cells() {
        let mut buffer = TerminalBuffer::new(ScreenSize::new(4, 20));
        buffer.write(b"\x1b[1;31mred\x1b[0m plain");

        let screen = buffer.screen();
        assert_eq!(screen.size(), (4, 20));
        let cell = screen.cell(0, 0).expect("the first cell");
        assert_eq!(cell.contents(), "r");
        assert!(cell.bold());
        assert_eq!(
            cell.fgcolor(),
            vt100::Color::Idx(1),
            "the palette index must survive; resolving it is the daemon's job"
        );
        let plain = screen.cell(0, 4).expect("a cell past the reset");
        assert_eq!(plain.fgcolor(), vt100::Color::Default);
        assert_eq!(screen.cursor_position(), (0, 9));
    }

    #[test]
    fn resizing_reflows_the_screen() {
        let mut buffer = TerminalBuffer::new(ScreenSize::new(5, 10));
        buffer.write(b"0123456789ABCDEFGHIJ");
        assert_eq!(buffer.snapshot().lines[0], "0123456789");

        buffer.resize(ScreenSize::new(5, 20));
        assert_eq!(buffer.size(), ScreenSize::new(5, 20));
        let snapshot = buffer.snapshot();
        assert_eq!(snapshot.size.cols, 20);
        assert_eq!(snapshot.lines.len(), 5);
    }

    #[test]
    fn a_zero_sized_terminal_is_clamped_instead_of_panicking() {
        let size = ScreenSize::new(0, 0);
        assert_eq!(size, ScreenSize { rows: 1, cols: 1 });
        let mut buffer = TerminalBuffer::new(size);
        buffer.write(b"x");
        assert_eq!(buffer.snapshot().lines.len(), 1);
    }

    #[test]
    fn snapshot_tail_returns_the_last_meaningful_lines() {
        let mut buffer = TerminalBuffer::new(ScreenSize::new(10, 30));
        buffer.write(b"first\r\nsecond\r\nthird\r\n\r\n\r\n");

        let snapshot = buffer.snapshot();
        assert_eq!(snapshot.tail(2), vec!["second", "third"]);
        assert!(snapshot.text().contains("first"));
    }

    /// A title comes from the process, which may be an adversarial agent. It
    /// must never carry control characters into the sidebar.
    #[test]
    fn a_malicious_title_is_stripped_of_control_characters() {
        let mut buffer = TerminalBuffer::new(ScreenSize::new(5, 40));
        buffer.write(b"\x1b]0;evil\x07");
        let title = buffer.snapshot().title;
        if let Some(title) = title {
            assert!(
                !title.chars().any(|c| c.is_control()),
                "control characters leaked into the title: {title:?}"
            );
        }

        assert_eq!(sanitise_title("clean title"), Some("clean title".into()));
        assert_eq!(
            sanitise_title("bad\x07\x1b[2Jtitle"),
            Some("badtitle".into())
        );
        assert_eq!(
            sanitise_title("a\x1b]0;nested\x07b"),
            Some("ab".into()),
            "a nested OSC must not leave its payload behind"
        );
        assert_eq!(sanitise_title("\x07\x07"), None);
        assert_eq!(sanitise_title(""), None);
        assert_eq!(sanitise_title(&"x".repeat(500)).unwrap().len(), 200);
    }

    #[test]
    fn c1_control_sequences_are_consumed_with_their_payload_syntax() {
        assert_eq!(
            sanitise_label("safe\u{009b}31mred\u{009b}0m text", 200),
            Some("safered text".into()),
            "a single-character CSI must not leave `31m` in the label"
        );
        assert_eq!(
            sanitise_label("before\u{009d}forged title\u{009c}after", 200),
            Some("beforeafter".into()),
            "a C1 OSC must be consumed through its ST terminator"
        );
    }

    /// A title is the one piece of process-controlled text Turn puts in native
    /// chrome, so it must not be able to render as something other than itself.
    /// `RLO` reverses everything after it: the payload below reads as
    /// "safe-title" on screen while the stored text says otherwise.
    #[test]
    fn a_title_cannot_reverse_its_own_rendering_with_a_direction_override() {
        let attack = "\u{202e}eltit-efas\u{202c}";
        let title = sanitise_title(attack).expect("some text survives");
        assert!(
            !title.contains('\u{202e}') && !title.contains('\u{202c}'),
            "bidi formatting reached a UI label: {title:?}"
        );
        assert_eq!(title, "eltit-efas");

        for hostile in [
            "\u{2066}isolated\u{2069}",
            "left\u{200b}\u{200b}right",
            "emoji\u{200d}joiner",
            "line\u{2028}second line",
            "para\u{2029}second para",
            "bom\u{feff}here",
        ] {
            let cleaned = sanitise_title(hostile).unwrap();
            assert!(
                cleaned.chars().all(is_display_safe),
                "{hostile:?} survived as {cleaned:?}"
            );
            assert!(!cleaned.contains('\n'));
        }
    }

    /// Unicode tag characters are invisible and can spell out a whole sentence.
    /// A label that renders as "build" must not carry a hidden instruction.
    #[test]
    fn a_title_cannot_smuggle_invisible_tag_characters_into_a_label() {
        let mut attack = String::from("build");
        for byte in b"rm -rf /" {
            attack.push(char::from_u32(0xE0000 + u32::from(*byte)).unwrap());
        }
        assert_eq!(sanitise_title(&attack), Some("build".to_string()));
    }

    /// The parser buffers an OSC payload without a bound of its own, so what Turn
    /// *retains* has to be bounded at the moment the callback fires.
    #[test]
    fn an_enormous_title_is_capped_when_it_arrives_not_when_it_is_read() {
        let mut buffer = TerminalBuffer::new(ScreenSize::new(5, 40));
        let mut sequence = Vec::from(b"\x1b]0;".as_slice());
        sequence.extend(std::iter::repeat_n(b'A', 512 * 1024));
        sequence.push(0x07);
        buffer.write(&sequence);

        let retained = buffer
            .parser
            .callbacks()
            .title
            .as_deref()
            .expect("a title was set");
        assert_eq!(
            retained.chars().count(),
            MAX_TITLE_CHARS,
            "half a megabyte of title must not be kept per pane"
        );
        assert_eq!(
            buffer.snapshot().title.unwrap().chars().count(),
            MAX_TITLE_CHARS
        );
    }

    /// Invalid UTF-8 in a title must degrade to replacement characters rather
    /// than panicking or being dropped silently.
    #[test]
    fn a_title_of_invalid_utf8_is_replaced_rather_than_fatal() {
        let mut buffer = TerminalBuffer::new(ScreenSize::new(5, 40));
        // A lone continuation byte and a truncated three-byte sequence.
        buffer.write(b"\x1b]0;ok\x80\xe2\x28more\x07");
        let title = buffer.snapshot().title.expect("a title survives");
        assert!(title.starts_with("ok"), "got {title:?}");
        assert!(title.contains('\u{fffd}'), "got {title:?}");
    }

    /// A process must not be able to read the user's clipboard by asking the
    /// terminal for it. vt100's default for this callback is an empty body, so
    /// the refusal is written down and asserted rather than inherited.
    #[test]
    fn a_clipboard_read_request_from_the_process_is_refused_and_recorded() {
        let mut buffer = TerminalBuffer::new(ScreenSize::new(5, 40));
        // OSC 52 with `?` as the payload: "send me the clipboard".
        buffer.write(b"\x1b]52;c;?\x07");

        assert_eq!(buffer.blocked_clipboard_reads(), 1);
        assert_eq!(buffer.blocked_clipboard_writes(), 0);
        assert_eq!(
            buffer.snapshot().lines[0],
            "",
            "and nothing of the request was drawn"
        );
    }

    /// Window geometry belongs to the user. A process asking for it is counted
    /// and ignored.
    #[test]
    fn a_resize_request_from_the_process_is_refused() {
        let mut buffer = TerminalBuffer::new(ScreenSize::new(24, 80));
        buffer.write(b"\x1b[8;200;500t");

        assert_eq!(buffer.blocked_resizes(), 1);
        assert_eq!(buffer.size(), ScreenSize::new(24, 80));
        assert_eq!(buffer.snapshot().lines.len(), 24);
    }

    /// Screen rows can reach product chrome through previews, so they get the same
    /// treatment as a title: an agent cannot print a direction override and have the preview
    /// render its output backwards.
    #[test]
    fn screen_rows_never_carry_invisible_or_direction_changing_characters() {
        let mut buffer = TerminalBuffer::new(ScreenSize::new(6, 40));
        buffer.write("running \u{202e}sdrawkcab\u{202c} now\r\n".as_bytes());
        buffer.write("zero\u{200b}width\r\n".as_bytes());

        let snapshot = buffer.snapshot();
        assert!(
            snapshot
                .lines
                .iter()
                .all(|line| line.chars().all(is_display_safe)),
            "got {:?}",
            snapshot.lines
        );
        assert!(snapshot.text().contains("running sdrawkcab now"));
        assert!(snapshot.text().contains("zerowidth"));
    }

    /// A very long single line is bounded by the screen, not by the process.
    #[test]
    fn one_enormous_line_is_bounded_by_the_terminal_geometry() {
        let mut buffer = TerminalBuffer::with_capacity(ScreenSize::new(24, 80), 64 * 1024, 100);
        buffer.write(&vec![b'x'; 4 * 1024 * 1024]);

        let snapshot = buffer.snapshot();
        assert_eq!(snapshot.lines.len(), 24);
        assert!(snapshot.lines.iter().all(|line| line.chars().count() <= 80));
        assert!(buffer.retained_bytes() <= 64 * 1024);
    }

    /// A process must not be able to rewrite the user's clipboard by printing
    /// an escape sequence.
    #[test]
    fn a_clipboard_write_from_the_process_is_refused_but_recorded() {
        let mut buffer = TerminalBuffer::new(ScreenSize::new(5, 40));
        assert_eq!(buffer.blocked_clipboard_writes(), 0);

        // OSC 52: "copy this base64 payload to the clipboard".
        buffer.write(b"\x1b]52;c;bWFsaWNpb3Vz\x07");
        assert_eq!(
            buffer.blocked_clipboard_writes(),
            1,
            "the attempt is counted so the UI can surface it"
        );
        // And nothing of it was drawn.
        assert_eq!(buffer.snapshot().lines[0], "");
    }

    #[test]
    fn heavy_output_does_not_grow_memory_without_bound() {
        let mut buffer = TerminalBuffer::with_capacity(ScreenSize::new(24, 80), 64 * 1024, 500);
        // A megabyte of noisy output, as a build or a test runner would produce.
        for i in 0..20_000 {
            buffer.write(format!("[{i:05}] compiling something rather verbose\r\n").as_bytes());
        }
        assert!(buffer.retained_bytes() <= 64 * 1024);
        let snapshot = buffer.snapshot();
        assert_eq!(
            snapshot.lines.len(),
            24,
            "only the visible rows are rendered"
        );
        assert!(buffer.bytes_seen() > 500_000);
    }
}
