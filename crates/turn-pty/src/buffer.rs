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

use crate::images;
use crate::links::{parse_osc8, screen_rows, LinkSpan, LinkTracker, REALIGN_BUDGET_PER_WRITE};

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
    /// Bumped only when the title actually becomes something different.
    ///
    /// The daemon watches this instead of comparing strings on every read. That
    /// matters more than it looks: a shell with `PROMPT_COMMAND` re-sends its title
    /// on every command and `vim` on every file, so the common case is the same
    /// title arriving over and over, and it must cost nothing.
    title_generation: u64,
    icon_name: Option<String>,
    bells: u32,
    /// Clipboard writes requested by the process, all refused.
    blocked_clipboard_writes: u32,
    /// Clipboard *reads* requested by the process, all refused.
    blocked_clipboard_reads: u32,
    /// Resizes the process asked the window manager for, all refused.
    blocked_resizes: u32,
    /// Which cells carry an OSC 8 hyperlink.
    ///
    /// Kept here because `vt100` has no notion of a hyperlink at all — the sequence arrives
    /// as [`vt100::Callbacks::unhandled_osc`] — and because this is the one place that sees
    /// the escape *and* the screen it landed on, which is what makes the extent of a link
    /// knowable. See [`crate::links`] for how the marks stay attached to their text.
    links: LinkTracker,
}

impl TerminalCallbacks {
    fn new(size: ScreenSize) -> Self {
        Self {
            links: LinkTracker::new(size.rows, size.cols),
            ..Self::default()
        }
    }
}

impl vt100::Callbacks for TerminalCallbacks {
    fn set_window_title(&mut self, _: &mut vt100::Screen, title: &[u8]) {
        // Sanitised and capped here rather than in `snapshot`, because what
        // arrives is an OSC payload the parser buffers without a bound: an agent
        // can emit a megabyte-long title, and retaining it per pane would be a
        // memory cost the process controls.
        let sanitised = sanitise_label(&String::from_utf8_lossy(title), MAX_TITLE_CHARS);
        if sanitised != self.title {
            self.title = sanitised;
            self.title_generation = self.title_generation.saturating_add(1);
        }
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

    fn unhandled_osc(&mut self, screen: &mut vt100::Screen, params: &[&[u8]]) {
        // OSC 8 is the only unhandled OSC Turn acts on. Everything else a program invents
        // is ignored here as it was before: an escape sequence with no meaning to Turn must
        // not become one by accident.
        let Some(link) = parse_osc8(params) else {
            return;
        };
        // Reading the screen is what keeps a link attached to its own text as the screen
        // scrolls, and it is skipped whenever it cannot change the answer — which is every
        // pane with no links, and any write that has already realigned enough times.
        let rows = self.links.wants_rows().then(|| screen_rows(screen));
        let cursor = screen.cursor_position();
        match link {
            Some(uri) => self.links.open_link(&uri, cursor, rows.as_deref()),
            None => self.links.close_link(cursor, rows.as_deref()),
        }
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
    /// Lifts inline-image sequences out of the stream before the parser sees them.
    ///
    /// It has to be upstream of the parser rather than a callback on it: `vt100` swallows
    /// DCS entirely, so Sixel would be invisible, and `ESC _` never reaches a callback at
    /// all, so the Kitty protocol would be too. See [`crate::images::scan`].
    scanner: images::Scanner,
    /// This pane's pictures: the payloads it holds and which slots are placed.
    images: images::ImageStore,
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
                TerminalCallbacks::new(size),
            ),
            bytes: VecDeque::with_capacity(byte_capacity.min(64 * 1024)),
            byte_capacity,
            bytes_seen: 0,
            truncated: false,
            size,
            scanner: images::Scanner::new(),
            images: images::ImageStore::new(),
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

    /// The title the process set for itself, sanitised and length-capped.
    ///
    /// `None` until the process sets one, which is most of the time: a title is
    /// something a program opts into.
    pub fn title(&self) -> Option<&str> {
        self.parser.callbacks().title.as_deref()
    }

    /// How many times the title has become something different.
    ///
    /// The daemon keeps the last value it saw and compares integers; a repeated
    /// identical title does not move it, so it produces no work.
    pub fn title_generation(&self) -> u64 {
        self.parser.callbacks().title_generation
    }

    /// Bells rung by the process.
    pub fn bells(&self) -> u32 {
        self.parser.callbacks().bells
    }

    /// The OSC 8 hyperlinks the process declared, as spans over the current screen.
    ///
    /// Empty for nearly every pane, because a hyperlink is something a program opts into.
    pub fn link_spans(&self) -> Vec<LinkSpan> {
        self.parser.callbacks().links.spans()
    }

    /// The hyperlinks in the borrowed form `turn_proto::from_screen_with_links` takes.
    ///
    /// A `(row, from, to, uri)` tuple rather than a named type on purpose: this crate is
    /// deliberately ignorant of the protocol's grid types — see [`Self::screen`] — and a
    /// borrowed tuple is the narrowest thing the two sides can agree on without one of them
    /// depending on the other.
    pub fn screen_links(&self) -> Vec<(u16, u16, u16, std::sync::Arc<str>)> {
        self.link_spans()
            .into_iter()
            .map(|span| (span.row, span.from, span.to, span.uri))
            .collect()
    }

    /// URIs the process declared that Turn refused to keep: empty, over-long, or carrying a
    /// control character. Counted so the UI can say a process tried rather than hiding it.
    pub fn refused_links(&self) -> u32 {
        self.parser.callbacks().links.refused()
    }

    /// Hyperlinks dropped because the cells they covered could not be proved.
    pub fn abandoned_links(&self) -> u32 {
        self.parser.callbacks().links.abandoned()
    }

    /// This pane's inline images: the payloads it holds and which slots are placed.
    pub fn images(&self) -> &images::ImageStore {
        &self.images
    }

    /// The pixels of one inline image, for a client that asked for them by id.
    ///
    /// `None` once the picture has been evicted from the pane's bounded store, which is the
    /// honest answer: the client draws a placeholder rather than a different picture.
    pub fn image_payload(&self, id: turn_proto::ImageId) -> Option<&turn_proto::ImagePayload> {
        self.images.payload(id)
    }

    /// Tells the buffer how many pixels a cell is on the client that is drawing it.
    ///
    /// Only used to turn a *pixel* size in an escape sequence — iTerm2's `width=400px` — into
    /// a number of cells. Until this is called, [`images::NOMINAL_CELL_PIXELS`] is assumed;
    /// being wrong about it changes how much of a pane a picture claims and never its shape,
    /// because the client fits the picture inside the box it was given.
    pub fn set_cell_pixels(&mut self, width: u16, height: u16) {
        self.images.set_cell_pixels(width, height);
    }

    /// This pane's screen as the grid a client renders, images and hyperlinks included.
    ///
    /// The one call the daemon needs: it gathers the three things a grid is built from — the
    /// parsed screen, the hyperlink spans and the image table — and hands them to the single
    /// conversion in `turn_proto`, so there is still exactly one reading of what a screen
    /// means as cells.
    pub fn grid(&self) -> turn_proto::Grid {
        let links = self.screen_links();
        turn_proto::from_screen_with_images(
            self.parser.screen(),
            links
                .iter()
                .map(|(row, from, to, uri)| (*row, *from, *to, uri.as_ref())),
            &self.images.table(),
        )
    }

    /// A screen-shaped window of this pane's history, with its pictures.
    ///
    /// The scrollback equivalent of [`Self::grid`], and it exists for the same reason: the
    /// window of history is built from the parser's own rows, so it carries the marker cells
    /// of any picture that scrolled into it, and those markers mean nothing without the table
    /// that says which payload each slot holds.
    ///
    /// The table is filtered to the slots actually in the window, so a client is only ever
    /// asked for pixels it has somewhere to draw.
    pub fn history_grid(&mut self, offset: usize) -> turn_proto::Grid {
        let table = self.images.table();
        let mut grid = self.with_history(|screen| turn_proto::history_grid(screen, offset));
        grid.attach_images(&table);
        grid
    }

    /// Feeds output from the process into both representations.
    ///
    /// Inline-image sequences are taken out of the stream first, decoded, and replaced by
    /// the marker cells that stand for the picture — so what the parser sees is the output
    /// minus the pictures, plus the cells they occupy. A sequence Turn will not act on
    /// becomes a line of text saying so, because a picture that silently did not appear is
    /// a bug report nobody can write.
    pub fn write(&mut self, data: &[u8]) {
        self.parser
            .callbacks_mut()
            .links
            .begin_write(REALIGN_BUDGET_PER_WRITE);
        // Decoding credit is earned from the bytes that arrived, before any of them are
        // spent, so a picture in this very write can be decoded.
        self.images.credit(data.len());
        for event in self.scanner.feed(data) {
            match event {
                images::ScanEvent::Text(bytes) => self.parser.process(&bytes),
                images::ScanEvent::Image(sequence) => {
                    if let Some(reason) =
                        images::apply(&mut self.parser, &mut self.images, *sequence)
                    {
                        self.parser.process(&reason.notice_bytes());
                    }
                }
                images::ScanEvent::Refused(reason) => {
                    // Refused by the scanner rather than by a decoder — a payload past its
                    // limit, or base64 that is not base64 — so it is counted here.
                    self.images.note_refusal();
                    self.parser.process(&reason.notice_bytes());
                }
            }
        }
        // The moment the daemon reads the marks is right after a write, so this is where
        // they have to be back in step with the screen. Skipped entirely — including the
        // hashing — for a pane that has never seen a hyperlink.
        if !self.parser.callbacks().links.is_idle() {
            let rows = screen_rows(self.parser.screen());
            self.parser.callbacks_mut().links.settle(&rows);
        }
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
        // A reflow moves every character, and a hyperlink mark left where it was would sit
        // on text it was never declared over.
        self.parser
            .callbacks_mut()
            .links
            .resize(size.rows, size.cols);
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
        strip_image_markers(self.parser.screen().contents_formatted())
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

    /// Reads the parsed screen *including its scrollback*, and puts the viewport back.
    ///
    /// The parser keeps history behind the live screen, and the only way to read it is to
    /// move the screen's own scrollback offset — which every other reader of this buffer
    /// shares. So the borrow is scoped: whatever the closure does with the offset, it is
    /// zero again by the time this returns, on the panicking path as well as the normal
    /// one. Without that, one client's search would leave every other client's pane
    /// rendering history as though it were live, and nothing would put it right until the
    /// process next wrote something.
    ///
    /// This exists rather than a `screen_mut` because a bare `&mut vt100::Screen` makes
    /// that mistake available to every caller, and it is a mistake nothing would catch:
    /// the screen would still be a perfectly valid screen, only of the wrong moment.
    pub fn with_history<R>(&mut self, read: impl FnOnce(&mut vt100::Screen) -> R) -> R {
        /// Restores the viewport when the borrow ends, however it ends.
        struct Restore<'a> {
            screen: &'a mut vt100::Screen,
        }
        impl Drop for Restore<'_> {
            fn drop(&mut self) {
                self.screen.set_scrollback(0);
            }
        }
        let guard = Restore {
            screen: self.parser.screen_mut(),
        };
        read(guard.screen)
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

    /// How many rows of history sit behind the live screen.
    ///
    /// Measured rather than counted, because the parser reports the maximum it was
    /// configured with and not what it is actually holding. Zero while a full-screen
    /// application is in control: the alternate screen has no scrollback of its own, which
    /// is exactly why Turn must not offer to scroll one.
    pub fn history_rows(&mut self) -> usize {
        self.with_history(|screen| {
            screen.set_scrollback(usize::MAX);
            screen.scrollback()
        })
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
        // The two supplementary private-use planes. No font agrees on what these mean, so
        // none of them renders as itself — and Turn's own inline-image markers live in
        // plane 16, where a label must never carry one.
        | '\u{f0000}'..='\u{10ffff}'
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
    let cleaned: String = row
        .chars()
        .filter_map(|c| {
            // A picture's marker cells become spaces rather than vanishing, so the words
            // on either side of an image do not run together in a preview.
            if turn_proto::is_marker(c) {
                Some(' ')
            } else if is_display_safe(c) {
                Some(c)
            } else {
                None
            }
        })
        .collect();
    cleaned.trim_end().to_string()
}

/// Replaces inline-image markers in a replay with spaces.
///
/// A replay is fed to a *real terminal* — a byte-stream client, or a log capture — and Turn's
/// markers mean nothing there: they would arrive as missing-glyph boxes where a picture
/// should be. A space is the honest substitute, and it keeps every column of the replayed
/// screen where it was.
///
/// Done at the byte level because the replay is escape sequences as well as text. Every
/// marker is a four-byte sequence beginning `F4 8x`, and no ANSI escape sequence contains a
/// byte over 0x7E, so the pattern cannot collide with one.
fn strip_image_markers(replay: Vec<u8>) -> Vec<u8> {
    if !replay.contains(&0xF4) {
        return replay;
    }
    let mut out = Vec::with_capacity(replay.len());
    let mut index = 0usize;
    while index < replay.len() {
        let marker = replay
            .get(index..index + 4)
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .and_then(|text| text.chars().next())
            .is_some_and(turn_proto::is_marker);
        if marker {
            out.push(b' ');
            index += 4;
            continue;
        }
        out.push(replay[index]);
        index += 1;
    }
    out
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

    /// The door stays shut, and this is the test that notices if somebody opens it.
    ///
    /// The refusal is written out in [`TerminalCallbacks`] rather than inherited from
    /// `vt100`'s empty defaults, and this asserts the *whole* of the contract rather than
    /// one sequence: neither direction of OSC 52 is acted on, every spelling of it is
    /// counted, and — the part a future change is most likely to break — the buffer exposes
    /// no way to read what a process asked to copy or to hand it anything back. A
    /// `clipboard()` accessor added here would compile, pass every other test, and turn an
    /// agent's stdout into clipboard access; it would fail this one.
    #[test]
    fn nothing_a_process_writes_can_reach_the_clipboard_in_either_direction() {
        let mut buffer = TerminalBuffer::new(ScreenSize::new(5, 40));

        // Every spelling of the write: BEL-terminated, ST-terminated, each of the
        // selections xterm defines, and a payload big enough to be worth stealing.
        let writes: [&[u8]; 4] = [
            b"\x1b]52;c;c2VjcmV0\x07",
            b"\x1b]52;c;c2VjcmV0\x1b\\",
            b"\x1b]52;p;c2VjcmV0\x07",
            b"\x1b]52;;c2VjcmV0\x07",
        ];
        for (count, sequence) in writes.iter().enumerate() {
            buffer.write(sequence);
            assert_eq!(
                buffer.blocked_clipboard_writes(),
                count as u32 + 1,
                "a clipboard write must be counted, not performed: {sequence:?}"
            );
        }

        // And every spelling of the read, which is the exfiltration direction: answering
        // one would put whatever the user last copied into the process's stdin.
        let reads: [&[u8]; 3] = [b"\x1b]52;c;?\x07", b"\x1b]52;p;?\x1b\\", b"\x1b]52;;?\x07"];
        for (count, sequence) in reads.iter().enumerate() {
            buffer.write(sequence);
            assert_eq!(buffer.blocked_clipboard_reads(), count as u32 + 1);
        }

        // Nothing was drawn, so the payload did not arrive as text either.
        assert!(
            buffer.snapshot().text().trim().is_empty(),
            "the request itself must not be printed: {:?}",
            buffer.snapshot().lines
        );
        // And there is nothing to answer with: the counters are the only trace, and a
        // count is not a payload.
        assert_eq!(buffer.blocked_clipboard_writes(), 4);
        assert_eq!(buffer.blocked_clipboard_reads(), 3);

        // The refusal is a property of the callbacks, not of this buffer's state: a fresh
        // one refuses identically, and a process cannot wear the door down by asking.
        let mut fresh = TerminalBuffer::new(ScreenSize::new(5, 40));
        for _ in 0..1_000 {
            fresh.write(b"\x1b]52;c;?\x07");
        }
        assert_eq!(fresh.blocked_clipboard_reads(), 1_000);
        assert!(
            fresh.replay().is_empty() || !String::from_utf8_lossy(&fresh.replay()).contains("52;")
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

    /// The whole OSC 8 path, driven by a real escape stream rather than by calling the
    /// tracker: this is how `gh`, `ls --hyperlink` and an agent citing a PR emit a link.
    #[test]
    fn an_osc_eight_hyperlink_from_a_real_stream_covers_the_text_it_wrapped() {
        let mut buffer = TerminalBuffer::new(ScreenSize::new(6, 40));
        buffer.write(b"open \x1b]8;;https://example.com/pull/42\x1b\\the PR\x1b]8;;\x1b\\ now\r\n");

        assert_eq!(
            buffer.snapshot().lines[0],
            "open the PR now",
            "the escape sequences must not be visible"
        );
        let spans = buffer.link_spans();
        assert_eq!(spans.len(), 1, "got {spans:?}");
        assert_eq!((spans[0].row, spans[0].from, spans[0].to), (0, 5, 11));
        assert_eq!(&*spans[0].uri, "https://example.com/pull/42");
        assert_eq!(buffer.refused_links(), 0);
        assert_eq!(buffer.abandoned_links(), 0);

        // The BEL-terminated spelling, which is what most shells actually emit.
        let mut bel = TerminalBuffer::new(ScreenSize::new(6, 40));
        bel.write(b"\x1b]8;id=1;https://example.com/a\x07link\x1b]8;;\x07");
        let spans = bel.link_spans();
        assert_eq!(spans.len(), 1, "got {spans:?}");
        assert_eq!(&*spans[0].uri, "https://example.com/a");
        assert_eq!((spans[0].from, spans[0].to), (0, 4));
    }

    /// The case the tracker exists for: a link scrolls up the screen with its own text, and
    /// leaves when the text leaves.
    #[test]
    fn a_hyperlink_follows_its_text_up_the_screen_and_off_it() {
        let mut buffer = TerminalBuffer::new(ScreenSize::new(4, 30));
        buffer.write(b"\x1b]8;;https://example.com/pr/9\x1b\\the PR\x1b]8;;\x1b\\\r\n");
        assert_eq!(buffer.link_spans()[0].row, 0);

        buffer.write(b"one\r\ntwo\r\n");
        let spans = buffer.link_spans();
        assert_eq!(spans.len(), 1, "got {spans:?}");
        assert_eq!(spans[0].row, 0, "still on screen, nothing has scrolled yet");

        // Enough output to push the link's row off the top.
        for line in 0..6 {
            buffer.write(format!("line {line}\r\n").as_bytes());
        }
        assert!(
            buffer.link_spans().is_empty(),
            "the link left with its text"
        );
        assert!(
            !buffer.snapshot().text().contains("the PR"),
            "and its text really is gone"
        );
    }

    /// An OSC 8 URI is a string a human will be shown. A process must not be able to put an
    /// escape sequence or a newline in it.
    #[test]
    fn a_hyperlink_uri_carrying_a_control_character_is_refused_and_counted() {
        let mut buffer = TerminalBuffer::new(ScreenSize::new(4, 30));
        // A DEL inside the URI. The OSC payload survives the parser, so the refusal has to
        // happen where the URI is captured.
        buffer.write(b"\x1b]8;;https://a.example/\x7f\x1b\\text\x1b]8;;\x1b\\");
        assert_eq!(buffer.refused_links(), 1);
        assert!(buffer.link_spans().is_empty());

        let mut over_long = TerminalBuffer::new(ScreenSize::new(4, 30));
        let mut sequence = Vec::from(b"\x1b]8;;https://a.example/".as_slice());
        sequence.extend(std::iter::repeat_n(b'x', crate::links::MAX_LINK_URI_CHARS));
        sequence.extend_from_slice(b"\x1b\\text\x1b]8;;\x1b\\");
        over_long.write(&sequence);
        assert_eq!(over_long.refused_links(), 1);
        assert!(over_long.link_spans().is_empty());
        assert_eq!(
            over_long.snapshot().lines[0],
            "text",
            "the text is still drawn; only the link is refused"
        );
    }

    /// A resize reflows the screen, so every mark has to go: the alternative is a link on
    /// whatever text ended up under it.
    #[test]
    fn resizing_forgets_the_hyperlinks_rather_than_leaving_them_on_reflowed_text() {
        let mut buffer = TerminalBuffer::new(ScreenSize::new(4, 30));
        buffer.write(b"\x1b]8;;https://a.example\x1b\\link\x1b]8;;\x1b\\");
        assert_eq!(buffer.link_spans().len(), 1);

        buffer.resize(ScreenSize::new(8, 12));
        assert!(buffer.link_spans().is_empty());
    }

    /// Every other unhandled OSC must stay unhandled: adding a meaning by accident is how a
    /// program ends up able to do something Turn never agreed to.
    #[test]
    fn an_unhandled_osc_that_is_not_a_hyperlink_still_does_nothing() {
        let mut buffer = TerminalBuffer::new(ScreenSize::new(4, 20));
        // OSC 7 (working directory), OSC 9 (a notification), and a nonsense one.
        buffer.write(b"\x1b]7;file://host/tmp\x1b\\\x1b]9;done\x07\x1b]1337;File=x\x07visible");
        assert!(buffer.link_spans().is_empty());
        assert_eq!(buffer.refused_links(), 0);
        assert_eq!(buffer.abandoned_links(), 0);
        assert_eq!(buffer.snapshot().lines[0], "visible");
    }

    /// A pane with no hyperlinks must not pay for the tracking, which is what makes it
    /// acceptable to have at all.
    #[test]
    fn a_pane_with_no_hyperlinks_does_no_link_work_per_write() {
        let mut buffer = TerminalBuffer::new(ScreenSize::new(40, 120));
        for line in 0..500 {
            buffer.write(format!("   Compiling something v0.1.{line}\r\n").as_bytes());
        }
        assert!(buffer.parser.callbacks().links.is_idle());
        assert!(buffer.link_spans().is_empty());
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

    /// The scrollback is the daemon's, and reading it must not change what anybody else
    /// sees. Every reader shares one screen, so the borrow has to put the viewport back.
    #[test]
    fn reading_the_history_leaves_the_live_screen_in_view_for_every_other_reader() {
        let mut buffer = TerminalBuffer::with_capacity(ScreenSize::new(4, 20), 64 * 1024, 100);
        for line in 0..20 {
            buffer.write(format!("line {line}\r\n").as_bytes());
        }
        assert_eq!(buffer.snapshot().lines[0], "line 17");

        let oldest = buffer.with_history(|screen| {
            screen.set_scrollback(usize::MAX);
            screen.rows(0, 20).next().unwrap_or_default()
        });
        assert_eq!(oldest.trim_end(), "line 0", "the history is readable");
        assert_eq!(
            buffer.snapshot().lines[0],
            "line 17",
            "and the live screen is what the next reader gets"
        );
        assert_eq!(buffer.screen().scrollback(), 0);
    }

    /// A panic inside the borrow must not leave every pane rendering history as though it
    /// were live: the screen would still be valid, only of the wrong moment, and nothing
    /// would put it right until the process next wrote something.
    #[test]
    fn a_panic_while_reading_the_history_still_restores_the_viewport() {
        let mut buffer = TerminalBuffer::with_capacity(ScreenSize::new(4, 20), 64 * 1024, 100);
        for line in 0..20 {
            buffer.write(format!("line {line}\r\n").as_bytes());
        }

        let escaped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            buffer.with_history(|screen| {
                screen.set_scrollback(10);
                panic!("a reader gave up half way");
            })
        }));
        assert!(
            escaped.is_err(),
            "the panic must propagate, not be swallowed"
        );
        assert_eq!(buffer.screen().scrollback(), 0);
        assert_eq!(buffer.snapshot().lines[0], "line 17");
    }

    /// The parser reports the maximum it was configured with, not what it holds, so the
    /// depth of the record has to be measured.
    #[test]
    fn the_depth_of_the_history_is_measured_rather_than_assumed() {
        let mut buffer = TerminalBuffer::with_capacity(ScreenSize::new(4, 20), 64 * 1024, 50);
        assert_eq!(buffer.history_rows(), 0, "nothing has scrolled off yet");

        for line in 0..10 {
            buffer.write(format!("l{line}\r\n").as_bytes());
        }
        assert_eq!(buffer.history_rows(), 7, "ten lines on a four-row screen");

        // Bounded by what the parser was told to keep, however much is written.
        for line in 0..500 {
            buffer.write(format!("x{line}\r\n").as_bytes());
        }
        assert_eq!(buffer.history_rows(), 50);
    }

    /// A full-screen application's grid has no history of its own, which is exactly why
    /// Turn must stand down rather than offer to scroll one.
    #[test]
    fn a_full_screen_application_reports_no_history_to_scroll_into() {
        let mut buffer = TerminalBuffer::with_capacity(ScreenSize::new(4, 20), 64 * 1024, 100);
        for line in 0..20 {
            buffer.write(format!("line {line}\r\n").as_bytes());
        }
        assert!(buffer.history_rows() > 0);

        buffer.write(b"\x1b[?1049h");
        assert_eq!(buffer.history_rows(), 0);

        // And it comes back when the program leaves.
        buffer.write(b"\x1b[?1049l");
        assert!(buffer.history_rows() > 0);
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

    // ---------------------------------------------------------------- inline images

    /// A PNG of one colour, encoded with a real encoder so the test exercises a real file.
    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut buffer = image::RgbaImage::new(width, height);
        for pixel in buffer.pixels_mut() {
            *pixel = image::Rgba([30, 120, 200, 255]);
        }
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(buffer)
            .write_to(&mut out, image::ImageFormat::Png)
            .expect("the encoder produces a PNG");
        out.into_inner()
    }

    /// The whole path, from the bytes a process writes to the grid a client renders.
    #[test]
    fn a_picture_a_process_prints_reaches_the_grid_a_client_renders() {
        let mut buffer = TerminalBuffer::new(ScreenSize::new(10, 40));
        let payload = turn_proto::encode_base64(&png(32, 17));
        buffer.write(b"before ");
        buffer.write(format!("\x1b]1337;File=inline=1:{payload}\x07").as_bytes());
        buffer.write(b" after\r\n");

        let grid = buffer.grid();
        assert_eq!(grid.images.len(), 1, "the table reached the grid");
        assert!(grid.has_images());
        let placement = grid.images[0];
        assert_eq!((placement.rows, placement.cols), (1, 4));
        assert_eq!((placement.width, placement.height), (32, 17));

        // The picture occupies cells, and the text after it flows around rather than over.
        let (image, tile) = grid.image_at(0, 7).expect("a tile of the picture");
        assert_eq!(*image, placement);
        assert_eq!(tile, turn_proto::ImageCell::new(0, 0, 0));
        assert_eq!(grid.row_text(0), "before      after");

        // And the payload is there for a client to fetch by id.
        let held = buffer
            .image_payload(placement.id)
            .expect("the pane holds the pixels");
        assert_eq!((held.width, held.height), (32, 17));
        assert_eq!(held.byte_len(), 32 * 17 * 4);
        assert_eq!(buffer.images().placements(), 1);
    }

    /// Not one byte of an image sequence may be printed as text, whichever protocol it is.
    #[test]
    fn none_of_the_three_protocols_leaves_its_escape_sequence_visible() {
        let payload = turn_proto::encode_base64(&png(16, 17));
        for sequence in [
            format!("\x1b]1337;File=inline=1:{payload}\x07"),
            "\x1bP0;1;0q#1!16~\x1b\\".to_string(),
            format!("\x1b_Ga=T,f=100;{payload}\x1b\\"),
        ] {
            let mut buffer = TerminalBuffer::new(ScreenSize::new(6, 40));
            buffer.write(sequence.as_bytes());
            let snapshot = buffer.snapshot();
            let text = snapshot.text();
            assert!(
                !text.contains("File=") && !text.contains("a=T") && !text.contains("!16~"),
                "the sequence was printed as text: {text:?}"
            );
            assert!(
                buffer.grid().has_images(),
                "and the picture was placed: {sequence:?}"
            );
        }
    }

    /// A sequence split across pty reads is the normal case, not the exception.
    #[test]
    fn a_picture_split_across_a_dozen_reads_is_still_one_picture() {
        let payload = turn_proto::encode_base64(&png(32, 34));
        let sequence = format!("\x1b]1337;File=inline=1:{payload}\x07");
        let mut buffer = TerminalBuffer::new(ScreenSize::new(10, 40));
        for chunk in sequence.as_bytes().chunks(7) {
            buffer.write(chunk);
        }
        let grid = buffer.grid();
        assert_eq!(grid.images.len(), 1);
        assert_eq!((grid.images[0].rows, grid.images[0].cols), (2, 4));
    }

    /// The marker is Turn's own bookkeeping. It must not reach a preview, a title, or a
    /// replay fed to a real terminal.
    #[test]
    fn an_image_marker_never_escapes_into_text_a_human_or_a_terminal_reads() {
        let mut buffer = TerminalBuffer::new(ScreenSize::new(6, 40));
        let payload = turn_proto::encode_base64(&png(32, 17));
        buffer.write(b"left");
        buffer.write(format!("\x1b]1337;File=inline=1:{payload}\x07").as_bytes());
        buffer.write(b"right");

        let snapshot = buffer.snapshot();
        assert!(
            !snapshot.text().chars().any(turn_proto::is_marker),
            "a marker reached a preview: {:?}",
            snapshot.text()
        );
        assert_eq!(
            snapshot.lines[0], "left    right",
            "the picture reads as the space it occupies"
        );

        let replay = buffer.replay();
        assert!(
            !String::from_utf8_lossy(&replay)
                .chars()
                .any(turn_proto::is_marker),
            "a marker reached a replay bound for a real terminal"
        );
        // And the replay still rebuilds the text around it.
        let mut rebuilt = TerminalBuffer::new(ScreenSize::new(6, 40));
        rebuilt.write(&replay);
        assert_eq!(rebuilt.snapshot().lines[0], "left    right");
        assert!(
            !rebuilt.grid().has_images(),
            "a replay carries the text, not the pictures: a byte-stream client has no way \
             to fetch them"
        );
    }

    /// The refusal a user has to be able to see. A payload past the limit produces a line
    /// in the pane, not silence.
    #[test]
    fn a_payload_over_the_limit_is_refused_with_a_line_the_user_can_read() {
        let mut buffer = TerminalBuffer::new(ScreenSize::new(8, 60));
        let flood = turn_proto::encode_base64(&vec![0u8; 9 * 1024 * 1024]);
        buffer.write(format!("\x1b]1337;File=inline=1:{flood}\x07").as_bytes());

        let text = buffer.snapshot().text();
        assert!(
            text.contains("image not shown"),
            "the user must be told: {text:?}"
        );
        assert!(text.contains("payload over"), "got {text:?}");
        assert!(!buffer.grid().has_images());
        assert_eq!(buffer.images().refusals(), 1);
        assert!(
            buffer.retained_bytes() <= DEFAULT_BYTE_CAPACITY,
            "and the ring stayed bounded"
        );
    }

    /// A program asking Turn to write a file to the user's disk, or to read one, is refused
    /// and the attempt is visible — the same posture as OSC 52's clipboard requests.
    #[test]
    fn a_request_to_touch_the_filesystem_is_refused_and_shown() {
        let mut buffer = TerminalBuffer::new(ScreenSize::new(8, 60));
        let payload = turn_proto::encode_base64(&png(8, 8));
        // iTerm2 without `inline=1` is a download request.
        buffer.write(format!("\x1b]1337;File=size=99:{payload}\x07").as_bytes());
        assert!(buffer
            .snapshot()
            .text()
            .contains("a download was requested"));

        // Kitty asking Turn to open a path the process chose.
        let mut buffer = TerminalBuffer::new(ScreenSize::new(8, 60));
        let path = turn_proto::encode_base64(b"/etc/passwd");
        buffer.write(format!("\x1b_Ga=T,f=100,t=f;{path}\x1b\\").as_bytes());
        let text = buffer.snapshot().text();
        assert!(text.contains("does not read images from"), "got {text:?}");
        assert!(!buffer.grid().has_images());
    }

    /// Clearing the screen drops the picture, and it does so through the terminal's own
    /// rules rather than through anything this feature had to write.
    #[test]
    fn clearing_the_screen_removes_the_picture_from_the_grid() {
        let mut buffer = TerminalBuffer::new(ScreenSize::new(8, 40));
        let payload = turn_proto::encode_base64(&png(32, 34));
        buffer.write(format!("\x1b]1337;File=inline=1:{payload}\x07").as_bytes());
        assert!(buffer.grid().has_images());

        buffer.write(b"\x1b[2J\x1b[H");
        let grid = buffer.grid();
        assert!(!grid.has_images());
        assert!(
            grid.images.is_empty(),
            "and no table entry survives for a client to fetch"
        );
    }

    /// Scrolling moves the picture with the rows above it, and takes it away at the top.
    #[test]
    fn scrolling_moves_the_picture_and_then_takes_it_away() {
        let mut buffer = TerminalBuffer::new(ScreenSize::new(4, 20));
        let payload = turn_proto::encode_base64(&png(16, 17));
        buffer.write(format!("\x1b]1337;File=inline=1:{payload}\x07").as_bytes());

        let row_of_picture = |buffer: &TerminalBuffer| -> Option<u16> {
            let grid = buffer.grid();
            (0..grid.rows).find(|row| {
                (0..grid.cols).any(|col| grid.cell(*row, col).is_some_and(|c| c.is_image()))
            })
        };
        assert_eq!(row_of_picture(&buffer), Some(0));

        // Three newlines fill the four-row screen without scrolling it.
        buffer.write(b"\r\n\r\n\r\n");
        assert_eq!(row_of_picture(&buffer), Some(0), "still at the top");
        buffer.write(b"\r\n");
        assert_eq!(row_of_picture(&buffer), None, "scrolled off");
        assert!(buffer.grid().images.is_empty());
    }

    /// A picture that scrolled out of the live screen is still in the daemon's history, and
    /// the window of history it serves carries the table that says which picture it is.
    #[test]
    fn a_picture_that_scrolled_into_history_is_still_there_with_its_table() {
        let mut buffer = TerminalBuffer::with_capacity(ScreenSize::new(4, 20), 64 * 1024, 200);
        let payload = turn_proto::encode_base64(&png(16, 17));
        buffer.write(format!("\x1b]1337;File=inline=1:{payload}\x07").as_bytes());
        let placed = buffer.grid().images[0];

        // Push it off the top of the four-row screen.
        buffer.write(b"\r\none\r\ntwo\r\nthree\r\nfour\r\nfive\r\n");
        assert!(
            !buffer.grid().has_images(),
            "the picture has left the live screen"
        );

        // A window of history deep enough to include the row it was on.
        let mut found = None;
        for offset in 1..=8usize {
            let window = buffer.history_grid(offset);
            if window.has_images() {
                found = Some(window);
                break;
            }
        }
        let window = found.expect("the picture is somewhere in the history the daemon holds");
        assert_eq!(
            window.images,
            vec![placed],
            "the window has to say which picture its markers refer to"
        );
        assert!(
            window
                .image_at(0, 0)
                .or_else(|| (0..window.rows)
                    .flat_map(|row| (0..window.cols).map(move |col| (row, col)))
                    .find_map(|(row, col)| window.image_at(row, col)))
                .is_some(),
            "and its tiles resolve"
        );
        // And the payload is still fetchable, so the picture can actually be drawn.
        assert!(buffer.image_payload(placed.id).is_some());
    }

    /// A window of history with no picture in it says nothing about images, so a client is
    /// never asked for pixels it has nowhere to draw.
    #[test]
    fn a_history_window_with_no_picture_in_it_carries_no_table() {
        let mut buffer = TerminalBuffer::with_capacity(ScreenSize::new(4, 20), 64 * 1024, 200);
        let payload = turn_proto::encode_base64(&png(16, 17));
        buffer.write(format!("\x1b]1337;File=inline=1:{payload}\x07").as_bytes());
        for line in 0..40 {
            buffer.write(format!("line {line}\r\n").as_bytes());
        }
        // A shallow window, far below the row the picture was on.
        let window = buffer.history_grid(1);
        assert!(!window.has_images());
        assert!(window.images.is_empty());
    }

    /// A pane whose client has reported its measured cell size resolves a pixel request the
    /// way that client will draw it.
    #[test]
    fn a_reported_cell_size_is_used_for_a_pixel_sized_request() {
        let mut buffer = TerminalBuffer::new(ScreenSize::new(10, 60));
        buffer.set_cell_pixels(16, 34);
        let payload = turn_proto::encode_base64(&png(64, 68));
        buffer.write(
            format!("\x1b]1337;File=inline=1;width=64px;height=68px:{payload}\x07").as_bytes(),
        );
        let grid = buffer.grid();
        assert_eq!((grid.images[0].rows, grid.images[0].cols), (2, 4));
    }

    /// Adversarial input through the real entry point. The only unacceptable outcome is a
    /// panic or unbounded memory.
    #[test]
    fn malformed_image_sequences_never_panic_and_never_grow_without_bound() {
        let mut buffer = TerminalBuffer::with_capacity(ScreenSize::new(12, 60), 64 * 1024, 200);
        for sequence in [
            "\x1b]1337;File=inline=1:".to_string(),
            "\x1b]1337;File=inline=1:!!!!\x07".to_string(),
            "\x1b]1337;File=:\x07".to_string(),
            "\x1b]1337;File=inline=1:Zm9v\x07".to_string(),
            "\x1bP".to_string(),
            "\x1bP0q".to_string(),
            "\x1bP0q\x1b\\".to_string(),
            "\x1b_".to_string(),
            "\x1b_G".to_string(),
            "\x1b_Ga=T,f=32,s=99999,v=99999;AAAA\x1b\\".to_string(),
            "\x1b_Ga=p,i=4242\x1b\\".to_string(),
            "\x1b_Ga=T,f=100,o=z;AAAA\x1b\\".to_string(),
            format!("\x1b]1337;File=inline=1;width={}:AAAA\x07", "9".repeat(40)),
        ] {
            buffer.write(sequence.as_bytes());
        }
        // A stream of nothing but introducers.
        buffer.write(&vec![0x1B; 20_000]);
        // And a deterministic soup, so a failure is reproducible.
        let mut state = 0x1234_5678_9ABC_DEF0u64;
        let mut soup = Vec::with_capacity(64 * 1024);
        for _ in 0..64 * 1024 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            soup.push((state & 0xFF) as u8);
        }
        buffer.write(&soup);

        assert!(buffer.retained_bytes() <= 64 * 1024);
        assert!(buffer.images().stored_bytes() <= crate::images::MAX_STORE_BYTES);
        let grid = buffer.grid();
        assert_eq!(grid.rows, 12);
        assert!(grid.images.len() <= turn_proto::MAX_PLACED_IMAGES);
    }

    /// The one hole no per-picture limit closes: a stream of small sequences that each
    /// decode to a lot of pixels.
    ///
    /// The guarantee is not "some picture is refused" — a well-compressing picture may
    /// legitimately pay its way — it is that **total decoding stays linear in the bytes the
    /// process wrote**. That is what stops a process from spending the machine's memory
    /// bandwidth for the price of a few kilobytes.
    #[test]
    fn total_decoding_stays_linear_in_the_bytes_the_process_wrote() {
        let mut buffer = TerminalBuffer::new(ScreenSize::new(40, 120));
        let payload = turn_proto::encode_base64(&png(1_024, 1_024));
        let sequence = format!("\x1b]1337;File=inline=1;width=2;height=1:{payload}\x07\r\n");
        for _ in 0..80 {
            buffer.write(sequence.as_bytes());
        }

        let allowed = crate::images::PIXELS_PER_INPUT_BYTE
            .saturating_mul(buffer.bytes_seen())
            .saturating_add(turn_proto::MAX_IMAGE_PIXELS as u64);
        assert!(
            buffer.images().decoded_pixels() <= allowed,
            "{} pixels were decoded for {} bytes of input, over the {allowed} allowed",
            buffer.images().decoded_pixels(),
            buffer.bytes_seen()
        );
        assert!(
            buffer.images().decoded_pixels() > 0,
            "and the pictures that paid their way were shown"
        );
        assert!(buffer.images().stored_bytes() <= crate::images::MAX_STORE_BYTES);
    }

    /// The same bound, from the other side: a picture whose pixels cost far more than the
    /// bytes that carried it runs the budget out and is refused with a line the user reads.
    #[test]
    fn a_picture_that_costs_far_more_than_it_carried_exhausts_the_budget_and_says_so() {
        let mut buffer = TerminalBuffer::new(ScreenSize::new(12, 60));
        // A Sixel is uncompressed, but run-length makes it dense: a few bytes per band of a
        // thousand columns. Repeated, it asks for far more pixels than it paid for.
        let mut body = Vec::from(b"\x1bP0;1;0q#1".as_slice());
        for _ in 0..60 {
            body.extend_from_slice(b"!1000~-");
        }
        body.extend_from_slice(b"\x1b\\");
        // Five of them: enough for the budget to run out while the notice is still on
        // screen rather than scrolled away by the pictures that did fit.
        let mut refused_text = None;
        for _ in 0..5 {
            buffer.write(&body);
            let text = buffer.snapshot().text();
            if text.contains("image not shown") {
                refused_text = Some(text);
                break;
            }
        }
        let text = refused_text.expect("the budget must run out within five megapixel Sixels");
        assert!(
            text.contains("too many images too quickly"),
            "the user must be told why a picture stopped appearing: {text:?}"
        );
        assert!(buffer.images().refusals() > 0);
        let allowed = crate::images::PIXELS_PER_INPUT_BYTE
            .saturating_mul(buffer.bytes_seen())
            .saturating_add(turn_proto::MAX_IMAGE_PIXELS as u64);
        assert!(buffer.images().decoded_pixels() <= allowed);
    }
}
