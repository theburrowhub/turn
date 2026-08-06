//! OSC 8 hyperlinks: the escape sequence `vt100` does not implement.
//!
//! `ESC ] 8 ; params ; URI ST` opens a hyperlink, everything printed until
//! `ESC ] 8 ; ; ST` carries it, and the text may say anything at all — "the PR", "here",
//! or another URL entirely. That is the whole point of the mechanism and it is also why it
//! needs care: the label is written by the process and the destination is not visible.
//! Nothing here decides whether a link may be *opened*; this module's job is to know
//! **which cells** a link covers, and to be honest when it does not.
//!
//! ## Why the tracking is not simply "remember the cursor"
//!
//! [`vt100::Callbacks`] hands the escape to us with the screen as it stands, so the cursor
//! at the open and at the close bound the text. But the screen keeps moving underneath:
//! the next hundred lines of a build scroll that text upwards, and marks recorded at row 5
//! would end up pointing at whatever landed on row 5 afterwards — a link on unrelated
//! text, which is worse than no link.
//!
//! So the tracker keeps a mark per cell and **a copy of each row's text**, and realigns
//! itself whenever it is shown the screen again. Two rules, and between them they never
//! leave a mark on text it was not declared over:
//!
//! * **Where the text went.** If a shift explains the screen — every row equal to the row
//!   `k` further down the one remembered — the marks move up by `k` with their text. This is
//!   the same rule the client's scrollback uses, and it is believed only when it is exact.
//! * **How much of a row survived.** A row is compared with the row it is predicted to be,
//!   and marks from the first differing column onwards are **dropped**. That is what makes
//!   the common case work: a program writing a second link further along a row has *added*
//!   to it, so everything before the addition is provably untouched and its link stays —
//!   while a program that repainted the row keeps nothing, because nothing before the first
//!   difference survived.
//!
//! Comparing by column rather than by row is the difference between `ls --hyperlink` working
//! and only its last filename being clickable.
//!
//! ## What it costs, and why that is nothing for most panes
//!
//! Realigning reads the screen's rows as text, so it is never done speculatively:
//!
//! * A pane that has never seen a hyperlink is [`LinkTracker::is_idle`] and does no work at
//!   all — no mark grid is even allocated. That is nearly every pane.
//! * Inside one write the number of realignments is capped
//!   ([`LinkTracker::begin_write`]), so a program that emits ten thousand hyperlinks in one
//!   burst cannot turn each one into a scan of the screen. The realignment at the end of the
//!   write is not capped, so the marks are always in step by the time anyone reads them.
//!
//! ## What is refused
//!
//! A URI that is empty, longer than [`MAX_LINK_URI_CHARS`], or carries a control character
//! or whitespace is refused at capture and counted. A link whose extent cannot be
//! proved — the close arrived before the open in reading order, or a second open arrived
//! with one still unclosed — is abandoned and counted. Both counters exist so the UI can say
//! a process tried something odd rather than leaving it invisible.

use std::sync::Arc;

/// Longest URI a hyperlink may carry, matching the protocol's own cap.
///
/// Not truncated when it is longer: half a URL is a different URL, and offering to open one
/// would be the exact failure this module exists to avoid.
pub const MAX_LINK_URI_CHARS: usize = 4_096;

/// Most distinct URIs one pane keeps at a time.
///
/// Past this the table is compacted against the marks that are still on screen, so a
/// session that has scrolled a million links past does not retain a million strings.
pub const MAX_TRACKED_URIS: usize = 512;

/// How many times one write may realign the marks against the screen.
///
/// A hostile program can emit hyperlinks as fast as it can emit anything else, and each one
/// arriving as a fresh read of the screen would let it spend the daemon's CPU. Sixty-four is
/// far more than real output produces between two scrolls, and the realignment at the end of
/// the write is not budgeted, so the marks are always in step by the time anyone reads them.
pub const REALIGN_BUDGET_PER_WRITE: u32 = 64;

/// The mark on a cell with no hyperlink.
const NO_LINK: u32 = u32::MAX;

/// One hyperlink over a half-open range of columns of one row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkSpan {
    pub row: u16,
    /// First column, inclusive.
    pub from: u16,
    /// Last column, exclusive.
    pub to: u16,
    pub uri: Arc<str>,
}

/// The hyperlink still being written, and where it started.
#[derive(Debug, Clone)]
struct Open {
    uri: Arc<str>,
    row: u16,
    col: u16,
}

/// Which cells of a pane's screen carry which hyperlink.
#[derive(Debug, Default)]
pub struct LinkTracker {
    rows: u16,
    cols: u16,
    /// Row-major, `rows * cols` entries of an index into [`Self::uris`], or [`NO_LINK`].
    /// Allocated only once a link actually arrives.
    marks: Vec<u32>,
    /// How many cells carry a mark, so "is there anything to keep aligned" is an integer
    /// comparison rather than a scan of the screen.
    marked: usize,
    uris: Vec<Arc<str>>,
    /// The screen's rows as text, as they were when the marks were last checked.
    ///
    /// Text rather than a hash per row: the question at realignment time is not "did this
    /// row change" but "how much of it is still the row I marked", and only the text can
    /// answer the second one. It also costs nothing extra — reading the screen produces
    /// these strings either way.
    witness: Vec<String>,
    open: Option<Open>,
    /// Realignments left in the current write.
    budget: u32,
    /// URIs the process declared that Turn would not keep.
    refused: u32,
    /// Links whose place on the screen could not be proved, and so were dropped.
    abandoned: u32,
}

impl LinkTracker {
    pub fn new(rows: u16, cols: u16) -> Self {
        Self {
            rows,
            cols,
            ..Self::default()
        }
    }

    /// Forgets everything, which is the only honest response to a reflow.
    ///
    /// A resize moves every character on the screen and `vt100` reflows wrapped rows as it
    /// sees fit. Marks kept across that would sit on text they were never declared over.
    pub fn resize(&mut self, rows: u16, cols: u16) {
        self.rows = rows;
        self.cols = cols;
        self.forget();
        if self.open.take().is_some() {
            self.abandoned = self.abandoned.saturating_add(1);
        }
    }

    /// Drops every mark and every URI, keeping the geometry and the counters.
    fn forget(&mut self) {
        self.marks.clear();
        self.marked = 0;
        self.uris.clear();
        self.witness.clear();
    }

    /// Starts a write's realignment budget. Called once per write, before parsing.
    pub fn begin_write(&mut self, budget: u32) {
        self.budget = budget;
    }

    /// Whether it is worth reading the screen's rows for the escape about to be handled.
    ///
    /// False when there is nothing to keep aligned, and false once a write has spent its
    /// budget. A caller that gets false passes `None`, and the tracker works without the
    /// information rather than pretending to have it.
    pub fn wants_rows(&self) -> bool {
        self.budget > 0 && !self.is_idle()
    }

    /// Whether there is nothing to keep aligned, so a caller can skip reading the screen.
    ///
    /// This is what makes hyperlink tracking free for the overwhelming majority of panes:
    /// no link has arrived, so no work is done per write.
    pub fn is_idle(&self) -> bool {
        self.marked == 0 && self.open.is_none()
    }

    /// How many URIs the process declared that Turn refused to keep.
    pub fn refused(&self) -> u32 {
        self.refused
    }

    /// How many links were dropped because their place on the screen could not be proved.
    pub fn abandoned(&self) -> u32 {
        self.abandoned
    }

    /// Whether a hyperlink is open, so text printed now would carry it.
    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// How many cells carry a hyperlink.
    pub fn marked_cells(&self) -> usize {
        self.marked
    }

    /// Every mark on the screen, as spans in reading order.
    ///
    /// Adjacent cells carrying the same URI become one span, which is both what a client
    /// wants and what makes the spans of a row non-overlapping by construction.
    pub fn spans(&self) -> Vec<LinkSpan> {
        let mut spans: Vec<LinkSpan> = Vec::new();
        if self.marked == 0 {
            return spans;
        }
        for row in 0..self.rows {
            let mut col = 0u16;
            while col < self.cols {
                let mark = self.mark(row, col);
                if mark == NO_LINK {
                    col += 1;
                    continue;
                }
                let from = col;
                while col < self.cols && self.mark(row, col) == mark {
                    col += 1;
                }
                if let Some(uri) = self.uris.get(mark as usize) {
                    spans.push(LinkSpan {
                        row,
                        from,
                        to: col,
                        uri: Arc::clone(uri),
                    });
                }
            }
        }
        spans
    }

    /// Opens a hyperlink at a cursor position, after realigning to the screen it is on.
    ///
    /// `rows` is the screen as it stands, one string per row, or `None` when the caller
    /// decided ([`Self::wants_rows`]) that reading it was not worth the work.
    pub fn open_link(&mut self, uri: &str, cursor: (u16, u16), rows: Option<&[String]>) {
        self.spend();
        self.realign(rows);
        if self.open.take().is_some() {
            // A second open without a close. The first link's extent is unknowable, so it is
            // dropped rather than guessed at.
            self.abandoned = self.abandoned.saturating_add(1);
        }
        let Some(uri) = self.acceptable(uri) else {
            return;
        };
        self.open = Some(Open {
            uri,
            row: cursor.0,
            col: cursor.1,
        });
    }

    /// Closes the open hyperlink, marking the cells it turned out to cover.
    pub fn close_link(&mut self, cursor: (u16, u16), rows: Option<&[String]>) {
        let shift = self.realign(rows);
        self.spend();
        let Some(open) = self.open.take() else {
            // A close with nothing open is what a program emits when it resets its own state
            // defensively. Nothing to record, and not a fault.
            return;
        };
        // The rows above the link's start moved up by `shift`, and so did the link.
        let Some(start_row) = open.row.checked_sub(shift as u16) else {
            self.abandoned = self.abandoned.saturating_add(1);
            return;
        };
        let cols = self.cols as usize;
        let start = start_row as usize * cols + open.col as usize;
        let end = cursor.0 as usize * cols + cursor.1 as usize;
        if end <= start || end > self.rows as usize * cols {
            // The close landed before the open in reading order, which happens when the
            // program moved the cursor backwards inside its own link, or when the screen
            // scrolled further than the rows above the link could prove. Either way the
            // extent is not known, so nothing is marked.
            self.abandoned = self.abandoned.saturating_add(1);
            return;
        }
        let index = self.intern(open.uri);
        self.ensure_marks();
        for cell in start..end {
            self.set_mark(cell, index);
        }
    }

    /// Realigns the marks at the end of a write, whatever the budget said.
    ///
    /// The budget bounds how much a *program* can make Turn do between two escapes; it must
    /// not leave the marks out of step once the write is over, because that is the moment the
    /// daemon reads them to build a grid.
    pub fn settle(&mut self, rows: &[String]) {
        self.realign(Some(rows));
    }

    fn spend(&mut self) {
        self.budget = self.budget.saturating_sub(1);
    }

    /// How far up the screen a shift may be looked for.
    ///
    /// While a link is open, only the rows *above* where it started: the link's own text is
    /// being written as we watch, so those rows have legitimately changed and comparing them
    /// would hide a shift rather than reveal it.
    fn anchor_limit(&self) -> usize {
        match &self.open {
            Some(open) => open.row as usize,
            None => self.rows as usize,
        }
    }

    /// Moves the marks to wherever their text went, and drops the ones whose text is gone.
    ///
    /// Returns how many rows the screen is judged to have scrolled, which is what lets an
    /// open link's start position follow its own text.
    fn realign(&mut self, rows: Option<&[String]>) -> usize {
        let Some(rows) = rows else {
            return 0;
        };
        if rows.len() != self.rows as usize {
            // A screen of an unexpected shape means a resize this tracker was not told
            // about. Keeping marks would be worse than losing them.
            self.forget();
            self.witness.extend_from_slice(rows);
            return 0;
        }
        let shift = detect_shift(&self.witness, rows, self.anchor_limit()).unwrap_or(0);
        if self.marked > 0 {
            if shift > 0 {
                self.shift_marks(shift);
            }
            for row in 0..self.rows {
                // The row this one is predicted to be. Absent for the rows a scroll has just
                // exposed at the bottom, which carry nothing that was ever marked.
                let kept = match self.witness.get(row as usize + shift) {
                    Some(before) => surviving_columns(before, &rows[row as usize]),
                    None => 0,
                };
                self.clear_row_from(row, kept);
            }
        }
        self.witness.clear();
        self.witness.extend_from_slice(rows);
        shift
    }

    /// Moves every mark up by `rows` rows, clearing the rows exposed at the bottom.
    fn shift_marks(&mut self, rows: usize) {
        if self.marks.is_empty() {
            return;
        }
        let cols = self.cols as usize;
        let offset = rows.saturating_mul(cols);
        if offset >= self.marks.len() {
            self.marks.fill(NO_LINK);
            self.marked = 0;
            return;
        }
        // Counted rather than recomputed: the marks leaving the top are the only ones lost.
        let lost = self.marks[..offset]
            .iter()
            .filter(|mark| **mark != NO_LINK)
            .count();
        self.marks.copy_within(offset.., 0);
        let keep = self.marks.len() - offset;
        self.marks[keep..].fill(NO_LINK);
        self.marked = self.marked.saturating_sub(lost);
    }

    /// Drops the marks on a row from a column onwards.
    fn clear_row_from(&mut self, row: u16, col: u16) {
        if col >= self.cols {
            return;
        }
        let cols = self.cols as usize;
        let start = row as usize * cols + col as usize;
        let end = (row as usize + 1) * cols;
        let Some(slice) = self.marks.get_mut(start..end) else {
            return;
        };
        let mut cleared = 0usize;
        for mark in slice.iter_mut() {
            if *mark != NO_LINK {
                *mark = NO_LINK;
                cleared += 1;
            }
        }
        self.marked = self.marked.saturating_sub(cleared);
    }

    fn mark(&self, row: u16, col: u16) -> u32 {
        let index = row as usize * self.cols as usize + col as usize;
        self.marks.get(index).copied().unwrap_or(NO_LINK)
    }

    fn set_mark(&mut self, cell: usize, index: u32) {
        if let Some(slot) = self.marks.get_mut(cell) {
            if *slot == NO_LINK {
                self.marked += 1;
            }
            *slot = index;
        }
    }

    fn ensure_marks(&mut self) {
        let wanted = self.rows as usize * self.cols as usize;
        if self.marks.len() != wanted {
            self.marks = vec![NO_LINK; wanted];
            self.marked = 0;
        }
    }

    /// Whether a URI is one Turn will keep, and the shared handle for it if so.
    ///
    /// Whitespace and control characters are refused rather than escaped. A URI cannot
    /// legally contain either, so their presence means the process is either broken or trying
    /// to smuggle something through a string that will be shown to a human.
    fn acceptable(&mut self, uri: &str) -> Option<Arc<str>> {
        let refuse = uri.is_empty()
            || uri.chars().count() > MAX_LINK_URI_CHARS
            || uri
                .chars()
                .any(|c| c.is_control() || c.is_whitespace() || c == '\u{7f}');
        if refuse {
            self.refused = self.refused.saturating_add(1);
            return None;
        }
        Some(Arc::from(uri))
    }

    /// The index of a URI in the table, adding it if it is new.
    fn intern(&mut self, uri: Arc<str>) -> u32 {
        if let Some(index) = self.uris.iter().position(|held| **held == *uri) {
            return index as u32;
        }
        if self.uris.len() >= MAX_TRACKED_URIS {
            self.compact();
        }
        match self.uris.len() >= MAX_TRACKED_URIS {
            // Still full after compaction: every slot is referred to by a cell on screen,
            // which takes a screen of nothing but distinct one-cell links. Reusing a slot
            // would silently change where those cells point, so they lose their mark instead.
            true => {
                self.release_slot(0);
                self.uris[0] = uri;
                0
            }
            false => {
                self.uris.push(uri);
                (self.uris.len() - 1) as u32
            }
        }
    }

    /// Unmarks every cell referring to a slot, so the slot can be reused honestly.
    fn release_slot(&mut self, index: u32) {
        let mut cleared = 0usize;
        for mark in self.marks.iter_mut() {
            if *mark == index {
                *mark = NO_LINK;
                cleared += 1;
            }
        }
        self.marked = self.marked.saturating_sub(cleared);
    }

    /// Drops URIs no cell refers to any more, renumbering the marks that survive.
    fn compact(&mut self) {
        let mut moved = vec![NO_LINK; self.uris.len()];
        let mut kept: Vec<Arc<str>> = Vec::new();
        let mut lost = 0usize;
        for mark in self.marks.iter_mut() {
            let old = *mark;
            if old == NO_LINK {
                continue;
            }
            match moved.get(old as usize).copied() {
                Some(NO_LINK) => match self.uris.get(old as usize) {
                    Some(uri) => {
                        kept.push(Arc::clone(uri));
                        let fresh = (kept.len() - 1) as u32;
                        moved[old as usize] = fresh;
                        *mark = fresh;
                    }
                    None => {
                        *mark = NO_LINK;
                        lost += 1;
                    }
                },
                Some(fresh) => *mark = fresh,
                None => {
                    *mark = NO_LINK;
                    lost += 1;
                }
            }
        }
        self.marked = self.marked.saturating_sub(lost);
        self.uris = kept;
    }
}

/// How many rows the screen scrolled, when a shift alone explains the top `limit` rows.
///
/// `Some(0)` for a screen whose top rows did not move at all, which is the common case and
/// the one worth answering fastest. `None` when no shift explains it — a repaint, or a row
/// that merely grew — and when there is nothing to compare, because a shift believed on no
/// evidence is how marks end up on unrelated text. A caller that gets `None` treats the
/// screen as not having scrolled, and then finds out per column how much of each row
/// survived, which is where the honesty actually comes from.
fn detect_shift(previous: &[String], next: &[String], limit: usize) -> Option<usize> {
    let limit = limit.min(previous.len()).min(next.len());
    if limit == 0 {
        return None;
    }
    (0..limit).find(|shift| {
        let overlap = limit - shift;
        (0..overlap).all(|row| previous[row + shift] == next[row])
    })
}

/// How many of a row's leading columns still hold what they held.
///
/// The count is of *characters*, and a double-width glyph is one character in two columns,
/// so a row containing one is judged to have kept fewer columns than it really did. That
/// errs towards dropping a mark that could have been kept, which is the direction to err in:
/// the alternative is a link on text somebody else wrote.
fn surviving_columns(before: &str, now: &str) -> u16 {
    let common = before
        .chars()
        .zip(now.chars())
        .take_while(|(was, is)| was == is)
        .count();
    u16::try_from(common).unwrap_or(u16::MAX)
}

/// A screen's visible rows as text, for noticing what a row has kept.
///
/// Trailing blanks are trimmed by the parser, which suits this exactly: the padding past a
/// row's last character is not text anybody wrote and not text anybody can hover.
pub fn screen_rows(screen: &vt100::Screen) -> Vec<String> {
    let (_, cols) = screen.size();
    screen.rows(0, cols).collect()
}

/// The URI of an OSC 8 sequence, or `None` when the sequence closes one.
///
/// `vt100` hands OSC parameters split on `;`, and a URI may legitimately contain one — a
/// query string of `a;b` is unusual but valid — so everything after the parameter field is
/// rejoined rather than only the third element being read. The parameter field itself
/// (`id=…`) is deliberately ignored: it exists to group spans of one logical link, and this
/// tracker already knows which cells a link covers.
pub fn parse_osc8(params: &[&[u8]]) -> Option<Option<String>> {
    let [b"8", _params, rest @ ..] = params else {
        return None;
    };
    let joined = rest.join(&b';');
    if joined.is_empty() {
        return Some(None);
    }
    // Lossy on purpose: a URI is ASCII by definition and an invalid byte is a defect in the
    // producer. Replacement characters make it visible instead of silently changing which
    // host the string names.
    Some(Some(String::from_utf8_lossy(&joined).into_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A screen of `rows` rows holding the given lines, as the parser would report it.
    fn screen(lines: &[&str], rows: usize) -> Vec<String> {
        (0..rows)
            .map(|row| lines.get(row).copied().unwrap_or("").to_string())
            .collect()
    }

    /// A tracker with a budget, as a buffer gives it one per write.
    fn tracker(rows: u16, cols: u16) -> LinkTracker {
        let mut tracker = LinkTracker::new(rows, cols);
        tracker.begin_write(REALIGN_BUDGET_PER_WRITE);
        tracker
    }

    fn spans(tracker: &LinkTracker) -> Vec<(u16, u16, u16, String)> {
        tracker
            .spans()
            .into_iter()
            .map(|span| (span.row, span.from, span.to, span.uri.to_string()))
            .collect()
    }

    #[test]
    fn an_osc_eight_sequence_is_recognised_as_opening_or_closing_a_link() {
        assert_eq!(
            parse_osc8(&[b"8", b"", b"https://example.com/pr/1"]),
            Some(Some("https://example.com/pr/1".to_string()))
        );
        assert_eq!(
            parse_osc8(&[b"8", b"id=42", b"ssh://host"]),
            Some(Some("ssh://host".to_string())),
            "the id parameter groups spans and is not part of the target"
        );
        assert_eq!(parse_osc8(&[b"8", b"", b""]), Some(None), "a close");
        assert_eq!(parse_osc8(&[b"8", b""]), Some(None), "a truncated close");
        assert_eq!(
            parse_osc8(&[b"8", b"", b"https://x.example/a", b"b=c"]),
            Some(Some("https://x.example/a;b=c".to_string())),
            "a semicolon inside a URI must not truncate it"
        );
        assert_eq!(parse_osc8(&[b"52", b"c", b"data"]), None, "not a hyperlink");
        assert_eq!(parse_osc8(&[b"8"]), None);
    }

    /// The ordinary case: a link opened, some text, a close.
    #[test]
    fn a_link_covers_exactly_the_cells_written_while_it_was_open() {
        let mut tracker = tracker(4, 10);
        tracker.open_link(
            "https://example.com/pr/9",
            (0, 4),
            Some(&screen(&["see"], 4)),
        );
        tracker.close_link((0, 6), Some(&screen(&["see PR"], 4)));

        assert_eq!(
            spans(&tracker),
            vec![(0, 4, 6, "https://example.com/pr/9".to_string())]
        );
        assert_eq!(tracker.refused(), 0);
        assert_eq!(tracker.abandoned(), 0);
        assert!(!tracker.is_idle(), "a pane with a link has work to do");
    }

    /// A link whose text ran off the right margin is one link over two rows, which is what
    /// lets a client join them back together.
    #[test]
    fn a_link_that_wrapped_at_the_margin_is_marked_on_both_rows() {
        let mut tracker = tracker(4, 10);
        tracker.open_link(
            "https://example.com/a/very/long/path",
            (0, 4),
            Some(&screen(&["    "], 4)),
        );
        // Six columns on row 0, then eight on row 1.
        tracker.close_link((1, 8), Some(&screen(&["    123456", "78901234"], 4)));

        assert_eq!(
            spans(&tracker),
            vec![
                (0, 4, 10, "https://example.com/a/very/long/path".to_string()),
                (1, 0, 8, "https://example.com/a/very/long/path".to_string()),
            ]
        );
    }

    /// The case the whole design is for: a build that keeps printing must carry its links up
    /// the screen with the text they belong to, and lose them when the text leaves.
    #[test]
    fn marks_follow_their_text_up_the_screen_and_off_it() {
        let mut tracker = tracker(4, 10);
        let start = screen(&["one", "two", "PR", "three"], 4);
        tracker.open_link("https://example.com/pr/9", (2, 0), Some(&start));
        tracker.close_link((2, 2), Some(&start));
        assert_eq!(spans(&tracker)[0].0, 2);

        // Two more lines arrive, and each pushes the screen up by one row.
        tracker.settle(&screen(&["two", "PR", "three", "four"], 4));
        assert_eq!(spans(&tracker)[0].0, 1);
        tracker.settle(&screen(&["PR", "three", "four", "five"], 4));
        assert_eq!(
            spans(&tracker),
            vec![(0, 0, 2, "https://example.com/pr/9".to_string())]
        );

        tracker.settle(&screen(&["three", "four", "five", "six"], 4));
        assert!(
            tracker.spans().is_empty(),
            "the link left with the text it belonged to"
        );
        assert!(tracker.is_idle(), "and nothing is left to keep aligned");
    }

    /// A link is only ever attached to the text it was declared over. When a row is
    /// repainted the mark goes, because whatever is there now was never linked.
    #[test]
    fn a_row_that_was_repainted_loses_its_marks_rather_than_keeping_them() {
        let mut tracker = tracker(3, 10);
        tracker.open_link("https://example.com/x", (1, 0), Some(&screen(&[""], 3)));
        tracker.close_link((1, 4), Some(&screen(&["", "link"], 3)));
        assert_eq!(spans(&tracker).len(), 1);

        tracker.settle(&screen(&["", "other"], 3));
        assert!(
            tracker.spans().is_empty(),
            "a mark on repainted text is a link on the wrong words"
        );
    }

    /// The rule that makes `ls --hyperlink` work: a row that *grew* kept everything before
    /// the growth, so the links already on it survive.
    #[test]
    fn several_links_written_along_one_row_all_survive_because_the_row_only_grew() {
        let mut tracker = tracker(2, 40);
        let names = ["one.rs", "two.rs", "three.rs"];
        let mut text = String::new();
        for (index, name) in names.iter().enumerate() {
            let at = text.chars().count() as u16;
            tracker.open_link(
                &format!("file:///repo/{name}"),
                (0, at),
                Some(&screen(&[&text], 2)),
            );
            text.push_str(name);
            let end = text.chars().count() as u16;
            tracker.close_link((0, end), Some(&screen(&[&text], 2)));
            text.push(' ');
            assert_eq!(
                spans(&tracker).len(),
                index + 1,
                "link {index} lost an earlier one: {:?}",
                spans(&tracker)
            );
        }

        let spans = spans(&tracker);
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0], (0, 0, 6, "file:///repo/one.rs".to_string()));
        assert_eq!(spans[1], (0, 7, 13, "file:///repo/two.rs".to_string()));
        assert_eq!(spans[2], (0, 14, 22, "file:///repo/three.rs".to_string()));
    }

    /// A row rewritten from a column onwards keeps the links before that column and loses
    /// the ones after it. Column granularity is the whole point.
    #[test]
    fn a_row_rewritten_part_way_along_keeps_only_the_links_before_the_change() {
        let mut tracker = tracker(2, 20);
        tracker.open_link("https://first.example", (0, 0), Some(&screen(&[""], 2)));
        tracker.close_link((0, 4), Some(&screen(&["1234"], 2)));
        tracker.open_link(
            "https://second.example",
            (0, 4),
            Some(&screen(&["1234"], 2)),
        );
        tracker.close_link((0, 8), Some(&screen(&["12345678"], 2)));
        assert_eq!(spans(&tracker).len(), 2);

        // Columns 4 onwards are redrawn with something else.
        tracker.settle(&screen(&["1234wxyz"], 2));
        assert_eq!(
            spans(&tracker),
            vec![(0, 0, 4, "https://first.example".to_string())],
            "the link over the untouched columns survives and the other does not"
        );
    }

    /// A URI a human will be shown must not be able to carry an escape sequence or a
    /// newline, and a megabyte of URI must not be retained per pane.
    #[test]
    fn a_hostile_uri_is_refused_at_capture_and_counted() {
        let mut tracker = tracker(2, 10);
        let blank = screen(&[""], 2);
        for hostile in [
            String::new(),
            "https://a.example/\u{1b}]0;title\u{7}".to_string(),
            "https://a.example/a b".to_string(),
            "https://a.example/\nsecond".to_string(),
            "https://a.example/\u{7f}".to_string(),
            format!("https://a.example/{}", "x".repeat(MAX_LINK_URI_CHARS)),
        ] {
            tracker.open_link(&hostile, (0, 0), Some(&blank));
            assert!(!tracker.is_open(), "{hostile:?} must not open a link");
            tracker.close_link((0, 4), Some(&blank));
        }
        assert_eq!(tracker.refused(), 6);
        assert!(tracker.spans().is_empty());
        assert_eq!(
            tracker.abandoned(),
            0,
            "a refused URI is not an abandoned link"
        );
    }

    /// A close that arrives before its own open, which is what a program that moved the
    /// cursor backwards produces. The extent is unknowable, so nothing is marked.
    #[test]
    fn a_link_whose_extent_cannot_be_proved_is_abandoned_rather_than_guessed_at() {
        let mut tracker = tracker(4, 10);
        let blank = screen(&[""], 4);
        tracker.open_link("https://a.example", (2, 5), Some(&blank));
        tracker.close_link((0, 0), Some(&blank));
        assert!(tracker.spans().is_empty());
        assert_eq!(tracker.abandoned(), 1);

        // An open with no close, followed by another open: the first is dropped.
        tracker.open_link("https://b.example", (0, 0), Some(&blank));
        tracker.open_link("https://c.example", (1, 0), Some(&blank));
        assert_eq!(tracker.abandoned(), 2);
        tracker.close_link((1, 4), Some(&screen(&["", "text"], 4)));
        assert_eq!(
            spans(&tracker),
            vec![(1, 0, 4, "https://c.example".to_string())]
        );

        // A close with nothing open is not a fault; programs emit it defensively.
        let before = tracker.abandoned();
        tracker.close_link((1, 6), Some(&screen(&["", "text"], 4)));
        assert_eq!(tracker.abandoned(), before);
    }

    /// A pane that never sees a hyperlink must not pay for the machinery.
    #[test]
    fn a_pane_with_no_links_stays_idle_and_allocates_nothing() {
        let tracker = LinkTracker::new(40, 120);
        assert!(tracker.is_idle());
        assert!(
            !tracker.wants_rows(),
            "an idle tracker must not ask for the screen to be read"
        );
        assert!(
            tracker.marks.is_empty(),
            "no mark grid until a link arrives"
        );
        assert!(tracker.spans().is_empty());
    }

    /// A program emitting hyperlinks as fast as it can emit anything else must not be able
    /// to turn each one into a scan of the screen.
    #[test]
    fn the_realignment_work_one_write_can_ask_for_is_bounded() {
        let mut tracker = tracker(4, 10);
        let held = screen(&["ab"], 4);
        tracker.open_link("https://a.example", (0, 0), Some(&screen(&[""], 4)));
        tracker.close_link((0, 2), Some(&held));
        assert!(tracker.wants_rows(), "there are marks to keep aligned");

        for _ in 0..REALIGN_BUDGET_PER_WRITE {
            tracker.open_link("https://b.example", (1, 0), Some(&held));
            tracker.close_link((1, 2), Some(&held));
        }
        assert!(!tracker.wants_rows(), "the write has spent its budget");

        // Settling is never budgeted, and the next write starts with a fresh one.
        tracker.settle(&held);
        assert!(!tracker.spans().is_empty(), "the marks are still there");
        tracker.begin_write(REALIGN_BUDGET_PER_WRITE);
        assert!(tracker.wants_rows());
    }

    /// Working without the screen must still produce a link, because the common case — the
    /// very first link on a screen that has not scrolled — is exactly that case.
    #[test]
    fn a_link_is_still_placed_when_the_screen_was_not_read() {
        let mut tracker = tracker(4, 10);
        tracker.open_link("https://a.example", (2, 1), None);
        tracker.close_link((2, 5), None);
        assert_eq!(
            spans(&tracker),
            vec![(2, 1, 5, "https://a.example".to_string())]
        );
    }

    /// A resize reflows every row, so keeping marks would put links on unrelated text.
    #[test]
    fn a_resize_forgets_the_links_because_the_text_moved_underneath_them() {
        let mut tracker = tracker(4, 10);
        tracker.open_link("https://a.example", (0, 0), Some(&screen(&[""], 4)));
        tracker.close_link((0, 4), Some(&screen(&["text"], 4)));
        assert_eq!(spans(&tracker).len(), 1);

        tracker.resize(8, 40);
        assert!(tracker.spans().is_empty());
        assert!(tracker.is_idle());

        // And a screen of the wrong shape is treated the same way: a tracker that was not
        // told about a resize forgets rather than guessing.
        tracker.open_link("https://b.example", (0, 0), Some(&screen(&[""], 8)));
        tracker.close_link((0, 4), Some(&screen(&["text"], 8)));
        assert_eq!(spans(&tracker).len(), 1);
        tracker.settle(&screen(&["text"], 3));
        assert!(tracker.spans().is_empty());
    }

    /// A million links scrolling past must not retain a million strings.
    #[test]
    fn the_uri_table_is_compacted_against_what_is_still_on_screen() {
        let mut tracker = tracker(4, 10);
        for index in 0..(MAX_TRACKED_URIS * 3) {
            let text = format!("row {index}");
            tracker.open_link(
                &format!("https://example.com/{index}"),
                (0, 0),
                Some(&screen(&[""], 4)),
            );
            tracker.close_link((0, 4), Some(&screen(&[&text], 4)));
        }
        assert!(
            tracker.uris.len() <= MAX_TRACKED_URIS,
            "the table grew to {}",
            tracker.uris.len()
        );
        // The newest link is the one on screen, and it is the right one.
        let spans = spans(&tracker);
        assert_eq!(spans.len(), 1);
        assert_eq!(
            spans[0].3,
            format!("https://example.com/{}", MAX_TRACKED_URIS * 3 - 1)
        );
        assert_eq!(tracker.marked_cells(), 4);
    }

    #[test]
    fn a_shift_is_only_believed_when_there_is_something_to_prove_it_with() {
        let previous = screen(&["a", "b", "c", "d"], 4);
        assert_eq!(detect_shift(&previous, &previous, 4), Some(0));
        assert_eq!(
            detect_shift(&previous, &screen(&["b", "c", "d", "e"], 4), 4),
            Some(1)
        );
        assert_eq!(
            detect_shift(&previous, &screen(&["c", "d", "e", "f"], 4), 4),
            Some(2)
        );
        assert_eq!(
            detect_shift(&previous, &screen(&["w", "x", "y", "z"], 4), 4),
            None
        );
        assert_eq!(
            detect_shift(&previous, &previous, 0),
            None,
            "no rows to compare is no evidence"
        );
        assert_eq!(detect_shift(&[], &previous, 4), None);
        // Only the first `limit` rows are evidence: the rest may be the link's own text.
        assert_eq!(
            detect_shift(&previous, &screen(&["b", "c", "??", "??"], 4), 2),
            Some(1)
        );
    }

    #[test]
    fn a_row_reports_how_much_of_itself_it_kept() {
        assert_eq!(surviving_columns("abc", "abc"), 3);
        assert_eq!(surviving_columns("abc", "abcdef"), 3, "the row only grew");
        assert_eq!(surviving_columns("abcdef", "abc"), 3, "the tail was erased");
        assert_eq!(surviving_columns("abcdef", "abZdef"), 2);
        assert_eq!(surviving_columns("abc", "xyz"), 0);
        assert_eq!(surviving_columns("", "anything"), 0);
    }
}
