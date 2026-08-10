//! Selecting and copying text out of a grid.
//!
//! A terminal selection is not a text selection: it is a range over a rectangular
//! array of cells, and turning it back into text is where the decisions are.
//!
//! * **Trailing blanks are dropped.** A terminal row is padded to the full width, so
//!   copying a line verbatim yields the text plus seventy spaces. Nobody wants that
//!   in a commit message.
//! * **A wide cell contributes its glyph once.** Its trailer holds no text, so
//!   including it would put a space inside every emoji.
//! * **A block selection is offered as well as a linear one.** Copying one column out
//!   of `docker ps` is a real thing people do, and a linear selection cannot express
//!   it.
//! * **A hard-wrapped line is one line.** The terminal broke it at the margin; the
//!   program never printed a newline there. Copying the break as a newline is how a
//!   path arrives in an editor with a hole in the middle of it, so a wrapped row is
//!   joined to the one below and its trailing spaces are kept, because they are text
//!   rather than padding. [`crate::cells::Grid::row_wrapped`] is the only source of
//!   that fact — a client cannot derive it, since a row that happens to fill the
//!   width is not necessarily a row that wrapped.
//!
//! ## What "a word" is in a terminal
//!
//! Double-clicking has to produce the thing the user was pointing at, and in a terminal
//! that thing is almost never a dictionary word. It is `src/main.rs:42` from a compiler
//! error, `--jobs=4` from a command line, `https://example.com/a?b=c` from a log, or
//! `some_ident` from source. Splitting those at every punctuation mark — which is what a
//! word class borrowed from a text editor does — makes double-click useless for the four
//! things it is actually for.
//!
//! So [`WORD_PUNCTUATION`] is chosen from those shapes rather than from grammar, and it
//! is deliberately not "all punctuation": a comma, a quote, a bracket, a semicolon and a
//! pipe are separators, because `foo,bar` is two things and `(foo)` is one thing in
//! brackets. Cells fall into three classes — word, blank, and everything else — and a
//! double-click takes the whole run of one class, so double-clicking a gap selects the gap
//! and double-clicking `|||` selects the pipes rather than silently doing nothing.
//!
//! A word runs **across a hard wrap**, because a path broken at the margin is still one
//! path. That is the same fact the copy rule uses, read the other way round.

use std::ops::RangeInclusive;

use crate::cells::{Cell, Grid};

/// The punctuation that belongs *inside* a terminal word.
///
/// Each character here is here for a shape somebody double-clicks:
///
/// * `/` and `\` — a path, in either dialect.
/// * `.` — a file extension, a hostname, a version.
/// * `:` — `file:line`, a scheme, a port.
/// * `-` and `_` — an identifier, a flag, a branch name.
/// * `=` — `--flag=value`, `FOO=bar`.
/// * `~` — a home-relative path.
/// * `@` — an email address, `user@host`, an npm scope.
/// * `+`, `?`, `#`, `%`, `&` — the parts of a URL after its path.
/// * `$` — `$HOME`.
///
/// What is *not* here matters as much: `,` `;` `'` `"` `` ` `` `|` `*` `!` `^` `<` `>`
/// and every kind of bracket separate, because they are what shells and prose use to put
/// two things next to each other.
pub const WORD_PUNCTUATION: &str = "_-./\\:@~+=?#%&$";

/// Where a cell is, in the grid's own coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct CellPos {
    pub row: u16,
    pub col: u16,
}

impl CellPos {
    pub fn new(row: u16, col: u16) -> Self {
        Self { row, col }
    }

    /// Reading order, which is the order a linear selection runs in.
    fn key(&self) -> (u16, u16) {
        (self.row, self.col)
    }
}

/// Whether a selection follows the text or a rectangle.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SelectionKind {
    /// Line by line, the way a text selection works.
    #[default]
    Linear,
    /// A rectangle, for copying one column out of tabular output. Held with Alt.
    Block,
}

/// How much one gesture takes at a time.
///
/// Stored on the selection rather than applied once, because it has to survive the drag:
/// a double-click that turns into a drag extends by whole words, and a triple-click drag
/// by whole lines. That is what every text field does and it is what makes selecting three
/// wrapped log lines possible without pixel-hunting the ends.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Granularity {
    #[default]
    Character,
    Word,
    Line,
}

/// A selection in progress or finished.
///
/// Stored as anchor and head rather than as start and end so that dragging backwards
/// works without the selection flipping inside out, and so the head is always where
/// the pointer is. [`Selection::origin`] is kept beside them because a word- or
/// line-granularity drag has to grow from the cell the gesture *started* on, in either
/// direction, and the anchor moves when it does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    /// The cell the gesture began on. Never moved by extending.
    pub origin: CellPos,
    pub anchor: CellPos,
    pub head: CellPos,
    pub kind: SelectionKind,
    pub granularity: Granularity,
}

impl Selection {
    pub fn new(anchor: CellPos, kind: SelectionKind) -> Self {
        Self {
            origin: anchor,
            anchor,
            head: anchor,
            kind,
            granularity: Granularity::Character,
        }
    }

    /// The word under a cell, as a selection that can then be dragged wider.
    ///
    /// Always returns something: a blank selects the run of blanks, and a punctuation
    /// character that is not part of a word selects the run of such characters. A
    /// double-click that produced nothing would read as a broken double-click.
    pub fn word(grid: &Grid, at: CellPos, kind: SelectionKind) -> Self {
        let (from, to) = word_span(grid, at);
        Self {
            origin: at,
            anchor: from,
            head: to,
            kind,
            granularity: Granularity::Word,
        }
    }

    /// The whole logical line under a cell: every row a hard wrap joined to it.
    pub fn line(grid: &Grid, at: CellPos) -> Self {
        let rows = logical_line(grid, at.row);
        Self {
            origin: at,
            anchor: CellPos::new(*rows.start(), 0),
            head: CellPos::new(*rows.end(), grid.cols),
            kind: SelectionKind::Linear,
            granularity: Granularity::Line,
        }
    }

    /// Everything on the screen that has anything on it.
    ///
    /// Stops after the last row with content rather than running to the bottom of the
    /// grid, because a terminal screen is mostly blank and "Select All" that yields three
    /// lines plus thirty-seven empty ones is a clipboard nobody can paste.
    pub fn all(grid: &Grid) -> Self {
        let last = last_row_with_content(grid).unwrap_or(0);
        Self {
            origin: CellPos::new(0, 0),
            anchor: CellPos::new(0, 0),
            head: CellPos::new(last, grid.cols),
            kind: SelectionKind::Linear,
            granularity: Granularity::Line,
        }
    }

    /// Moves the loose end, one cell at a time.
    ///
    /// The character-granularity primitive. A gesture that started as a double- or
    /// triple-click must use [`Selection::extend_in_grid`] instead, or it would lose the
    /// granularity the user asked for on the first click.
    pub fn extend_to(&mut self, head: CellPos) {
        self.head = head;
        if self.granularity == Granularity::Character {
            self.anchor = self.origin;
        }
    }

    /// Moves the loose end, honouring the granularity the gesture started with.
    ///
    /// Both ends are recomputed from [`Selection::origin`], so dragging back across the
    /// origin extends the other way rather than collapsing the selection to nothing.
    pub fn extend_in_grid(&mut self, grid: &Grid, to: CellPos) {
        match self.granularity {
            Granularity::Character => {
                self.anchor = self.origin;
                self.head = to;
            }
            Granularity::Word => {
                let (origin_from, origin_to) = word_span(grid, self.origin);
                let (to_from, to_to) = word_span(grid, to);
                if to.key() >= self.origin.key() {
                    self.anchor = origin_from;
                    self.head = to_to;
                } else {
                    self.anchor = to_from;
                    self.head = origin_to;
                }
            }
            Granularity::Line => {
                let origin_rows = logical_line(grid, self.origin.row);
                let to_rows = logical_line(grid, to.row);
                if to.row >= self.origin.row {
                    self.anchor = CellPos::new(*origin_rows.start(), 0);
                    self.head = CellPos::new(*to_rows.end(), grid.cols);
                } else {
                    self.anchor = CellPos::new(*to_rows.start(), 0);
                    self.head = CellPos::new(*origin_rows.end(), grid.cols);
                }
            }
        }
    }

    /// Slides the whole selection down or up by `delta` rows.
    ///
    /// What an auto-scrolling drag needs: the viewport moved under the selection, so the
    /// cells the user chose are now at different row numbers and the anchor has to follow
    /// them or the highlight would crawl across the text. Clamped to the grid, because a
    /// selection whose start has scrolled off the screen is still a selection of what
    /// remains visible.
    pub fn shift_rows(&mut self, delta: i32, rows: u16) {
        let shift = |pos: &mut CellPos| {
            let moved = i64::from(pos.row) + i64::from(delta);
            pos.row = moved.clamp(0, i64::from(rows.saturating_sub(1))) as u16;
        };
        shift(&mut self.origin);
        shift(&mut self.anchor);
        shift(&mut self.head);
    }

    /// The two ends in reading order.
    pub fn ordered(&self) -> (CellPos, CellPos) {
        if self.anchor.key() <= self.head.key() {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    /// Whether a selection covers nothing, so a copy is not worth offering.
    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    /// Whether a cell is inside the selection, for painting.
    ///
    /// The head cell is excluded, which is what makes a selection that ends where the
    /// pointer is not include the character under the pointer — the behaviour every
    /// text selection has.
    pub fn contains(&self, row: u16, col: u16) -> bool {
        if self.is_empty() {
            return false;
        }
        let (start, end) = self.ordered();
        match self.kind {
            SelectionKind::Block => {
                let (left, right) = self.columns();
                row >= start.row && row <= end.row && col >= left && col < right
            }
            SelectionKind::Linear => {
                if row < start.row || row > end.row {
                    return false;
                }
                if start.row == end.row {
                    return col >= start.col && col < end.col;
                }
                if row == start.row {
                    return col >= start.col;
                }
                if row == end.row {
                    return col < end.col;
                }
                true
            }
        }
    }

    /// The columns a block selection covers, left inclusive and right exclusive.
    ///
    /// At least one column wide: a rectangle dragged straight down a single column is the
    /// gesture for "this column", and a zero-width answer would select nothing at all.
    fn columns(&self) -> (u16, u16) {
        let (left, right) = if self.anchor.col <= self.head.col {
            (self.anchor.col, self.head.col)
        } else {
            (self.head.col, self.anchor.col)
        };
        (left, right.max(left.saturating_add(1)))
    }

    /// The selected text, ready for the clipboard.
    ///
    /// Three rules, each of which is a bug when it is missing: a row's trailing padding is
    /// dropped, a wide glyph is taken once rather than twice, and a row the terminal
    /// wrapped is joined to the one below without a newline.
    pub fn text(&self, grid: &Grid) -> String {
        if self.is_empty() || grid.rows == 0 {
            return String::new();
        }
        let (start, end) = self.ordered();
        let last = end.row.min(grid.rows.saturating_sub(1));
        if start.row > last {
            return String::new();
        }
        let mut out = String::new();
        for row in start.row..=last {
            // A row that wrapped into the next one is being joined to it, so its trailing
            // spaces are part of the line rather than the terminal's padding. Trimming
            // them would close up a gap the program printed on purpose.
            let joins = self.joins_below(grid, row, last);
            out.push_str(&self.row_text(grid, row, !joins));
            if row < last && !joins {
                out.push('\n');
            }
        }
        out
    }

    /// Whether the row below continues this row's text with no newline between them.
    ///
    /// Never for a block selection: a rectangle's rows are separate by construction — that
    /// is the whole point of taking one column out of a table — and joining them would
    /// produce one long meaningless string.
    fn joins_below(&self, grid: &Grid, row: u16, last: u16) -> bool {
        self.kind == SelectionKind::Linear && row < last && grid.row_wrapped(row)
    }

    /// One row's worth of the selection, as text.
    fn row_text(&self, grid: &Grid, row: u16, trim: bool) -> String {
        let mut line = String::new();
        for col in 0..grid.cols {
            if !self.contains(row, col) {
                continue;
            }
            match grid.cell(row, col) {
                // A wide cell's trailer carries no glyph; including a space for it
                // would put a gap inside every emoji.
                Some(cell) if cell.is_trailer() => {}
                // A tile of an inline image contributes a space. Its text is a marker with
                // no glyph anywhere — Turn's own bookkeeping — and putting one on somebody's
                // clipboard would paste an invisible private-use character into their commit
                // message. A space keeps the words on either side of a picture apart.
                Some(cell) if cell.is_image() => line.push(' '),
                Some(cell) if cell.text.is_empty() => line.push(' '),
                Some(cell) => line.push_str(&cell.text),
                None => {}
            }
        }
        if trim {
            line.truncate(line.trim_end().len());
        }
        line
    }
}

/// The rows one logical line occupies: the run of rows a hard wrap joined together.
///
/// A program that printed a newline ended its line, and the row below is a new one. A
/// program that ran past the right margin did not, and the row below is the same line
/// continued. Only the emulator knows which happened, which is why the flag travels with
/// the grid.
///
/// The other reader of the same flag is [`crate::terminal::links::logical_lines`], which
/// needs the joined *text* of every line in order to find URLs in it. This answers the
/// cheaper question a double-click asks — which rows is this one line? — without building
/// the whole screen's worth.
pub fn logical_line(grid: &Grid, row: u16) -> RangeInclusive<u16> {
    let last_row = grid.rows.saturating_sub(1);
    let row = row.min(last_row);
    let mut first = row;
    while first > 0 && grid.row_wrapped(first - 1) {
        first -= 1;
    }
    let mut last = row;
    while last < last_row && grid.row_wrapped(last) {
        last += 1;
    }
    first..=last
}

/// What kind of run a cell belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CellClass {
    /// Part of a path, an identifier, a flag or a URL.
    Word,
    /// Nothing there.
    Blank,
    /// Punctuation that separates words: a comma, a bracket, a pipe. Also an emoji,
    /// which is not part of a word and is not nothing.
    Other,
}

/// Whether a character belongs inside a terminal word.
pub fn is_word_char(ch: char) -> bool {
    ch.is_alphanumeric() || WORD_PUNCTUATION.contains(ch)
}

fn class_of(cell: Option<&Cell>) -> CellClass {
    match cell {
        None => CellClass::Blank,
        // A picture is not a word and not punctuation: a double-click beside one must not
        // drag the selection across it, and its marker is not a character to classify.
        Some(cell) if cell.is_image() => CellClass::Blank,
        Some(cell) => match cell.text.chars().next() {
            None => CellClass::Blank,
            Some(ch) if ch.is_whitespace() => CellClass::Blank,
            Some(ch) if is_word_char(ch) => CellClass::Word,
            Some(_) => CellClass::Other,
        },
    }
}

/// The cells of a logical line, in reading order.
///
/// Bounded by the grid, which the protocol caps at `MAX_SCREEN_CELLS`, so this cannot be
/// made large by a peer.
fn line_cells(grid: &Grid, rows: RangeInclusive<u16>) -> Vec<CellPos> {
    let mut cells = Vec::new();
    for row in rows {
        for col in 0..grid.cols {
            cells.push(CellPos::new(row, col));
        }
    }
    cells
}

/// The class of a cell, with a wide glyph's trailer taking its leader's class.
///
/// Without this a double-click on the second half of an emoji would see a blank cell and
/// select the gap rather than the glyph.
fn class_at(grid: &Grid, at: CellPos) -> CellClass {
    let cell = grid.cell(at.row, at.col);
    if cell.is_some_and(Cell::is_trailer) && at.col > 0 {
        return class_of(grid.cell(at.row, at.col - 1));
    }
    class_of(cell)
}

/// The run of same-class cells around a cell, as a half-open selection range.
///
/// Runs across a hard wrap, so a path the terminal broke at the margin comes out whole.
fn word_span(grid: &Grid, at: CellPos) -> (CellPos, CellPos) {
    let cols = grid.cols;
    let at = CellPos::new(
        at.row.min(grid.rows.saturating_sub(1)),
        at.col.min(cols.saturating_sub(1)),
    );
    let cells = line_cells(grid, logical_line(grid, at.row));
    let Some(index) = cells.iter().position(|pos| *pos == at) else {
        return (at, CellPos::new(at.row, at.col.saturating_add(1)));
    };
    let class = class_at(grid, at);
    let mut first = index;
    while first > 0 && class_at(grid, cells[first - 1]) == class {
        first -= 1;
    }
    let mut last = index;
    while last + 1 < cells.len() && class_at(grid, cells[last + 1]) == class {
        last += 1;
    }
    let start = cells[first];
    let end = match cells.get(last + 1) {
        Some(after) => *after,
        // The run reaches the end of the line: one past the last column of its last row,
        // which `contains` reads as "to the end of this row".
        None => CellPos::new(cells[last].row, cols),
    };
    (start, end)
}

/// The last row with anything on it, for a selection that means "everything".
fn last_row_with_content(grid: &Grid) -> Option<u16> {
    (0..grid.rows)
        .rev()
        .find(|row| (0..grid.cols).any(|col| grid.cell(*row, col).is_some_and(|c| !c.is_blank())))
}

/// How a keystroke moves the keyboard's cell cursor.
///
/// Selection by keyboard exists because a selection only reachable with a pointer is a
/// feature half the users of a terminal cannot use. The motions are the ones a terminal
/// user already knows from `less` and from `tmux`'s copy mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    Left,
    Right,
    Up,
    Down,
    WordLeft,
    WordRight,
    LineStart,
    LineEnd,
    ScreenTop,
    ScreenBottom,
}

/// Where a motion puts the cursor.
///
/// The cursor is a **caret between cells**, not a cell: its column may reach `cols`, one past
/// the last column, exactly as a pointer's may. That is what lets Shift+Right at the end of a
/// line include the last character — a cursor clamped to `cols - 1` can only ever select up
/// to the second-to-last one, which is the off-by-one every hand-rolled selection has.
///
/// Clamped at every edge rather than wrapping: a cursor that jumped from the end of one line
/// to the start of the next would make a careful selection impossible to place.
pub fn advance(grid: &Grid, from: CellPos, motion: Motion) -> CellPos {
    let last_row = grid.rows.saturating_sub(1);
    let past_last_col = grid.cols;
    let from = CellPos::new(from.row.min(last_row), from.col.min(past_last_col));
    match motion {
        Motion::Left => CellPos::new(from.row, from.col.saturating_sub(1)),
        Motion::Right => CellPos::new(from.row, (from.col + 1).min(past_last_col)),
        Motion::Up => CellPos::new(from.row.saturating_sub(1), from.col),
        Motion::Down => CellPos::new((from.row + 1).min(last_row), from.col),
        Motion::LineStart => CellPos::new(*logical_line(grid, from.row).start(), 0),
        Motion::LineEnd => {
            let row = *logical_line(grid, from.row).end();
            CellPos::new(row, past_last_col)
        }
        Motion::ScreenTop => CellPos::new(0, 0),
        Motion::ScreenBottom => CellPos::new(last_row, past_last_col),
        Motion::WordLeft => word_left(grid, from),
        Motion::WordRight => word_right(grid, from),
    }
}

/// The start of the word to the left, skipping whatever separates them.
fn word_left(grid: &Grid, from: CellPos) -> CellPos {
    let (start, _) = word_span(grid, from);
    if start != from {
        return start;
    }
    let before = advance(grid, from, Motion::Left);
    if before == from {
        return from;
    }
    word_span(grid, before).0
}

/// One past the end of the word to the right, which is where a caret goes.
fn word_right(grid: &Grid, from: CellPos) -> CellPos {
    let (_, end) = word_span(grid, from);
    if end.key() > from.key() {
        CellPos::new(end.row.min(grid.rows.saturating_sub(1)), end.col)
    } else {
        advance(grid, from, Motion::Right)
    }
}

/// The cell a point falls in, given the grid's origin and cell size.
///
/// Clamped to the grid rather than returning `None` for a point outside it: a drag
/// that leaves the pane should extend the selection to the edge, which is what every
/// terminal does, and a `None` there would make the selection stop following the
/// pointer.
pub fn cell_at(point: egui::Pos2, origin: egui::Pos2, cell: egui::Vec2, grid: &Grid) -> CellPos {
    if cell.x <= 0.0 || cell.y <= 0.0 {
        return CellPos::new(0, 0);
    }
    let col = ((point.x - origin.x) / cell.x).floor();
    let row = ((point.y - origin.y) / cell.y).floor();
    CellPos::new(
        clamp_index(row, grid.rows),
        // Columns clamp to `cols` rather than `cols - 1`: a selection may legitimately
        // end just past the last character, which is how a whole line gets selected.
        clamp_index(col, grid.cols.saturating_add(1)),
    )
}

fn clamp_index(value: f32, limit: u16) -> u16 {
    if !value.is_finite() || value < 0.0 {
        return 0;
    }
    let limit = limit.saturating_sub(1);
    (value as u32).min(limit as u32) as u16
}

/// How many rows a drag that has left the pane should scroll, and in which direction.
///
/// Positive is backwards into history, matching [`crate::terminal::PaneAction::Scroll`].
/// Proportional to how far past the edge the pointer is, so a small overshoot creeps and a
/// deliberate throw to the top of the screen moves quickly — and capped, because a drag to
/// the top of a large display should not jump the viewport by fifty rows in one frame.
pub fn autoscroll_rows(pointer_y: f32, rect: egui::Rect, cell_height: f32) -> i32 {
    const MOST_ROWS_PER_FRAME: f32 = 4.0;
    if cell_height <= 0.0 || !pointer_y.is_finite() {
        return 0;
    }
    let past = if pointer_y < rect.min.y {
        rect.min.y - pointer_y
    } else if pointer_y > rect.max.y {
        -(pointer_y - rect.max.y)
    } else {
        return 0;
    };
    let rows = (past / cell_height)
        .trunc()
        .clamp(-MOST_ROWS_PER_FRAME, MOST_ROWS_PER_FRAME);
    // A pointer only just past the edge is still asking to scroll: a whole cell of
    // overshoot before anything moves would feel stuck.
    if rows == 0.0 {
        return if past > 0.0 { 1 } else { -1 };
    }
    rows as i32
}

/// How many of the rows an auto-scroll asked for the viewport can actually move.
///
/// Asking for four rows of history that does not exist would slide the selection to
/// somewhere the screen never went, so the answer is capped by what is there: backwards, the
/// history above the viewport; forwards, how far back it already is.
pub fn scrollable_rows(wanted: i32, offset: usize, len: usize) -> i32 {
    let available = if wanted > 0 {
        len.saturating_sub(offset)
    } else {
        offset
    };
    let available = i64::try_from(available).unwrap_or(i64::MAX);
    (i64::from(wanted.abs()).min(available) as i32) * wanted.signum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid() -> Grid {
        Grid::from_lines(&["hello world", "second line", "third"], 20)
    }

    /// A screen where one logical line was broken at the margin, which is what the copy
    /// and triple-click rules are about.
    fn wrapped_grid() -> Grid {
        // Row 0 is exactly as wide as the grid, which is what a row that wrapped looks
        // like: the terminal broke it because it ran out of columns.
        let mut grid = Grid::from_lines(
            &["/Users/xy/personal-w", "orkspace/turn/src.rs", "done"],
            20,
        );
        assert!(grid.set_row_wrapped(0, true));
        grid
    }

    /// The whole of the path the wrapped rows hold between them.
    const WRAPPED_PATH: &str = "/Users/xy/personal-workspace/turn/src.rs";

    #[test]
    fn a_selection_within_one_row_copies_exactly_those_cells() {
        let mut selection = Selection::new(CellPos::new(0, 0), SelectionKind::Linear);
        selection.extend_to(CellPos::new(0, 5));
        assert_eq!(selection.text(&grid()), "hello");
        assert!(selection.contains(0, 0));
        assert!(selection.contains(0, 4));
        assert!(
            !selection.contains(0, 5),
            "the cell under the pointer is not included, as in any text selection"
        );
    }

    #[test]
    fn a_selection_dragged_backwards_reads_the_same_as_one_dragged_forwards() {
        let mut forwards = Selection::new(CellPos::new(0, 0), SelectionKind::Linear);
        forwards.extend_to(CellPos::new(0, 5));
        let mut backwards = Selection::new(CellPos::new(0, 5), SelectionKind::Linear);
        backwards.extend_to(CellPos::new(0, 0));
        assert_eq!(forwards.text(&grid()), backwards.text(&grid()));
    }

    /// The padding rule. A terminal row is the full width, so a naive copy of three
    /// lines produces the text plus sixty spaces.
    #[test]
    fn trailing_blanks_are_not_copied_because_a_terminal_pads_every_row() {
        let mut selection = Selection::new(CellPos::new(0, 0), SelectionKind::Linear);
        selection.extend_to(CellPos::new(2, 5));
        assert_eq!(selection.text(&grid()), "hello world\nsecond line\nthird");
    }

    #[test]
    fn a_selection_spanning_rows_takes_the_end_of_one_and_the_start_of_the_next() {
        let mut selection = Selection::new(CellPos::new(0, 6), SelectionKind::Linear);
        selection.extend_to(CellPos::new(1, 6));
        assert_eq!(selection.text(&grid()), "world\nsecond");
    }

    /// The reason a block selection exists: one column out of tabular output.
    #[test]
    fn a_block_selection_takes_a_rectangle_rather_than_following_the_text() {
        let grid = Grid::from_lines(&["alpha  1", "beta   2", "gamma  3"], 8);
        let mut selection = Selection::new(CellPos::new(0, 7), SelectionKind::Block);
        selection.extend_to(CellPos::new(2, 8));
        assert_eq!(
            selection.text(&grid),
            "1\n2\n3",
            "a linear selection could not express this"
        );

        let mut linear = Selection::new(CellPos::new(0, 7), SelectionKind::Linear);
        linear.extend_to(CellPos::new(2, 8));
        assert_ne!(linear.text(&grid), "1\n2\n3");
    }

    #[test]
    fn a_block_selection_dragged_leftwards_still_covers_the_columns_between() {
        let grid = Grid::from_lines(&["abcdef", "ghijkl"], 6);
        let mut selection = Selection::new(CellPos::new(0, 4), SelectionKind::Block);
        selection.extend_to(CellPos::new(1, 1));
        assert_eq!(selection.text(&grid), "bcd\nhij");
    }

    /// A rectangle dragged straight down one column is the gesture for "this column".
    #[test]
    fn a_block_selection_down_a_single_column_still_covers_that_column() {
        let grid = Grid::from_lines(&["abc", "def", "ghi"], 3);
        let mut selection = Selection::new(CellPos::new(0, 1), SelectionKind::Block);
        selection.extend_to(CellPos::new(2, 1));
        assert_eq!(selection.text(&grid), "b\ne\nh");
    }

    /// A wide glyph is one glyph, and its trailer must not become a space.
    #[test]
    fn a_wide_glyph_is_copied_once_and_not_padded_by_its_trailer() {
        let mut grid = Grid::from_lines(&["a  b"], 6);
        assert!(grid.set_wide(0, 1, "漢"));
        let mut selection = Selection::new(CellPos::new(0, 0), SelectionKind::Linear);
        selection.extend_to(CellPos::new(0, 4));
        assert_eq!(selection.text(&grid), "a漢b");
    }

    /// A rectangle whose edge falls between the halves of a wide glyph. The leader is
    /// inside, so its glyph is taken once; a trailer with no leader contributes nothing,
    /// because half a character is not a character.
    #[test]
    fn a_block_selection_that_cuts_a_wide_glyph_takes_it_once_or_not_at_all() {
        let mut grid = Grid::from_lines(&["ab  cd", "ef  gh"], 6);
        assert!(grid.set_wide(0, 2, "漢"));
        assert!(grid.set_wide(1, 2, "字"));

        // Columns 2..4: both halves inside.
        let mut whole = Selection::new(CellPos::new(0, 2), SelectionKind::Block);
        whole.extend_to(CellPos::new(1, 4));
        assert_eq!(whole.text(&grid), "漢\n字");

        // Columns 3..4: only the trailer, which holds no glyph of its own.
        let mut trailing = Selection::new(CellPos::new(0, 3), SelectionKind::Block);
        trailing.extend_to(CellPos::new(1, 4));
        assert_eq!(trailing.text(&grid), "\n");
    }

    #[test]
    fn an_empty_selection_copies_nothing_so_a_click_does_not_clear_the_clipboard() {
        let selection = Selection::new(CellPos::new(1, 3), SelectionKind::Linear);
        assert!(selection.is_empty());
        assert_eq!(selection.text(&grid()), "");
        assert!(!selection.contains(1, 3));
    }

    /// The shapes a terminal user double-clicks. Every one of these is one thing to the
    /// person pointing at it, and a word class borrowed from a text editor splits all
    /// four.
    #[test]
    fn a_double_click_takes_a_path_a_flag_a_url_and_an_identifier_whole() {
        let cases = [
            ("src/main.rs:42", 4u16),
            ("--jobs=4", 3),
            ("https://example.com/a?b=c#d", 12),
            ("some_ident", 5),
            ("~/.config/turn/turn.toml", 9),
            ("user@example.com", 6),
            ("$HOME/bin", 2),
        ];
        for (text, col) in cases {
            let line = format!("run {text} now");
            let grid = Grid::from_lines(&[line.as_str()], 60);
            let selection = Selection::word(&grid, CellPos::new(0, 4 + col), SelectionKind::Linear);
            assert_eq!(
                selection.text(&grid),
                text,
                "double-clicking inside {text} must take all of it"
            );
        }
    }

    /// The other half of the class: the separators. `foo,bar` is two things.
    #[test]
    fn punctuation_that_separates_two_things_is_not_part_of_either() {
        for (line, col, expected) in [
            ("foo,bar", 1u16, "foo"),
            ("foo,bar", 5, "bar"),
            ("(wrapped)", 3, "wrapped"),
            ("'quoted'", 3, "quoted"),
            ("a|b", 0, "a"),
            ("x; y", 0, "x"),
            ("one*two", 5, "two"),
        ] {
            let grid = Grid::from_lines(&[line], 20);
            let selection = Selection::word(&grid, CellPos::new(0, col), SelectionKind::Linear);
            assert_eq!(selection.text(&grid), expected, "in {line:?} at {col}");
        }
    }

    /// A double-click always selects something, so it never reads as broken.
    #[test]
    fn a_double_click_on_a_gap_takes_the_gap_and_on_punctuation_takes_the_punctuation() {
        let grid = Grid::from_lines(&["ab    cd", "x|||y"], 8);
        let gap = Selection::word(&grid, CellPos::new(0, 3), SelectionKind::Linear);
        assert_eq!(gap.text(&grid), "", "a run of blanks trims to nothing");
        assert!(!gap.is_empty(), "but the run itself is highlighted");
        assert!(gap.contains(0, 2) && gap.contains(0, 5) && !gap.contains(0, 6));

        let pipes = Selection::word(&grid, CellPos::new(1, 2), SelectionKind::Linear);
        assert_eq!(pipes.text(&grid), "|||");
    }

    /// The point of carrying the wrap flag: a path the terminal broke at the margin is
    /// still one path, and a double-click on either half takes all of it.
    #[test]
    fn a_word_broken_by_a_hard_wrap_is_taken_whole() {
        let grid = wrapped_grid();
        for at in [CellPos::new(0, 4), CellPos::new(1, 3)] {
            let selection = Selection::word(&grid, at, SelectionKind::Linear);
            assert_eq!(selection.text(&grid), WRAPPED_PATH, "from {at:?}");
        }
    }

    /// Copying across a hard wrap must not invent a newline the program never printed.
    #[test]
    fn a_hard_wrapped_line_is_copied_as_one_line() {
        let grid = wrapped_grid();
        let mut selection = Selection::new(CellPos::new(0, 0), SelectionKind::Linear);
        selection.extend_to(CellPos::new(2, 4));
        assert_eq!(
            selection.text(&grid),
            format!("{WRAPPED_PATH}\ndone"),
            "the wrap is joined; the newline the program printed is kept"
        );
    }

    /// A wrapped row's trailing spaces are text rather than padding: the program printed
    /// them and the line continues after them.
    #[test]
    fn spaces_at_the_end_of_a_wrapped_row_survive_because_the_line_continues() {
        let mut grid = Grid::from_lines(&["hello     ", "world"], 10);
        assert!(grid.set_row_wrapped(0, true));
        let mut selection = Selection::new(CellPos::new(0, 0), SelectionKind::Linear);
        selection.extend_to(CellPos::new(1, 5));
        assert_eq!(selection.text(&grid), "hello     world");
    }

    /// Triple-click takes the logical line, which for a wrapped line is every row of it.
    #[test]
    fn a_triple_click_takes_the_whole_logical_line_not_one_visual_row() {
        let grid = wrapped_grid();
        for row in [0u16, 1] {
            let selection = Selection::line(&grid, CellPos::new(row, 7));
            assert_eq!(selection.text(&grid), WRAPPED_PATH);
        }
        let unwrapped = Selection::line(&grid, CellPos::new(2, 0));
        assert_eq!(unwrapped.text(&grid), "done");
    }

    #[test]
    fn a_word_drag_grows_by_words_in_both_directions() {
        let grid = Grid::from_lines(&["alpha beta gamma delta"], 24);
        let mut selection = Selection::word(&grid, CellPos::new(0, 7), SelectionKind::Linear);
        assert_eq!(selection.text(&grid), "beta");

        selection.extend_in_grid(&grid, CellPos::new(0, 13));
        assert_eq!(
            selection.text(&grid),
            "beta gamma",
            "dragging into the next word takes all of it"
        );

        selection.extend_in_grid(&grid, CellPos::new(0, 2));
        assert_eq!(
            selection.text(&grid),
            "alpha beta",
            "dragging back past the origin extends the other way"
        );
    }

    #[test]
    fn a_line_drag_grows_by_logical_lines() {
        let grid = wrapped_grid();
        let mut selection = Selection::line(&grid, CellPos::new(2, 0));
        selection.extend_in_grid(&grid, CellPos::new(0, 3));
        assert_eq!(
            selection.text(&grid),
            format!("{WRAPPED_PATH}\ndone"),
            "the wrapped line above is taken whole"
        );
    }

    /// Shift-click extends an existing selection rather than starting a new one, and it
    /// respects the granularity the selection already has.
    #[test]
    fn shift_clicking_extends_the_selection_that_is_already_there() {
        let grid = Grid::from_lines(&["alpha beta gamma"], 20);
        let mut character = Selection::new(CellPos::new(0, 0), SelectionKind::Linear);
        character.extend_to(CellPos::new(0, 5));
        character.extend_in_grid(&grid, CellPos::new(0, 10));
        assert_eq!(character.text(&grid), "alpha beta");

        let mut word = Selection::word(&grid, CellPos::new(0, 2), SelectionKind::Linear);
        word.extend_in_grid(&grid, CellPos::new(0, 12));
        assert_eq!(
            word.text(&grid),
            "alpha beta gamma",
            "extending a word selection lands on a word boundary"
        );
    }

    #[test]
    fn select_all_stops_after_the_last_row_with_anything_on_it() {
        let mut grid = Grid::blank(40, 20);
        for (row, line) in ["one", "two"].iter().enumerate() {
            for (col, ch) in line.chars().enumerate() {
                if let Some(cell) = grid.cell_mut(row as u16, col as u16) {
                    cell.text = ch.to_string();
                }
            }
        }
        assert_eq!(
            Selection::all(&grid).text(&grid),
            "one\ntwo",
            "thirty-eight blank rows must not become thirty-eight blank lines"
        );

        // And a screen with nothing on it offers nothing rather than a wall of newlines.
        assert_eq!(
            Selection::all(&Grid::blank(24, 80)).text(&Grid::blank(24, 80)),
            ""
        );
    }

    /// An auto-scrolling drag moves the viewport under the selection, so the anchor has
    /// to move with the text or the highlight crawls.
    #[test]
    fn a_selection_slides_with_the_viewport_when_a_drag_scrolls_it() {
        let mut selection = Selection::new(CellPos::new(5, 2), SelectionKind::Linear);
        selection.extend_to(CellPos::new(7, 4));
        selection.shift_rows(3, 24);
        assert_eq!(selection.anchor, CellPos::new(8, 2));
        assert_eq!(selection.head, CellPos::new(10, 4));

        // Clamped at the edges: a start that scrolled off the top is still a selection of
        // what is left on screen.
        selection.shift_rows(-100, 24);
        assert_eq!(selection.anchor.row, 0);
        selection.shift_rows(1_000, 24);
        assert_eq!(selection.head.row, 23);
    }

    #[test]
    fn a_drag_past_the_edge_asks_for_a_scroll_in_the_direction_it_left() {
        let rect = egui::Rect::from_min_max(egui::pos2(0.0, 100.0), egui::pos2(200.0, 300.0));
        assert_eq!(autoscroll_rows(200.0, rect, 15.0), 0, "inside, nothing");
        assert!(
            autoscroll_rows(95.0, rect, 15.0) > 0,
            "past the top scrolls backwards into history"
        );
        assert!(
            autoscroll_rows(305.0, rect, 15.0) < 0,
            "past the bottom scrolls forwards"
        );
        assert_eq!(
            autoscroll_rows(-10_000.0, rect, 15.0),
            4,
            "a throw off the screen is capped rather than jumping the viewport"
        );
        assert_eq!(
            autoscroll_rows(-10_000.0, rect, 0.0),
            0,
            "no cell size, no scroll"
        );
    }

    /// The viewport can only move as far as there is history to move into. Shifting the
    /// selection by more than that would put the highlight where the screen never went.
    #[test]
    fn an_autoscroll_is_capped_by_the_history_that_exists() {
        // Four rows back, but only one row of history above the viewport.
        assert_eq!(scrollable_rows(4, 99, 100), 1);
        assert_eq!(scrollable_rows(4, 100, 100), 0, "already at the top");
        assert_eq!(scrollable_rows(4, 0, 500), 4);
        // Forwards is bounded by how far back the viewport already is.
        assert_eq!(scrollable_rows(-4, 2, 500), -2);
        assert_eq!(scrollable_rows(-4, 0, 500), 0, "already live");
        assert_eq!(scrollable_rows(0, 10, 500), 0);
    }

    #[test]
    fn a_point_maps_to_the_cell_it_falls_in() {
        let grid = Grid::blank(24, 80);
        let origin = egui::pos2(10.0, 20.0);
        let cell = egui::vec2(8.0, 17.0);
        assert_eq!(
            cell_at(egui::pos2(10.0, 20.0), origin, cell, &grid),
            CellPos::new(0, 0)
        );
        assert_eq!(
            cell_at(
                egui::pos2(10.0 + 8.0 * 3.5, 20.0 + 17.0 * 2.5),
                origin,
                cell,
                &grid
            ),
            CellPos::new(2, 3),
            "a point inside a cell belongs to that cell, not the nearest boundary"
        );
    }

    /// A drag that leaves the pane must extend the selection to the edge rather than
    /// stop following the pointer.
    #[test]
    fn a_point_outside_the_grid_is_clamped_to_its_edge() {
        let grid = Grid::blank(24, 80);
        let origin = egui::pos2(0.0, 0.0);
        let cell = egui::vec2(8.0, 17.0);
        assert_eq!(
            cell_at(egui::pos2(-500.0, -500.0), origin, cell, &grid),
            CellPos::new(0, 0)
        );
        let far = cell_at(egui::pos2(100_000.0, 100_000.0), origin, cell, &grid);
        assert_eq!(far.row, 23);
        assert_eq!(
            far.col, 80,
            "the column may reach one past the last cell, which is how a whole line \
             gets selected"
        );
    }

    #[test]
    fn a_pane_with_no_cell_size_yet_does_not_divide_by_zero() {
        let grid = Grid::blank(4, 4);
        assert_eq!(
            cell_at(
                egui::pos2(5.0, 5.0),
                egui::Pos2::ZERO,
                egui::Vec2::ZERO,
                &grid
            ),
            CellPos::new(0, 0)
        );
    }

    #[test]
    fn a_selection_that_runs_past_the_bottom_of_the_grid_copies_what_exists() {
        let mut selection = Selection::new(CellPos::new(2, 0), SelectionKind::Linear);
        selection.extend_to(CellPos::new(50, 5));
        assert_eq!(selection.text(&grid()), "third");
    }

    // ---------------------------------------------------------------------------------
    // The copy rules against a screen a real terminal parsed. A grid built by hand can be
    // made to satisfy any rule — including a wrap flag set where no wrap happened — so the
    // emulator has to be the one that decides where the lines broke and which cells are the
    // halves of a wide glyph.
    // ---------------------------------------------------------------------------------

    /// A 20-column screen holding a path too long for it, a line of wide glyphs, and a line
    /// padded with spaces the program printed.
    fn parsed_screen() -> Grid {
        let mut parser = vt100::Parser::new(6, 20, 0);
        // 33 characters into 20 columns: the terminal breaks it at the margin and records
        // that it did.
        // `\r\n`, not a bare line feed: a line feed moves down without returning to column
        // zero, so every line after the first would start wherever the last one ended and no
        // row below would be where a terminal really puts it.
        parser.process(b"/Users/xy/personal-workspace/turn\r\n");
        parser.process("漢字 ok\r\n".as_bytes());
        parser.process(b"done      \r\n");
        turn_proto::cells::from_screen(parser.screen())
    }

    #[test]
    fn a_terminal_that_wrapped_a_line_is_copied_as_one_line() {
        let grid = parsed_screen();
        assert!(
            grid.row_wrapped(0),
            "the emulator has to be the one that says the row wrapped"
        );
        assert!(!grid.row_wrapped(1), "and that the next one did not");

        let mut selection = Selection::new(CellPos::new(0, 0), SelectionKind::Linear);
        selection.extend_to(CellPos::new(2, 20));
        assert_eq!(
            selection.text(&grid),
            "/Users/xy/personal-workspace/turn\n漢字 ok",
            "the wrap is joined, the printed newline is kept, and the wide glyphs are \
             taken once each"
        );
    }

    #[test]
    fn a_double_click_on_a_path_a_terminal_wrapped_takes_all_of_it() {
        let grid = parsed_screen();
        for at in [CellPos::new(0, 3), CellPos::new(1, 2)] {
            assert_eq!(
                Selection::word(&grid, at, SelectionKind::Linear).text(&grid),
                "/Users/xy/personal-workspace/turn",
                "from {at:?}"
            );
        }
    }

    /// The padding rule, on a real screen: the program printed six spaces after `done` and a
    /// terminal pads the rest of the row to its full width. Neither belongs on a clipboard.
    #[test]
    fn the_padding_a_terminal_adds_is_not_copied_from_a_real_screen() {
        let grid = parsed_screen();
        let selection = Selection::line(&grid, CellPos::new(3, 0));
        assert_eq!(selection.text(&grid), "done");
    }

    /// A wide glyph occupies two columns of the parsed screen, and its second column is a
    /// trailer with no text of its own. Copying it twice is the classic emoji corruption.
    /// A picture's marker is Turn's own bookkeeping: an invisible private-use character with
    /// no glyph in any font. Pasting one into a commit message would be a defect nobody could
    /// see, so a tile copies as a space.
    #[test]
    fn a_picture_copies_as_the_space_it_occupies_and_never_as_its_marker() {
        use turn_proto::images::{GridImage, ImageCell, ImageId};

        let mut grid = Grid::from_lines(&["ab    cd"], 8);
        for dx in 0..4u16 {
            if let Some(cell) = grid.cell_mut(0, 2 + dx) {
                *cell = Cell::image(ImageCell::new(0, 0, dx)).expect("an addressable tile");
            }
        }
        grid.images
            .push(GridImage::new(0, ImageId(9), 1, 4, 32, 16));

        let mut selection = Selection::new(CellPos::new(0, 0), SelectionKind::Linear);
        selection.extend_to(CellPos::new(0, 8));
        let copied = selection.text(&grid);
        assert_eq!(copied, "ab    cd");
        assert!(
            !copied.chars().any(turn_proto::is_marker),
            "a marker reached the clipboard: {copied:?}"
        );

        // And a double-click beside a picture does not drag through it.
        let word = Selection::word(&grid, CellPos::new(0, 0), SelectionKind::Linear);
        assert_eq!(word.text(&grid), "ab");
    }

    #[test]
    fn a_wide_glyph_from_a_real_screen_is_copied_once() {
        let grid = parsed_screen();
        assert!(
            grid.cell(2, 1).is_some_and(Cell::is_trailer),
            "the emulator marked the second column as the trailer"
        );
        let mut selection = Selection::new(CellPos::new(2, 0), SelectionKind::Linear);
        selection.extend_to(CellPos::new(2, 4));
        assert_eq!(
            selection.text(&grid),
            "漢字",
            "two glyphs over four columns, and not a space between them"
        );
    }

    /// The keyboard's own cursor, for a user who has no pointer. Every motion stays on
    /// the grid.
    #[test]
    fn the_keyboard_cursor_moves_by_cell_word_and_line_and_stops_at_the_edges() {
        let grid = Grid::from_lines(&["alpha beta", "gamma"], 10);
        let start = CellPos::new(0, 0);
        assert_eq!(advance(&grid, start, Motion::Right), CellPos::new(0, 1));
        assert_eq!(
            advance(&grid, start, Motion::Left),
            start,
            "the left edge holds"
        );
        assert_eq!(
            advance(&grid, start, Motion::Up),
            start,
            "the top edge holds"
        );
        assert_eq!(advance(&grid, start, Motion::Down), CellPos::new(1, 0));
        assert_eq!(
            advance(&grid, CellPos::new(1, 0), Motion::Down),
            CellPos::new(1, 0)
        );

        assert_eq!(
            advance(&grid, start, Motion::WordRight),
            CellPos::new(0, 5),
            "past the end of `alpha`"
        );
        assert_eq!(
            advance(&grid, CellPos::new(0, 7), Motion::WordLeft),
            CellPos::new(0, 6),
            "to the start of the word the cursor is inside"
        );
        assert_eq!(
            advance(&grid, CellPos::new(0, 6), Motion::WordLeft),
            CellPos::new(0, 5),
            "then across the separator"
        );
        assert_eq!(
            advance(&grid, CellPos::new(0, 3), Motion::LineEnd).col,
            10,
            "one past the last column: the caret sits after the last character"
        );
        assert_eq!(advance(&grid, CellPos::new(0, 3), Motion::LineStart).col, 0);
        assert_eq!(
            advance(&grid, start, Motion::ScreenBottom),
            CellPos::new(1, 10)
        );
        assert_eq!(advance(&grid, CellPos::new(1, 4), Motion::ScreenTop), start);
    }

    /// A logical line is the unit the keyboard's Home and End keys work in, too.
    #[test]
    fn the_keyboard_line_motions_span_a_hard_wrap() {
        let grid = wrapped_grid();
        assert_eq!(
            advance(&grid, CellPos::new(1, 4), Motion::LineStart),
            CellPos::new(0, 0)
        );
        assert_eq!(
            advance(&grid, CellPos::new(0, 4), Motion::LineEnd),
            CellPos::new(1, 20),
            "the end of the *logical* line, one past its last column"
        );
    }
}
