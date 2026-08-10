//! Searching a pane's scrollback: the query, the matches, and the one engine that
//! answers it.
//!
//! Searching a long-running agent's output is an everyday act, and the only place it can
//! honestly be answered is the daemon. The daemon holds the parsed screen *and*
//! [`DEFAULT_SCROLLBACK_ROWS`](turn_pty) rows of history behind it; a client holds only
//! the frames it has been sent, and a pane that printed five hundred lines between two
//! coalesced updates never sent the four hundred and eighty in the middle. So a search
//! that ran on the client would quietly miss most of what the user is looking for.
//!
//! This module is therefore the shared half: the query a client sends, the matches the
//! daemon returns, and — behind the `vt100` feature, exactly like
//! [`from_screen`](crate::cells::from_screen) — the scan itself, so there is one reading
//! of what "this pattern matches that scrollback" means rather than two that drift.
//!
//! ## Coordinates
//!
//! A match is a **line index**: `0` is the oldest row the daemon still holds and
//! `scrollback_len + screen_rows - 1` is the bottom of the live screen. That is the only
//! coordinate both ends can compute, and it is what [`viewport_offset`] turns into the
//! scrollback offset a client scrolls to. It is relative to a moment: rows scroll off and
//! the indices move, so [`SearchOutcome`] carries the `scrollback_len` it was taken at and
//! a client that sees a different one re-runs the query rather than scrolling to a line
//! that has since moved.
//!
//! ## Columns, not characters
//!
//! Matches are reported in **columns**, because that is what a renderer highlights. A
//! wide glyph — a CJK ideograph, an emoji — is one character of text and two columns of
//! screen, so a match reported as a character offset would highlight the wrong cells for
//! every row containing one. [`RowIndex`] keeps the mapping and is built the same way on
//! both sides of the socket.
//!
//! ## The bound
//!
//! Every part of this is capped, because both the pattern and the buffer come from
//! outside:
//!
//! * The pattern is at most [`MAX_QUERY_CHARS`] characters and compiles to at most
//!   [`MAX_PATTERN_BYTES`] of automaton. A pattern that needs more is refused with a
//!   message rather than accepted and left to allocate.
//! * Matching itself is linear in the length of the row: `regex` is a finite automaton
//!   with no backtracking, so there is no pattern that can be made to take exponential
//!   time — the classic `(a+)+$` catastrophe does not exist here. That is the reason this
//!   module uses it rather than hand-rolling a matcher.
//! * The scan reads at most [`MAX_SEARCH_ROWS`] rows and returns at most
//!   [`MAX_MATCHES`] matches, at most [`MAX_MATCHES_PER_ROW`] from any one row — a
//!   pattern of `a` against a row of five hundred `a`s is a real thing a user will type.
//!   When a cap stops the scan the outcome says [`SearchOutcome::truncated`], so the UI
//!   can say "200+" instead of implying it counted them all.

use serde::{Deserialize, Serialize};

/// Longest pattern a client may send. A search box is not a program.
pub const MAX_QUERY_CHARS: usize = 256;

/// Most matches one search returns.
///
/// A thousand is far more than anybody navigates through one at a time, and it bounds the
/// response at about thirty kilobytes.
pub const MAX_MATCHES: usize = 1_000;

/// Most matches taken from a single row.
///
/// Without this, `a` against a row of blanks-free `a`s would contribute one match per
/// column and a screen of them would fill the whole budget from one uninteresting row.
pub const MAX_MATCHES_PER_ROW: usize = 64;

/// Most rows one search reads.
///
/// Above the daemon's own scrollback (5,000 rows) plus any plausible screen, so a normal
/// search is never cut short by it; it exists so that a future, larger scrollback cannot
/// silently turn a keystroke into a hundred-millisecond stall.
pub const MAX_SEARCH_ROWS: usize = 8_192;

/// Most memory a compiled pattern may occupy, and the same cap on its lazy automaton.
///
/// One megabyte compiles every pattern a human types and refuses the generated ones that
/// exist only to make a matcher allocate.
pub const MAX_PATTERN_BYTES: usize = 1 << 20;

/// Longest error message kept from the regular expression compiler.
///
/// Its errors are multi-line and quote the pattern, so they are truncated before they
/// become something a client puts in a label.
const MAX_PATTERN_ERROR_CHARS: usize = 200;

/// How the pattern is read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchMode {
    /// The text as typed. What a user means almost every time, so it is the default.
    #[default]
    Literal,
    /// A regular expression.
    Regex,
}

impl SearchMode {
    pub fn is_regex(&self) -> bool {
        matches!(self, SearchMode::Regex)
    }
}

/// What to look for.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SearchQuery {
    pub text: String,
    #[serde(default, skip_serializing_if = "is_literal")]
    pub mode: SearchMode,
    /// Case-insensitive by default: a user searching for `error` means `Error` too.
    #[serde(default, skip_serializing_if = "is_false")]
    pub case_sensitive: bool,
}

fn is_literal(mode: &SearchMode) -> bool {
    matches!(mode, SearchMode::Literal)
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

impl SearchQuery {
    /// A plain, case-insensitive search for some text.
    pub fn literal(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            mode: SearchMode::Literal,
            case_sensitive: false,
        }
    }

    pub fn regex(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            mode: SearchMode::Regex,
            case_sensitive: false,
        }
    }

    pub fn with_case_sensitive(mut self, sensitive: bool) -> Self {
        self.case_sensitive = sensitive;
        self
    }
}

/// Why a query cannot be run.
///
/// Every variant is something the user can see and correct, which is why the pattern
/// error carries its message: "invalid regular expression" with no reason is a dead end.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SearchError {
    #[error("the search is empty")]
    Empty,
    #[error("a search of {chars} characters is longer than the limit of {max}")]
    TooLong { chars: usize, max: usize },
    #[error("that pattern cannot be used: {0}")]
    BadPattern(String),
}

/// One match, in the coordinates a client can act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneMatch {
    /// Line index: `0` is the oldest retained row, `scrollback_len` is the top row of the
    /// live screen.
    pub line: usize,
    /// First column of the match.
    pub col: u16,
    /// How many columns it covers. Never zero: an empty match is not reported.
    pub cols: u16,
}

impl PaneMatch {
    pub fn new(line: usize, col: u16, cols: u16) -> Self {
        Self { line, col, cols }
    }

    /// Whether a column falls inside this match.
    pub fn contains_column(&self, col: u16) -> bool {
        col >= self.col && col < self.col.saturating_add(self.cols)
    }
}

/// What a search found.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SearchOutcome {
    /// Oldest first, so "next" moves towards the live screen and the order is the order
    /// the output was produced in.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub matches: Vec<PaneMatch>,
    /// Set when a cap stopped the scan, so the count is a floor and the UI must say so.
    #[serde(default, skip_serializing_if = "is_false")]
    pub truncated: bool,
    /// How many rows were read.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub scanned_lines: usize,
    /// How many rows exist, history and live screen together.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub total_lines: usize,
    /// The screen's height, for turning a line index into a viewport offset.
    pub screen_rows: u16,
    /// How much history existed when this was taken. A client that later sees a different
    /// value knows the line indices have moved and re-runs the query.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub scrollback_len: usize,
}

impl SearchOutcome {
    pub fn count(&self) -> usize {
        self.matches.len()
    }

    pub fn is_empty(&self) -> bool {
        self.matches.is_empty()
    }

    /// The words a search bar shows: `3 of 200`, `3 of 1000+`, or `no matches`.
    ///
    /// A search that cannot say which match of how many is not finished, so this is part
    /// of the protocol's own vocabulary rather than something each client invents.
    pub fn position_label(&self, current: Option<usize>) -> String {
        if self.matches.is_empty() {
            return "no matches".to_string();
        }
        let total = if self.truncated {
            format!("{}+", self.matches.len())
        } else {
            self.matches.len().to_string()
        };
        match current {
            Some(index) if index < self.matches.len() => format!("{} of {total}", index + 1),
            _ => format!("{total} matches"),
        }
    }

    /// The scrollback offset that brings match `index` into view.
    pub fn offset_for(&self, index: usize) -> Option<usize> {
        let found = self.matches.get(index)?;
        Some(viewport_offset(
            found.line,
            self.screen_rows,
            self.scrollback_len,
        ))
    }

    /// Which viewport row a line falls on, at a given scrollback offset.
    ///
    /// `None` when the line is outside the viewport, which is what a renderer needs in
    /// order to highlight only the matches actually on screen.
    pub fn viewport_row(&self, line: usize, offset: usize) -> Option<u16> {
        viewport_row(line, offset, self.screen_rows, self.scrollback_len)
    }

    /// The index of the first match at or after `line`, for resuming a search where the
    /// user is looking rather than at the top of the buffer.
    pub fn first_at_or_after(&self, line: usize) -> Option<usize> {
        self.matches.iter().position(|found| found.line >= line)
    }
}

/// The scrollback offset that shows `line`, roughly centred.
///
/// Centred rather than put on the top row because a match with no context above it reads
/// as the start of the output rather than as a hit inside it.
pub fn viewport_offset(line: usize, screen_rows: u16, scrollback_len: usize) -> usize {
    let half = usize::from(screen_rows) / 2;
    let top = line.saturating_sub(half);
    // Zero once the wanted top row is the live screen or below it: there is nothing to
    // scroll back into for a match the user can already see.
    scrollback_len.saturating_sub(top)
}

/// Which viewport row a line lands on at a given offset, or `None` when it is off screen.
pub fn viewport_row(
    line: usize,
    offset: usize,
    screen_rows: u16,
    scrollback_len: usize,
) -> Option<u16> {
    let top = scrollback_len.checked_sub(offset)?;
    let row = line.checked_sub(top)?;
    if row < usize::from(screen_rows) {
        Some(row as u16)
    } else {
        None
    }
}

/// One row as searchable text, plus the column every byte of it belongs to.
///
/// Built identically from a [`Grid`](crate::cells::Grid) row and from a parsed screen row,
/// so a match found on the daemon's copy lands on the same columns in the client's.
///
/// The text follows the terminal's own rule: a gap between two written cells is spaces, a
/// wide glyph appears once, its trailer contributes nothing, and trailing blanks are
/// dropped.
#[derive(Debug, Default, Clone)]
pub struct RowIndex {
    text: String,
    /// One entry per glyph written, in order. `byte` is where it starts in `text`.
    glyphs: Vec<Glyph>,
    /// The column after the last glyph written, so a gap can be padded.
    next_column: u16,
}

#[derive(Debug, Clone, Copy)]
struct Glyph {
    byte: usize,
    col: u16,
    cols: u16,
}

impl RowIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Forgets the previous row, keeping the allocations. A scan of five thousand rows
    /// reuses one of these rather than allocating two vectors per row.
    pub fn clear(&mut self) {
        self.text.clear();
        self.glyphs.clear();
        self.next_column = 0;
    }

    /// Appends one cell's contribution.
    ///
    /// `text` empty means an unwritten cell, which contributes nothing until something
    /// after it does — that is what makes trailing blanks disappear and interior gaps
    /// become spaces.
    pub fn push_cell(&mut self, col: u16, cols: u16, text: &str) {
        if text.is_empty() {
            return;
        }
        while self.next_column < col {
            self.glyphs.push(Glyph {
                byte: self.text.len(),
                col: self.next_column,
                cols: 1,
            });
            self.text.push(' ');
            self.next_column = self.next_column.saturating_add(1);
        }
        self.glyphs.push(Glyph {
            byte: self.text.len(),
            col,
            cols: cols.max(1),
        });
        self.text.push_str(text);
        self.next_column = col.saturating_add(cols.max(1));
    }

    /// Builds from a grid's row. The client's side of the shared reading.
    pub fn from_grid_row(grid: &crate::cells::Grid, row: u16) -> Self {
        let mut index = Self::new();
        index.fill_from_grid_row(grid, row);
        index
    }

    /// Refills from a grid's row, reusing the allocations.
    pub fn fill_from_grid_row(&mut self, grid: &crate::cells::Grid, row: u16) {
        self.clear();
        for col in 0..grid.cols {
            let Some(cell) = grid.cell(row, col) else {
                continue;
            };
            if cell.is_trailer() {
                continue;
            }
            self.push_cell(col, cell.columns(), &cell.text);
        }
    }

    /// The columns a byte range of [`RowIndex::text`] covers.
    ///
    /// `None` for a range that does not start on a glyph, which cannot happen for a match
    /// found in this row's own text and is refused rather than guessed at.
    pub fn column_span(&self, start: usize, end: usize) -> Option<(u16, u16)> {
        if end <= start {
            return None;
        }
        let first = self.glyphs.iter().find(|glyph| glyph.byte == start)?;
        let last = self
            .glyphs
            .iter()
            .rev()
            .find(|glyph| glyph.byte < end)
            .unwrap_or(first);
        let right = last.col.saturating_add(last.cols);
        Some((first.col, right.saturating_sub(first.col).max(1)))
    }
}

/// A compiled query.
///
/// Both modes compile to the same engine: a literal search is the escaped pattern, so
/// case-insensitivity is Unicode-correct and the byte offsets it reports are offsets into
/// the row's own text rather than into a lowercased copy of it — which would be a
/// different length for characters like `İ` and would put the highlight on the wrong
/// column.
#[derive(Debug, Clone)]
pub struct Matcher {
    regex: regex::Regex,
}

impl Matcher {
    /// Compiles a query, or says why it cannot be.
    pub fn compile(query: &SearchQuery) -> Result<Self, SearchError> {
        if query.text.is_empty() {
            return Err(SearchError::Empty);
        }
        let chars = query.text.chars().count();
        if chars > MAX_QUERY_CHARS {
            return Err(SearchError::TooLong {
                chars,
                max: MAX_QUERY_CHARS,
            });
        }
        let pattern = match query.mode {
            SearchMode::Literal => regex::escape(&query.text),
            SearchMode::Regex => query.text.clone(),
        };
        let regex = regex::RegexBuilder::new(&pattern)
            .case_insensitive(!query.case_sensitive)
            // Both caps, because a pattern can be expensive to compile *or* expensive to
            // run the lazy automaton for, and only bounding one of them leaves the other
            // as a way to make the daemon allocate.
            .size_limit(MAX_PATTERN_BYTES)
            .dfa_size_limit(MAX_PATTERN_BYTES)
            .build()
            .map_err(|error| SearchError::BadPattern(short_reason(&error.to_string())))?;
        Ok(Self { regex })
    }

    /// Whether this row is worth looking at closely. The cheap pre-filter a scan uses to
    /// avoid rebuilding the column mapping for rows that cannot match.
    pub fn matches_text(&self, text: &str) -> bool {
        self.regex.is_match(text)
    }

    /// Every match in a row, as columns, bounded by [`MAX_MATCHES_PER_ROW`].
    ///
    /// Empty matches are skipped: a pattern like `x*` matches emptiness at every position,
    /// and reporting those would drown the real hits and highlight nothing.
    pub fn find_in(&self, row: &RowIndex, out: &mut Vec<(u16, u16)>) {
        out.clear();
        for found in self.regex.find_iter(row.text()) {
            if found.start() == found.end() {
                continue;
            }
            if let Some(span) = row.column_span(found.start(), found.end()) {
                out.push(span);
            }
            if out.len() >= MAX_MATCHES_PER_ROW {
                return;
            }
        }
    }
}

/// Trims a compiler message down to something a label can hold.
fn short_reason(message: &str) -> String {
    let single: String = message
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let trimmed = single.split_whitespace().collect::<Vec<_>>().join(" ");
    if trimmed.chars().count() <= MAX_PATTERN_ERROR_CHARS {
        return trimmed;
    }
    trimmed
        .chars()
        .take(MAX_PATTERN_ERROR_CHARS)
        .collect::<String>()
        + "…"
}

/// A scan in progress: the matcher, the budget, and what has been found.
///
/// Separate from the thing being scanned so the daemon can drive it over a parsed screen
/// and a test can drive it over a list of rows, with one definition of when the budget is
/// spent.
pub struct Scan {
    matcher: Matcher,
    matches: Vec<PaneMatch>,
    row: RowIndex,
    columns: Vec<(u16, u16)>,
    scanned: usize,
    truncated: bool,
}

impl Scan {
    pub fn new(query: &SearchQuery) -> Result<Self, SearchError> {
        Ok(Self {
            matcher: Matcher::compile(query)?,
            matches: Vec::new(),
            row: RowIndex::new(),
            columns: Vec::new(),
            scanned: 0,
            truncated: false,
        })
    }

    /// Whether there is budget left. `false` means every further row is unread and the
    /// outcome will say it was truncated.
    pub fn wants_more(&self) -> bool {
        !self.truncated
    }

    /// The matcher, for a caller that wants to pre-filter a row's text cheaply before
    /// paying for its column mapping.
    pub fn matcher(&self) -> &Matcher {
        &self.matcher
    }

    /// A mutable row index to fill in, so a caller reuses the scan's own allocation.
    pub fn row_buffer(&mut self) -> &mut RowIndex {
        &mut self.row
    }

    /// Counts a row as read, whether or not it is worth looking at closely.
    ///
    /// Separate from [`Scan::take_row`] because the row budget is about how much was
    /// *read*: counting only the rows that matched would leave a five-thousand-row buffer
    /// of near misses unbounded, which is the one case the cap exists for.
    pub fn count_row(&mut self) -> bool {
        self.scanned += 1;
        if self.scanned >= MAX_SEARCH_ROWS {
            self.truncated = true;
        }
        self.wants_more()
    }

    /// Records whatever the filled row buffer matches, at line `line`.
    ///
    /// Returns whether the scan wants more rows.
    pub fn take_row(&mut self, line: usize) -> bool {
        let (matcher, row) = (&self.matcher, &self.row);
        matcher.find_in(row, &mut self.columns);
        for (col, cols) in self.columns.drain(..) {
            if self.matches.len() >= MAX_MATCHES {
                self.truncated = true;
                return false;
            }
            self.matches.push(PaneMatch { line, col, cols });
        }
        self.wants_more()
    }

    /// Scans one row given as text and cells, for a caller that has a grid rather than a
    /// parsed screen.
    pub fn visit_grid_row(&mut self, grid: &crate::cells::Grid, row: u16, line: usize) -> bool {
        if !self.count_row() {
            return false;
        }
        self.row.fill_from_grid_row(grid, row);
        self.take_row(line)
    }

    /// Finishes, reporting what was found and the geometry it was found in.
    pub fn finish(
        self,
        screen_rows: u16,
        scrollback_len: usize,
        total_lines: usize,
    ) -> SearchOutcome {
        SearchOutcome {
            matches: self.matches,
            truncated: self.truncated,
            scanned_lines: self.scanned,
            total_lines,
            screen_rows,
            scrollback_len,
        }
    }
}

/// Searches a grid, for a caller with no scrollback: a test, or a client checking its own
/// screen.
///
/// The daemon uses [`search_screen`] instead, which covers the history behind the screen.
pub fn search_grid(
    grid: &crate::cells::Grid,
    query: &SearchQuery,
) -> Result<SearchOutcome, SearchError> {
    let mut scan = Scan::new(query)?;
    for row in 0..grid.rows {
        if !scan.visit_grid_row(grid, row, usize::from(row)) {
            break;
        }
    }
    Ok(scan.finish(grid.rows, 0, usize::from(grid.rows)))
}

#[cfg(feature = "vt100")]
mod parsed {
    use super::*;

    /// Fills a row index from a parsed screen's row, at whatever scrollback offset the
    /// screen is currently at.
    pub fn fill_from_screen_row(index: &mut RowIndex, screen: &vt100::Screen, row: u16) {
        index.clear();
        let (_, cols) = screen.size();
        let mut col = 0;
        while col < cols {
            let Some(cell) = screen.cell(row, col) else {
                break;
            };
            // A wide cell's continuation repeats its partner's contents in `vt100`;
            // stepping over it is what stops the glyph being counted twice.
            let width = if cell.is_wide() { 2 } else { 1 };
            if !cell.is_wide_continuation() {
                index.push_cell(col, width, cell.contents());
            }
            col = col.saturating_add(width);
        }
    }

    /// How many rows of history the screen holds behind its live rows.
    ///
    /// Measured by asking for an impossible offset and reading back what was clamped to,
    /// because `vt100` exposes the configured maximum rather than what is actually
    /// retained. The offset is restored before returning, and the caller is expected to
    /// be holding this screen exclusively — the daemon's `TerminalBuffer` guarantees that.
    pub fn history_len(screen: &mut vt100::Screen) -> usize {
        let previous = screen.scrollback();
        screen.set_scrollback(usize::MAX);
        let len = screen.scrollback();
        screen.set_scrollback(previous);
        len
    }

    /// A screen-shaped window of history, as cells.
    ///
    /// `offset` is rows above the top of the live screen and is clamped to what exists,
    /// so a client asking to scroll further than the record goes gets the oldest window
    /// rather than an error. The returned grid says which offset it actually is, and how
    /// much history there is, so a client never has to guess whether it reached the end.
    pub fn history_grid(screen: &mut vt100::Screen, offset: usize) -> crate::cells::Grid {
        let len = history_len(screen);
        let wanted = offset.min(len);
        screen.set_scrollback(wanted);
        let mut grid = crate::cells::from_screen(screen);
        screen.set_scrollback(0);
        grid.scrollback_offset = wanted;
        grid.scrollback_len = len;
        // The cursor belongs to the live screen. Drawn at the same coordinates over
        // history it would sit on an unrelated character, so a scrolled window has none.
        if wanted > 0 {
            grid.cursor = None;
        }
        grid
    }

    /// Searches the whole of a pane's retained output: the history, then the live screen.
    ///
    /// Walks it in windows of one screen height rather than a row at a time, because
    /// reading a window costs one pass over its rows however many of them are wanted.
    /// The scrollback offset is restored before returning.
    pub fn search_screen(
        screen: &mut vt100::Screen,
        query: &SearchQuery,
    ) -> Result<SearchOutcome, SearchError> {
        let mut scan = Scan::new(query)?;
        let (rows, cols) = screen.size();
        let history = history_len(screen);
        let total = history.saturating_add(usize::from(rows));
        let height = usize::from(rows).max(1);

        let mut next_line = 0usize;
        loop {
            // The window whose first row is `next_line`: `offset` rows above the live
            // screen's top row, clamped at zero once the live screen is reached.
            let offset = history.saturating_sub(next_line);
            screen.set_scrollback(offset);
            let first = history - offset;
            let lines: Vec<String> = screen.rows(0, cols).collect();
            for (row, text) in lines.iter().enumerate() {
                let line = first + row;
                if line < next_line {
                    continue;
                }
                if line >= total {
                    break;
                }
                next_line = line + 1;
                if !scan.count_row() {
                    screen.set_scrollback(0);
                    return Ok(scan.finish(rows, history, total));
                }
                // The cheap filter first: the precise column mapping is only worth
                // building for a row that can actually contain a match, and on a
                // five-thousand-row buffer that is nearly none of them.
                if !scan.matcher().matches_text(text) {
                    continue;
                }
                fill_from_screen_row(scan.row_buffer(), screen, row as u16);
                if !scan.take_row(line) {
                    screen.set_scrollback(0);
                    return Ok(scan.finish(rows, history, total));
                }
            }
            if next_line >= total || offset == 0 {
                break;
            }
            // Defensive: a window that advanced nothing would loop for ever.
            if lines.is_empty() {
                break;
            }
            next_line = next_line.max(first + height);
        }
        screen.set_scrollback(0);
        Ok(scan.finish(rows, history, total))
    }
}

#[cfg(feature = "vt100")]
pub use parsed::{fill_from_screen_row, history_grid, history_len, search_screen};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cells::Grid;

    fn lines(text: &[&str], cols: u16) -> Grid {
        Grid::from_lines(text, cols)
    }

    #[test]
    fn a_literal_search_is_case_insensitive_until_it_is_told_otherwise() {
        let grid = lines(&["Error: nothing", "error again", "fine"], 20);
        let found = search_grid(&grid, &SearchQuery::literal("error")).expect("a valid query");
        assert_eq!(found.count(), 2, "{:?}", found.matches);
        assert_eq!(found.matches[0], PaneMatch::new(0, 0, 5));
        assert_eq!(found.matches[1], PaneMatch::new(1, 0, 5));

        let sensitive = search_grid(
            &grid,
            &SearchQuery::literal("error").with_case_sensitive(true),
        )
        .expect("a valid query");
        assert_eq!(sensitive.count(), 1);
        assert_eq!(sensitive.matches[0].line, 1);
    }

    /// A literal search must not be read as a pattern. `.` is a character, not "anything".
    #[test]
    fn a_literal_search_takes_its_punctuation_literally() {
        let grid = lines(&["a.c", "abc"], 8);
        let found = search_grid(&grid, &SearchQuery::literal("a.c")).expect("a valid query");
        assert_eq!(found.count(), 1);
        assert_eq!(found.matches[0].line, 0);

        let pattern = search_grid(&grid, &SearchQuery::regex("a.c")).expect("a valid pattern");
        assert_eq!(pattern.count(), 2, "as a pattern it matches both");
    }

    #[test]
    fn a_regular_expression_search_reports_the_columns_it_covered() {
        let grid = lines(&["build 12 failed", "build 345 failed"], 20);
        let found = search_grid(&grid, &SearchQuery::regex(r"\d+")).expect("a valid pattern");
        assert_eq!(found.count(), 2);
        assert_eq!(found.matches[0], PaneMatch::new(0, 6, 2));
        assert_eq!(found.matches[1], PaneMatch::new(1, 6, 3));
    }

    /// The whole reason matches are columns and not characters: a wide glyph is one
    /// character of text and two columns of screen.
    #[test]
    fn a_match_after_a_wide_glyph_lands_on_the_columns_it_occupies() {
        let mut grid = Grid::blank(1, 12);
        assert!(grid.set_wide(0, 0, "漢"));
        for (offset, ch) in "err".chars().enumerate() {
            if let Some(cell) = grid.cell_mut(0, 2 + offset as u16) {
                cell.text = ch.to_string();
            }
        }
        let found = search_grid(&grid, &SearchQuery::literal("err")).expect("a valid query");
        assert_eq!(
            found.matches[0],
            PaneMatch::new(0, 2, 3),
            "the ideograph occupies columns 0 and 1"
        );

        // And a match *on* the wide glyph covers both of its columns.
        let wide = search_grid(&grid, &SearchQuery::literal("漢")).expect("a valid query");
        assert_eq!(wide.matches[0], PaneMatch::new(0, 0, 2));
    }

    #[test]
    fn a_gap_between_written_cells_is_searchable_as_spaces() {
        let grid = lines(&["a    b"], 10);
        let index = RowIndex::from_grid_row(&grid, 0);
        assert_eq!(index.text(), "a    b");
        assert_eq!(
            index.column_span(2, 4),
            Some((2, 2)),
            "the padding carries its own columns"
        );
        let found = search_grid(&grid, &SearchQuery::regex("a +b")).expect("a valid pattern");
        assert_eq!(found.matches[0], PaneMatch::new(0, 0, 6));
    }

    #[test]
    fn trailing_blanks_are_not_part_of_a_row_so_a_pattern_cannot_match_padding() {
        let grid = lines(&["short"], 40);
        let index = RowIndex::from_grid_row(&grid, 0);
        assert_eq!(index.text(), "short", "not thirty-five spaces of padding");
        let found = search_grid(&grid, &SearchQuery::regex(" +$")).expect("a valid pattern");
        assert!(found.is_empty(), "{:?}", found.matches);
    }

    #[test]
    fn an_empty_query_is_refused_rather_than_matching_everything() {
        assert_eq!(
            Matcher::compile(&SearchQuery::literal("")).err(),
            Some(SearchError::Empty)
        );
    }

    #[test]
    fn a_query_longer_than_the_limit_is_refused_before_it_is_compiled() {
        let long = "x".repeat(MAX_QUERY_CHARS + 1);
        assert_eq!(
            Matcher::compile(&SearchQuery::literal(long)).err(),
            Some(SearchError::TooLong {
                chars: MAX_QUERY_CHARS + 1,
                max: MAX_QUERY_CHARS
            })
        );
        // Exactly at the limit is allowed: the cap is on what is unreasonable, not on
        // what is long.
        assert!(Matcher::compile(&SearchQuery::literal("x".repeat(MAX_QUERY_CHARS))).is_ok());
    }

    /// A pattern from a user can be nonsense, and the answer has to say why rather than
    /// failing silently or hanging.
    #[test]
    fn an_invalid_pattern_is_refused_with_a_bounded_reason() {
        let error = Matcher::compile(&SearchQuery::regex("(unclosed"))
            .expect_err("an unclosed group is not a pattern");
        match error {
            SearchError::BadPattern(reason) => {
                assert!(!reason.is_empty());
                assert!(
                    reason.chars().count() <= MAX_PATTERN_ERROR_CHARS + 1,
                    "the reason must fit in a label: {reason}"
                );
                assert!(!reason.contains('\n'), "and must stay one line: {reason}");
            }
            other => panic!("wrong error: {other:?}"),
        }
    }

    /// The pattern every "regular expressions are dangerous" example uses. With a finite
    /// automaton it is linear, and this test is here to prove the choice of engine holds
    /// rather than to assert a wall-clock number.
    #[test]
    fn a_pattern_designed_to_backtrack_for_ever_finishes_immediately() {
        let mut grid = Grid::blank(1, 120);
        for col in 0..119u16 {
            if let Some(cell) = grid.cell_mut(0, col) {
                cell.text = "a".to_string();
            }
        }
        let started = std::time::Instant::now();
        let found = search_grid(&grid, &SearchQuery::regex("(a+)+$")).expect("it compiles");
        assert!(!found.is_empty());
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "a finite automaton cannot be made to backtrack, so this must be instant"
        );
    }

    /// A pattern that only exists to make the compiler allocate is refused.
    #[test]
    fn a_pattern_that_would_compile_to_something_enormous_is_refused() {
        // Bounded repetition multiplies the automaton out; nested, it exceeds a megabyte
        // long before it exceeds the query length limit.
        let error = Matcher::compile(&SearchQuery::regex("((((a{99}){99}){99}){99})")).err();
        assert!(
            matches!(error, Some(SearchError::BadPattern(_))),
            "got {error:?}"
        );
    }

    #[test]
    fn an_empty_match_is_not_reported_because_it_highlights_nothing() {
        let grid = lines(&["abc"], 8);
        let found = search_grid(&grid, &SearchQuery::regex("x*")).expect("a valid pattern");
        assert!(found.is_empty(), "{:?}", found.matches);
    }

    #[test]
    fn one_row_cannot_spend_the_whole_match_budget() {
        let mut grid = Grid::blank(2, 200);
        for row in 0..2u16 {
            for col in 0..200u16 {
                if let Some(cell) = grid.cell_mut(row, col) {
                    cell.text = "a".to_string();
                }
            }
        }
        let found = search_grid(&grid, &SearchQuery::literal("a")).expect("a valid query");
        assert_eq!(
            found.count(),
            MAX_MATCHES_PER_ROW * 2,
            "each row contributes at most its share"
        );
    }

    #[test]
    fn the_match_budget_stops_the_scan_and_says_the_count_is_a_floor() {
        let rows = (MAX_MATCHES / MAX_MATCHES_PER_ROW) as u16 + 4;
        let mut grid = Grid::blank(rows, 80);
        for row in 0..rows {
            for col in 0..80u16 {
                if let Some(cell) = grid.cell_mut(row, col) {
                    cell.text = "a".to_string();
                }
            }
        }
        let found = search_grid(&grid, &SearchQuery::literal("a")).expect("a valid query");
        assert!(found.truncated, "the cap was reached");
        assert_eq!(found.count(), MAX_MATCHES);
        assert_eq!(
            found.position_label(Some(2)),
            format!("3 of {MAX_MATCHES}+")
        );
    }

    /// The sentence the task is about: a search that cannot say "3 of 200" is not
    /// finished.
    #[test]
    fn the_outcome_can_say_which_match_of_how_many() {
        let mut outcome = SearchOutcome {
            matches: (0..200).map(|line| PaneMatch::new(line, 0, 3)).collect(),
            screen_rows: 40,
            ..SearchOutcome::default()
        };
        assert_eq!(outcome.position_label(Some(2)), "3 of 200");
        assert_eq!(outcome.position_label(None), "200 matches");
        outcome.matches.clear();
        assert_eq!(outcome.position_label(Some(0)), "no matches");
    }

    #[test]
    fn a_match_in_the_history_is_turned_into_the_offset_that_shows_it() {
        // 1,000 rows of history behind a 40-row screen.
        let outcome = SearchOutcome {
            matches: vec![
                PaneMatch::new(0, 0, 3),
                PaneMatch::new(500, 0, 3),
                PaneMatch::new(1_030, 0, 3),
            ],
            screen_rows: 40,
            scrollback_len: 1_000,
            total_lines: 1_040,
            ..SearchOutcome::default()
        };
        // The oldest row: centring would run off the top, so the offset is the whole
        // history and the match sits on the first row.
        assert_eq!(outcome.offset_for(0), Some(1_000));
        assert_eq!(outcome.viewport_row(0, 1_000), Some(0));
        // The middle: centred, twenty rows above the match.
        assert_eq!(outcome.offset_for(1), Some(520));
        assert_eq!(outcome.viewport_row(500, 520), Some(20));
        // On the live screen: no offset at all, and the row is where it is.
        assert_eq!(outcome.offset_for(2), Some(0));
        assert_eq!(outcome.viewport_row(1_030, 0), Some(30));
        assert_eq!(outcome.offset_for(9), None);
        assert_eq!(
            outcome.viewport_row(0, 0),
            None,
            "a line above the viewport is not on screen"
        );
    }

    #[test]
    fn the_first_match_at_or_after_a_line_is_findable_so_a_search_can_resume_where_the_user_is() {
        let outcome = SearchOutcome {
            matches: vec![
                PaneMatch::new(10, 0, 1),
                PaneMatch::new(50, 0, 1),
                PaneMatch::new(90, 0, 1),
            ],
            screen_rows: 24,
            ..SearchOutcome::default()
        };
        assert_eq!(outcome.first_at_or_after(0), Some(0));
        assert_eq!(outcome.first_at_or_after(11), Some(1));
        assert_eq!(outcome.first_at_or_after(90), Some(2));
        assert_eq!(outcome.first_at_or_after(91), None);
    }

    #[test]
    fn a_query_round_trips_through_json_in_its_documented_shape() {
        let query = SearchQuery::regex("er+or").with_case_sensitive(true);
        let json = serde_json::to_string(&query).expect("it serialises");
        assert_eq!(
            json,
            "{\"text\":\"er+or\",\"mode\":\"regex\",\"case_sensitive\":true}"
        );
        assert_eq!(
            serde_json::from_str::<SearchQuery>(&json).expect("it reads back"),
            query
        );
        // The common case is the cheap one on the wire.
        assert_eq!(
            serde_json::to_string(&SearchQuery::literal("x")).expect("it serialises"),
            "{\"text\":\"x\"}"
        );
    }

    #[test]
    fn an_outcome_round_trips_through_json() {
        let outcome = SearchOutcome {
            matches: vec![PaneMatch::new(3, 4, 5)],
            truncated: true,
            scanned_lines: 900,
            total_lines: 900,
            screen_rows: 40,
            scrollback_len: 860,
        };
        let json = serde_json::to_string(&outcome).expect("it serialises");
        assert_eq!(
            serde_json::from_str::<SearchOutcome>(&json).expect("it reads back"),
            outcome
        );
        assert!(json.contains("\"line\":3"), "got {json}");
    }
}

#[cfg(all(test, feature = "vt100"))]
mod parsed_tests {
    use super::*;

    /// Feeds a parser enough output to push most of it into the scrollback.
    fn parser(rows: u16, cols: u16, scrollback: usize) -> vt100::Parser {
        vt100::Parser::new(rows, cols, scrollback)
    }

    #[test]
    fn a_search_covers_the_history_behind_the_screen_and_not_only_what_is_visible() {
        let mut p = parser(6, 40, 500);
        for line in 0..200 {
            p.process(format!("line {line}\r\n").as_bytes());
        }
        // The visible screen holds the last few lines only.
        let visible = p.screen().contents();
        assert!(!visible.contains("line 7"), "got {visible:?}");

        let found =
            search_screen(p.screen_mut(), &SearchQuery::literal("line 7")).expect("a valid query");
        // `line 7` itself and `line 70` through `line 79`.
        assert_eq!(found.count(), 11, "{:?}", found.matches);
        assert_eq!(found.matches[0].line, 7);
        assert!(
            found.matches.iter().any(|m| m.line < found.scrollback_len),
            "at least one match must be in the history, or the search is a toy"
        );
        assert_eq!(
            p.screen().scrollback(),
            0,
            "the search must leave the live screen in view for every other reader"
        );
    }

    #[test]
    fn a_matched_line_index_resolves_to_the_history_window_that_contains_it() {
        let mut p = parser(6, 40, 500);
        for line in 0..100 {
            p.process(format!("row {line:03}\r\n").as_bytes());
        }
        let found =
            search_screen(p.screen_mut(), &SearchQuery::literal("row 042")).expect("a valid query");
        assert_eq!(found.count(), 1, "{:?}", found.matches);
        let hit = found.matches[0];
        let offset = found.offset_for(0).expect("the first match has an offset");
        let window = history_grid(p.screen_mut(), offset);
        let row = found
            .viewport_row(hit.line, window.scrollback_offset)
            .expect("the match is inside the window it asked for");
        assert!(
            window.row_text(row).contains("row 042"),
            "row {row} of the window is {:?}",
            window.row_text(row)
        );
        assert_eq!(window.cursor, None, "a history window has no cursor");
        assert_eq!(p.screen().scrollback(), 0);
    }

    #[test]
    fn the_row_text_built_from_cells_matches_the_parsers_own_reading_of_that_row() {
        let mut p = parser(4, 20, 0);
        p.process("a  b漢c\r\n".as_bytes());
        let mut index = RowIndex::new();
        fill_from_screen_row(&mut index, p.screen(), 0);
        let theirs: Vec<String> = p.screen().rows(0, 20).collect();
        assert_eq!(
            index.text(),
            theirs[0].trim_end(),
            "the pre-filter and the precise reading must agree, or a match could be missed"
        );
    }

    #[test]
    fn history_is_measured_rather_than_assumed_and_the_offset_is_left_where_it_was() {
        let mut p = parser(4, 20, 50);
        assert_eq!(history_len(p.screen_mut()), 0);
        for line in 0..10 {
            p.process(format!("l{line}\r\n").as_bytes());
        }
        assert_eq!(history_len(p.screen_mut()), 7);
        assert_eq!(p.screen().scrollback(), 0);

        // Past the end is clamped rather than refused.
        let window = history_grid(p.screen_mut(), 900);
        assert_eq!(window.scrollback_offset, 7);
        assert_eq!(window.scrollback_len, 7);
        assert_eq!(window.row_text(0), "l0");
    }

    /// A full-screen program's grid has no scrollback of its own, so a search covers what
    /// is on screen and Turn has nothing to scroll into. That is the same rule the pane
    /// follows visually.
    #[test]
    fn while_a_full_screen_program_is_in_control_there_is_no_history_to_search() {
        let mut p = parser(4, 20, 100);
        for line in 0..20 {
            p.process(format!("scrolled {line}\r\n").as_bytes());
        }
        assert!(history_len(p.screen_mut()) > 0);
        p.process(b"\x1b[?1049h");
        p.process(b"inside vim");
        let found = search_screen(p.screen_mut(), &SearchQuery::literal("scrolled"))
            .expect("a valid query");
        assert!(
            found.is_empty(),
            "the alternate screen has no history: {:?}",
            found.matches
        );
        assert_eq!(found.scrollback_len, 0);
        let inside =
            search_screen(p.screen_mut(), &SearchQuery::literal("inside")).expect("a valid query");
        assert_eq!(inside.count(), 1);
    }

    /// The cost bound, measured rather than asserted from memory: a full five thousand
    /// rows of scrollback is one search, and it has to be quick enough to run on a
    /// keystroke.
    #[test]
    fn a_search_over_five_thousand_rows_of_history_stays_well_inside_a_frame_budget() {
        let mut p = parser(40, 120, 5_000);
        for line in 0..5_000 {
            p.process(format!("[{line:05}] compiling something rather verbose\r\n").as_bytes());
        }
        assert!(history_len(p.screen_mut()) >= 4_900);

        let started = std::time::Instant::now();
        let found =
            search_screen(p.screen_mut(), &SearchQuery::literal("[04999]")).expect("a valid query");
        let elapsed = started.elapsed();
        assert_eq!(found.count(), 1);
        assert!(
            found.scanned_lines >= 5_000,
            "the whole buffer must be read: {}",
            found.scanned_lines
        );
        // Around 20 ms in a debug build on an M-series laptop, and a few milliseconds in
        // release. The assertion is loose because it runs on other people's machines; what
        // it is really guarding is the difference between milliseconds and seconds.
        assert!(
            elapsed < std::time::Duration::from_millis(750),
            "a search over five thousand rows took {elapsed:?}"
        );
    }

    #[test]
    fn the_row_cap_stops_a_scan_of_an_absurd_buffer_and_admits_it() {
        let mut p = parser(4, 20, MAX_SEARCH_ROWS * 2);
        for line in 0..(MAX_SEARCH_ROWS + 500) {
            p.process(format!("r{line}\r\n").as_bytes());
        }
        let found = search_screen(p.screen_mut(), &SearchQuery::literal("nothing here"))
            .expect("a valid query");
        assert!(found.truncated, "the row cap must be admitted");
        assert!(found.scanned_lines <= MAX_SEARCH_ROWS);
    }
}
