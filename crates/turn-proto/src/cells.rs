//! The terminal grid: a pane's screen as cells, and the form it travels in.
//!
//! A pane's screen crosses this boundary as cells rather than as an escape-code
//! stream, because the daemon has already parsed it — it must, since previews and
//! the output heuristics work with no client attached. The types are deliberately
//! dumb: they store what to draw and answer questions about geometry, and they decide
//! nothing.
//!
//! They live in the protocol crate rather than in the client so that both ends share
//! one definition, and so that [`from_screen`] — the single reading of what a
//! [`vt100::Screen`] means as cells — sits beside them. That is what makes "the
//! daemon's screen and the client's screen agree" a property of there being one
//! function rather than of two implementations being kept in step.
//!
//! ## Why the modes travel with the screen
//!
//! A grid is not only colours and glyphs. Whether an arrow key sends `ESC [ A` or
//! `ESC O A`, whether a wheel notch becomes a mouse report or a scroll, and whether
//! a paste is bracketed are all decisions the *program* made through escape
//! sequences the daemon has already parsed. If the client guessed at them, arrow
//! keys would break inside `vim` and a paste into an editor would execute half of
//! itself. So [`Grid`] carries the modes next to the cells: the client reads them
//! and never derives them.
//!
//! ## Why the wire form is runs and not cells
//!
//! A 40x120 screen is 4,800 cells and it travels on every update, so the naive
//! encoding matters. One JSON object per cell — `{"text":"a","fg":null,…}` — is
//! around 30 bytes each, so even a blank screen would cost well over 100 kB a frame.
//!
//! So a row is encoded as **runs**: consecutive cells that share a colour and
//! attribute set become one object carrying the run's text and its length. A blank
//! 120-column row is `{"n":120}`; a prompt line is two or three objects. That turns
//! the same screen into roughly a kilobyte — measured by
//! `a_full_screen_costs_about_a_kilobyte_rather_than_a_hundred` in this module — for
//! one pass over the row in each direction.
//!
//! Field names inside a run are single letters (`t`, `n`, `f`, `b`, `a`) because
//! there are thousands of them per screen. The grid's own fields are spelled out,
//! because there is one of each and a frame in a bug report should be readable.
//!
//! ## Decoding is strict
//!
//! A run whose text does not account for exactly the cells it claims, a row that is
//! not exactly as wide as the grid, or a grid larger than [`MAX_SCREEN_CELLS`] is
//! **rejected**. Quietly repairing malformed input is how two implementations of a
//! protocol end up disagreeing, and the cap is what stops a thirty-byte line from
//! asking the receiver to allocate four billion cells.
//!
//! ## What is *not* sanitised here
//!
//! Screen cells are the terminal's own contents and are passed through as the program
//! wrote them. That is deliberate and it is the opposite of the rule for labels: a
//! title or an Activity Preview line goes through `turn_pty::sanitise_label`, because those
//! end up in Turn's chrome where a direction override could make text lie about
//! itself. Inside a pane the client paints cell by cell, so no cell can reorder its
//! neighbours, and stripping characters would mean a terminal that does not show what
//! the program printed.

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::VecDeque;

/// Most cells a single grid may describe, in either direction.
///
/// 65,536 is 256x256: far past any real terminal — a 6K display at a tiny font is
/// around 20,000 cells — and small enough that the worst case stays bounded. That
/// worst case is a screen where no two adjacent cells share a style, which is one
/// run per cell at about 30 bytes: 2 MB, well inside the frame limit.
///
/// Enforced on the way *in*, before anything is allocated, so a peer cannot ask a
/// receiver for gigabytes with a short line.
pub const MAX_SCREEN_CELLS: usize = 65_536;

/// Most durable history rows carried by one attachment.
pub const MAX_SCROLLBACK_ROWS: usize = 5_000;
/// Serialized row budget reserved for history inside the 8 MiB protocol frame.
pub const MAX_SCROLLBACK_WIRE_BYTES: usize = 3 * 1024 * 1024;

/// A colour, already resolved. The daemon maps the terminal's palette indices to
/// concrete values so the client never has to know which of the sixteen
/// conventions a program meant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self(r, g, b)
    }
}

/// Text attributes, packed into a byte so a full screen of cells stays cheap to
/// send and to compare.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CellAttrs(pub u8);

impl CellAttrs {
    pub const BOLD: u8 = 1 << 0;
    pub const ITALIC: u8 = 1 << 1;
    pub const UNDERLINE: u8 = 1 << 2;
    /// The program asked for reversed video and there was nothing to swap, because
    /// both of its colours were the theme's own.
    ///
    /// Reversal is applied when the grid is *built*: whenever either colour is
    /// concrete, [`from_screen`] exchanges them and leaves this flag clear. It is set
    /// only for the one case a daemon cannot resolve — default on default — where the
    /// swap belongs to whoever owns the theme. A client must therefore never swap a
    /// cell that does not carry this flag, or a selection highlight would come out
    /// twice-reversed and invisible.
    pub const INVERSE: u8 = 1 << 3;
    pub const DIM: u8 = 1 << 4;
    /// This cell is two columns wide: a CJK ideograph, an emoji. The cell to its
    /// right is its [`CellAttrs::WIDE_TRAILER`] and holds no glyph of its own.
    pub const WIDE: u8 = 1 << 5;
    /// The right-hand half of a [`CellAttrs::WIDE`] cell. Painted as background
    /// only.
    ///
    /// Represented as a cell rather than implied by its neighbour's width so the
    /// grid stays a plain rectangular array: a renderer walking it row by row never
    /// has to look backwards to know whether a column is real, which is the bug
    /// that makes an emoji shift the rest of a line.
    pub const WIDE_TRAILER: u8 = 1 << 6;

    pub fn has(&self, flag: u8) -> bool {
        self.0 & flag != 0
    }

    pub fn with(mut self, flag: u8) -> Self {
        self.0 |= flag;
        self
    }

    /// Whether nothing is set, so the wire form can leave the field out.
    pub fn is_plain(&self) -> bool {
        self.0 == 0
    }
}

/// One character cell.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cell {
    /// The cell's text. A `String` rather than a `char` because a grapheme
    /// cluster — an emoji with a modifier, a combining accent — occupies one cell
    /// and is more than one `char`.
    pub text: String,
    pub fg: Option<Rgb>,
    pub bg: Option<Rgb>,
    pub attrs: CellAttrs,
}

impl Cell {
    pub fn blank() -> Self {
        Self {
            text: String::new(),
            fg: None,
            bg: None,
            attrs: CellAttrs::default(),
        }
    }

    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Self::blank()
        }
    }

    /// A double-width cell, and the trailer that must follow it.
    pub fn wide(text: impl Into<String>) -> (Self, Self) {
        (
            Self {
                text: text.into(),
                attrs: CellAttrs::default().with(CellAttrs::WIDE),
                ..Self::blank()
            },
            Self {
                attrs: CellAttrs::default().with(CellAttrs::WIDE_TRAILER),
                ..Self::blank()
            },
        )
    }

    pub fn is_blank(&self) -> bool {
        self.text.trim().is_empty() && self.bg.is_none()
    }

    /// How many columns this cell's glyph occupies.
    pub fn columns(&self) -> u16 {
        if self.attrs.has(CellAttrs::WIDE) {
            2
        } else {
            1
        }
    }

    /// Whether this cell is the second half of its neighbour and paints no glyph.
    pub fn is_trailer(&self) -> bool {
        self.attrs.has(CellAttrs::WIDE_TRAILER)
    }

    /// Whether two cells can share a run: same colours, same attributes.
    fn same_style(&self, other: &Self) -> bool {
        self.fg == other.fg && self.bg == other.bg && self.attrs == other.attrs
    }
}

/// How the program in the pane wants the mouse reported.
///
/// Mirrors `vt100::MouseProtocolMode`. Reporting only ever happens when the program
/// asked for it: sending mouse escape sequences to a shell that did not enable them
/// would type garbage at the prompt.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseMode {
    /// The program is not interested. Wheel notches scroll Turn's own scrollback.
    #[default]
    None,
    /// Button presses and releases.
    Press,
    /// Presses, releases, and motion while a button is held.
    ButtonMotion,
    /// Everything, including motion with no button down.
    AnyMotion,
}

impl MouseMode {
    /// Whether the program wants to hear about the mouse at all.
    pub fn reports(&self) -> bool {
        !matches!(self, MouseMode::None)
    }

    /// Whether motion with no button held should be reported.
    pub fn reports_hover(&self) -> bool {
        matches!(self, MouseMode::AnyMotion)
    }

    /// Whether motion with a button held should be reported.
    pub fn reports_drag(&self) -> bool {
        matches!(self, MouseMode::ButtonMotion | MouseMode::AnyMotion)
    }
}

/// The keyboard and mouse modes a program set, which decide how input is encoded.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Modes {
    /// Arrow and Home/End keys send `ESC O x` rather than `ESC [ x`. Set by very
    /// nearly every full-screen application; getting it wrong makes arrows insert
    /// letters.
    #[serde(default, skip_serializing_if = "is_false")]
    pub application_cursor: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub application_keypad: bool,
    /// Pasted text is wrapped in `ESC [ 200 ~` … `ESC [ 201 ~`, so an editor can
    /// tell a paste from typing rather than auto-indenting it into soup.
    #[serde(default, skip_serializing_if = "is_false")]
    pub bracketed_paste: bool,
    #[serde(default, skip_serializing_if = "MouseMode::is_silent")]
    pub mouse: MouseMode,
}

impl MouseMode {
    /// Whether this is the "nobody is listening" mode the wire form can omit.
    fn is_silent(&self) -> bool {
        matches!(self, MouseMode::None)
    }
}

impl Modes {
    /// Whether nothing is set, so a grid of a plain shell can omit the field.
    pub fn is_default(&self) -> bool {
        *self == Modes::default()
    }
}

/// A pane's screen: rows of cells, plus where the cursor is.
#[derive(Debug, Clone, PartialEq)]
pub struct Grid {
    pub rows: u16,
    pub cols: u16,
    /// Row-major, `rows * cols` entries. Kept flat so a redraw walks memory in
    /// the order it draws.
    pub cells: Vec<Cell>,
    /// `(row, col)`, or `None` when the program hid the cursor.
    pub cursor: Option<(u16, u16)>,
    /// Set while a full-screen application is in control. The client uses it to
    /// stop offering scrollback that the program is managing itself.
    pub alternate_screen: bool,
    /// The input modes in force, from the program's own escape sequences.
    pub modes: Modes,
    /// How far back in the scrollback this grid was taken, in rows. Zero is the
    /// live screen.
    pub scrollback_offset: usize,
    /// How many rows of history exist above the live screen, so the client can say
    /// whether scrolling further up is possible rather than guessing.
    pub scrollback_len: usize,
}

impl Grid {
    /// An empty grid of the given size.
    pub fn blank(rows: u16, cols: u16) -> Self {
        let rows = rows.max(1);
        let cols = cols.max(1);
        Self {
            rows,
            cols,
            cells: vec![Cell::blank(); rows as usize * cols as usize],
            cursor: Some((0, 0)),
            alternate_screen: false,
            modes: Modes::default(),
            scrollback_offset: 0,
            scrollback_len: 0,
        }
    }

    /// Builds a grid from plain lines, for tests and for the empty-pane message.
    pub fn from_lines(lines: &[&str], cols: u16) -> Self {
        let mut grid = Self::blank(lines.len().max(1) as u16, cols);
        for (row, line) in lines.iter().enumerate() {
            for (col, ch) in line.chars().take(cols as usize).enumerate() {
                if let Some(cell) = grid.cell_mut(row as u16, col as u16) {
                    cell.text = ch.to_string();
                }
            }
        }
        grid
    }

    fn index(&self, row: u16, col: u16) -> Option<usize> {
        if row >= self.rows || col >= self.cols {
            return None;
        }
        Some(row as usize * self.cols as usize + col as usize)
    }

    pub fn cell(&self, row: u16, col: u16) -> Option<&Cell> {
        self.index(row, col).and_then(|i| self.cells.get(i))
    }

    pub fn cell_mut(&mut self, row: u16, col: u16) -> Option<&mut Cell> {
        match self.index(row, col) {
            Some(i) => self.cells.get_mut(i),
            None => None,
        }
    }

    /// Writes a double-width glyph and its trailer, so a caller cannot produce a
    /// wide cell without the trailer that keeps the row aligned.
    ///
    /// Returns false when there is no room for both halves, and writes nothing:
    /// half an emoji at the right margin would shift every column after it.
    pub fn set_wide(&mut self, row: u16, col: u16, text: impl Into<String>) -> bool {
        let (wide, trailer) = Cell::wide(text);
        match (self.index(row, col), self.index(row, col.saturating_add(1))) {
            (Some(first), Some(second)) if col + 1 < self.cols => {
                self.cells[first] = wide;
                self.cells[second] = trailer;
                true
            }
            _ => false,
        }
    }

    /// One row's cells, or an empty slice for a row that is not there.
    pub fn row(&self, row: u16) -> &[Cell] {
        match self.index(row, 0) {
            Some(start) => {
                let end = start + self.cols as usize;
                self.cells.get(start..end).unwrap_or(&[])
            }
            None => &[],
        }
    }

    /// Replaces one row's cells. `false` when the row is out of range or the wrong
    /// width, so a caller cannot half-write a row and leave the grid ragged.
    pub fn set_row(&mut self, row: u16, cells: &[Cell]) -> bool {
        if cells.len() != self.cols as usize {
            return false;
        }
        let Some(start) = self.index(row, 0) else {
            return false;
        };
        let end = start + self.cols as usize;
        match self.cells.get_mut(start..end) {
            Some(slot) => {
                slot.clone_from_slice(cells);
                true
            }
            None => false,
        }
    }

    /// Whether two grids are the same shape, which is what makes a row diff
    /// meaningful.
    pub fn same_size(&self, other: &Self) -> bool {
        self.rows == other.rows && self.cols == other.cols
    }

    /// The wire form of one row: runs of cells that share a style.
    pub fn row_runs(&self, row: u16) -> Vec<CellRun> {
        encode_runs(self.row(row))
    }

    /// A row as a string, for the accessibility tree and for tests.
    ///
    /// The accessibility tree is the reason this exists: a screen reader needs the
    /// line, not a thousand one-character labels. A wide cell contributes its glyph
    /// once and its trailer contributes nothing, so the text reads as the user sees
    /// it rather than with a hole after every emoji.
    pub fn row_text(&self, row: u16) -> String {
        let mut out = String::new();
        for col in 0..self.cols {
            match self.cell(row, col) {
                Some(cell) if cell.is_trailer() => {}
                Some(cell) if !cell.text.is_empty() => out.push_str(&cell.text),
                _ => out.push(' '),
            }
        }
        out.trim_end().to_string()
    }

    /// Every row's text, top to bottom.
    pub fn text(&self) -> String {
        (0..self.rows)
            .map(|row| self.row_text(row))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Whether there is history above this grid to scroll into.
    ///
    /// False in the alternate screen whatever the numbers say: a full-screen
    /// program owns its own viewport, and scrolling Turn's history out from under
    /// `lazygit` would show the user a screen that no longer exists.
    pub fn can_scroll_back(&self) -> bool {
        !self.alternate_screen && self.scrollback_offset < self.scrollback_len
    }

    /// The rows whose cells differ from `previous`, in order.
    ///
    /// Only meaningful between grids of the same size; a caller that has resized has
    /// to send the whole screen, and [`crate::ScreenUpdate::between`] is where that
    /// decision lives.
    pub fn changed_rows(&self, previous: &Self) -> Vec<u16> {
        if !self.same_size(previous) {
            return (0..self.rows).collect();
        }
        (0..self.rows)
            .filter(|row| self.row(*row) != previous.row(*row))
            .collect()
    }
}

/// Compact terminal history, oldest row first.
///
/// History uses the same validated cell runs as a [`Grid`], so colours, Unicode,
/// attributes and wide-cell trailers survive a UI restart without inventing a second
/// terminal representation.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Scrollback {
    cols: u16,
    rows: Vec<Vec<CellRun>>,
}

impl Scrollback {
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn cols(&self) -> u16 {
        self.cols
    }

    /// Expands the compact rows after the wire representation has been validated.
    pub fn decode_rows(&self) -> Result<Vec<Vec<Cell>>, GridError> {
        self.rows
            .iter()
            .enumerate()
            .map(|(row, runs)| decode_runs(runs, self.cols, row as u16))
            .collect()
    }

    #[cfg(feature = "vt100")]
    fn from_screen_with_limits(
        screen: &vt100::Screen,
        max_rows: usize,
        max_wire_bytes: usize,
    ) -> (Self, bool) {
        let (_, cols) = screen.size();
        if screen.alternate_screen() || max_rows == 0 || max_wire_bytes == 0 {
            return (Self::default(), screen.scrollback() > 0);
        }

        let mut source = screen.clone();
        source.set_scrollback(usize::MAX);
        let available = source.scrollback();
        let mut offset = available.min(max_rows);
        let mut truncated = available > offset;
        let mut rows: VecDeque<(Vec<CellRun>, usize)> = VecDeque::new();
        let mut encoded_bytes = 0usize;

        // At offset N the top min(N, screen-height) rows are the next oldest
        // retained history. Reading a viewport at a time keeps this O(history).
        while offset > 0 {
            source.set_scrollback(offset);
            let grid = from_screen(&source);
            let take = offset.min(grid.rows as usize);
            for row in 0..take {
                let runs = grid.row_runs(row as u16);
                let bytes = serde_json::to_vec(&runs).map_or(max_wire_bytes + 1, |row| row.len());
                if bytes > max_wire_bytes {
                    truncated = true;
                    continue;
                }
                while encoded_bytes.saturating_add(bytes) > max_wire_bytes {
                    let Some((_, removed)) = rows.pop_front() else {
                        break;
                    };
                    encoded_bytes = encoded_bytes.saturating_sub(removed);
                    truncated = true;
                }
                encoded_bytes = encoded_bytes.saturating_add(bytes);
                rows.push_back((runs, bytes));
            }
            offset -= take;
        }

        (
            Self {
                cols,
                rows: rows.into_iter().map(|(row, _)| row).collect(),
            },
            truncated,
        )
    }
}

#[derive(Deserialize)]
struct ScrollbackWire {
    cols: u16,
    rows: Vec<Vec<CellRun>>,
}

impl<'de> Deserialize<'de> for Scrollback {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = ScrollbackWire::deserialize(deserializer)?;
        if wire.cols == 0 && !wire.rows.is_empty() {
            return Err(D::Error::custom("scrollback has zero columns"));
        }
        if wire.rows.len() > MAX_SCROLLBACK_ROWS {
            return Err(D::Error::custom(format!(
                "scrollback has {} rows, over the limit of {}",
                wire.rows.len(),
                MAX_SCROLLBACK_ROWS
            )));
        }
        for (row, runs) in wire.rows.iter().enumerate() {
            decode_runs(runs, wire.cols, row as u16).map_err(D::Error::custom)?;
        }
        Ok(Self {
            cols: wire.cols,
            rows: wire.rows,
        })
    }
}

/// Extracts the newest bounded scrollback from a parsed terminal.
#[cfg(feature = "vt100")]
pub fn scrollback_from_screen(screen: &vt100::Screen) -> (Scrollback, bool) {
    Scrollback::from_screen_with_limits(screen, MAX_SCROLLBACK_ROWS, MAX_SCROLLBACK_WIRE_BYTES)
}

/// Why a grid or a row could not be read off the wire.
///
/// Every variant is a peer sending something structurally impossible rather than a
/// state Turn could recover from by guessing.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GridError {
    #[error("a grid of {rows}x{cols} is {cells} cells, over the limit of {max}")]
    TooLarge {
        rows: u16,
        cols: u16,
        cells: usize,
        max: usize,
    },
    #[error("the grid claims {expected} rows but carries {actual}")]
    RowCount { expected: u16, actual: usize },
    #[error("row {row} covers {actual} cells, not the grid's {expected}")]
    RowWidth {
        row: u16,
        expected: u16,
        actual: usize,
    },
    #[error("a run of {cells} cells carries {chars} characters, which cannot be split")]
    RunWidth { cells: u16, chars: usize },
    #[error("row {row} is outside a grid of {rows} rows")]
    RowOutOfRange { row: u16, rows: u16 },
    #[error("an update for {update_rows}x{update_cols} cannot apply to {rows}x{cols}")]
    SizeMismatch {
        rows: u16,
        cols: u16,
        update_rows: u16,
        update_cols: u16,
    },
}

/// One run of cells that share a colour and an attribute set.
///
/// Three shapes, distinguished without ambiguity when decoding:
///
/// * `t` absent — `n` blank cells carrying the run's style. A blank row is one of
///   these.
/// * `t` holding exactly `n` characters — one character per cell.
/// * `n == 1` with `t` holding more than one character — a single cell whose text is
///   a grapheme cluster: an emoji with a modifier, a combining accent.
///
/// Anything else is [`GridError::RunWidth`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CellRun {
    /// The run's text. Absent on the wire when the run is blank.
    #[serde(default, rename = "t", skip_serializing_if = "String::is_empty")]
    pub text: String,
    /// How many cells this run covers.
    #[serde(rename = "n")]
    pub cells: u16,
    #[serde(default, rename = "f", skip_serializing_if = "Option::is_none")]
    pub fg: Option<Rgb>,
    #[serde(default, rename = "b", skip_serializing_if = "Option::is_none")]
    pub bg: Option<Rgb>,
    #[serde(default, rename = "a", skip_serializing_if = "CellAttrs::is_plain")]
    pub attrs: CellAttrs,
}

impl CellRun {
    /// Expands the run into the cells it describes.
    pub fn expand(&self, into: &mut Vec<Cell>) -> Result<(), GridError> {
        let style = |text: String| Cell {
            text,
            fg: self.fg,
            bg: self.bg,
            attrs: self.attrs,
        };
        if self.text.is_empty() {
            for _ in 0..self.cells {
                into.push(style(String::new()));
            }
            return Ok(());
        }
        let chars = self.text.chars().count();
        if chars == self.cells as usize {
            for ch in self.text.chars() {
                into.push(style(ch.to_string()));
            }
            return Ok(());
        }
        if self.cells == 1 {
            // A grapheme cluster: several characters, one cell.
            into.push(style(self.text.clone()));
            return Ok(());
        }
        Err(GridError::RunWidth {
            cells: self.cells,
            chars,
        })
    }
}

/// Turns a row's cells into runs.
fn encode_runs(cells: &[Cell]) -> Vec<CellRun> {
    let mut runs: Vec<CellRun> = Vec::new();
    for cell in cells {
        if extends(runs.last(), cell) {
            if let Some(last) = runs.last_mut() {
                last.cells += 1;
                last.text.push_str(&cell.text);
            }
            continue;
        }
        runs.push(CellRun {
            text: cell.text.clone(),
            cells: 1,
            fg: cell.fg,
            bg: cell.bg,
            attrs: cell.attrs,
        });
    }
    runs
}

/// Whether `cell` can join the run being built.
///
/// The three shapes documented on [`CellRun`] have to stay decodable, which is what
/// every clause here protects: a cell holding a grapheme cluster can never share a
/// run, because a multi-character run cannot be split back into cells, and a blank
/// run and a text run cannot merge because "no text" and "a space" are different
/// cells.
fn extends(last: Option<&CellRun>, cell: &Cell) -> bool {
    let Some(last) = last else {
        return false;
    };
    if cell.text.chars().count() > 1 || last.cells == u16::MAX {
        return false;
    }
    if last.text.is_empty() != cell.text.is_empty() {
        return false;
    }
    // A run holding a cluster has one cell and more characters than cells; extending
    // it would produce something no decoder could split.
    if !last.text.is_empty() && last.text.chars().count() != last.cells as usize {
        return false;
    }
    cell.same_style(&Cell {
        text: String::new(),
        fg: last.fg,
        bg: last.bg,
        attrs: last.attrs,
    })
}

/// Expands a row's runs, checking they account for exactly `cols` cells.
pub fn decode_runs(runs: &[CellRun], cols: u16, row: u16) -> Result<Vec<Cell>, GridError> {
    let mut cells: Vec<Cell> = Vec::with_capacity(cols as usize);
    for run in runs {
        // Checked before expanding, so a peer claiming sixty thousand cells per run
        // cannot make the receiver allocate them.
        if cells.len() + run.cells as usize > cols as usize {
            return Err(GridError::RowWidth {
                row,
                expected: cols,
                actual: cells.len() + run.cells as usize,
            });
        }
        run.expand(&mut cells)?;
    }
    if cells.len() != cols as usize {
        return Err(GridError::RowWidth {
            row,
            expected: cols,
            actual: cells.len(),
        });
    }
    Ok(cells)
}

/// The grid as it is written and read. Kept separate from [`Grid`] so the in-memory
/// form stays a flat vector — the order a renderer walks it in — while the wire form
/// stays runs.
#[derive(Serialize, Deserialize)]
struct GridWire {
    rows: u16,
    cols: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cursor: Option<(u16, u16)>,
    #[serde(default, skip_serializing_if = "is_false")]
    alternate_screen: bool,
    #[serde(default, skip_serializing_if = "Modes::is_default")]
    modes: Modes,
    #[serde(default, skip_serializing_if = "is_zero")]
    scrollback_offset: usize,
    #[serde(default, skip_serializing_if = "is_zero")]
    scrollback_len: usize,
    /// One entry per row, top to bottom.
    runs: Vec<Vec<CellRun>>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

/// Refuses a geometry no real terminal has before anything is allocated for it.
fn check_size(rows: u16, cols: u16) -> Result<usize, GridError> {
    let cells = rows as usize * cols as usize;
    if cells > MAX_SCREEN_CELLS {
        return Err(GridError::TooLarge {
            rows,
            cols,
            cells,
            max: MAX_SCREEN_CELLS,
        });
    }
    Ok(cells)
}

impl Serialize for Grid {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let wire = GridWire {
            rows: self.rows,
            cols: self.cols,
            cursor: self.cursor,
            alternate_screen: self.alternate_screen,
            modes: self.modes,
            scrollback_offset: self.scrollback_offset,
            scrollback_len: self.scrollback_len,
            runs: (0..self.rows).map(|row| self.row_runs(row)).collect(),
        };
        wire.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Grid {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = GridWire::deserialize(deserializer)?;
        Grid::from_wire(wire).map_err(D::Error::custom)
    }
}

impl Grid {
    fn from_wire(wire: GridWire) -> Result<Self, GridError> {
        let expected = check_size(wire.rows, wire.cols)?;
        if wire.runs.len() != wire.rows as usize {
            return Err(GridError::RowCount {
                expected: wire.rows,
                actual: wire.runs.len(),
            });
        }
        let mut cells = Vec::with_capacity(expected);
        for (row, runs) in wire.runs.iter().enumerate() {
            cells.extend(decode_runs(runs, wire.cols, row as u16)?);
        }
        Ok(Self {
            rows: wire.rows,
            cols: wire.cols,
            cells,
            cursor: wire.cursor,
            alternate_screen: wire.alternate_screen,
            modes: wire.modes,
            scrollback_offset: wire.scrollback_offset,
            scrollback_len: wire.scrollback_len,
        })
    }
}

/// Resolves one of the 256 indexed terminal colours.
///
/// The daemon does this so the client never has to know which of the sixteen
/// conventions a program meant. The low sixteen use the widely-copied xterm values
/// rather than a theme's own, because a program that asked for "red" by index is
/// asking for the terminal's red, and a themed substitute would silently recolour
/// output the user is comparing against another terminal.
pub fn indexed_rgb(index: u8) -> Rgb {
    const BASE: [Rgb; 16] = [
        Rgb(0x00, 0x00, 0x00),
        Rgb(0xcd, 0x00, 0x00),
        Rgb(0x00, 0xcd, 0x00),
        Rgb(0xcd, 0xcd, 0x00),
        Rgb(0x00, 0x00, 0xee),
        Rgb(0xcd, 0x00, 0xcd),
        Rgb(0x00, 0xcd, 0xcd),
        Rgb(0xe5, 0xe5, 0xe5),
        Rgb(0x7f, 0x7f, 0x7f),
        Rgb(0xff, 0x00, 0x00),
        Rgb(0x00, 0xff, 0x00),
        Rgb(0xff, 0xff, 0x00),
        Rgb(0x5c, 0x5c, 0xff),
        Rgb(0xff, 0x00, 0xff),
        Rgb(0x00, 0xff, 0xff),
        Rgb(0xff, 0xff, 0xff),
    ];
    // The steps of the 6x6x6 cube are not linear: xterm's first step is a long one
    // so that "dark" stays dark.
    const STEPS: [u8; 6] = [0x00, 0x5f, 0x87, 0xaf, 0xd7, 0xff];

    match index {
        0..=15 => BASE[index as usize],
        16..=231 => {
            let offset = index as usize - 16;
            Rgb(
                STEPS[offset / 36],
                STEPS[(offset / 6) % 6],
                STEPS[offset % 6],
            )
        }
        232..=255 => {
            let level = (8 + (index as u16 - 232) * 10).min(255) as u8;
            Rgb(level, level, level)
        }
    }
}

#[cfg(feature = "vt100")]
fn resolve(colour: vt100::Color) -> Option<Rgb> {
    match colour {
        // Absent rather than resolved: "the default" is the theme's business, and
        // baking a concrete value in here would stop a themed background working.
        vt100::Color::Default => None,
        vt100::Color::Idx(index) => Some(indexed_rgb(index)),
        vt100::Color::Rgb(r, g, b) => Some(Rgb(r, g, b)),
    }
}

/// Turns a parsed screen into cells.
///
/// The single definition of that conversion, which is what makes the daemon's screen
/// and the client's agree by construction rather than by discipline. Palette indices
/// are resolved here, so no reader of a [`Grid`] ever has to guess which convention a
/// program meant.
///
/// Behind the `vt100` feature — on by default, since the daemon needs it — so a client
/// that only ever renders cells can build this crate without a terminal parser.
#[cfg(feature = "vt100")]
pub fn from_screen(screen: &vt100::Screen) -> Grid {
    let (rows, cols) = screen.size();
    let mut grid = Grid::blank(rows, cols);
    grid.alternate_screen = screen.alternate_screen();
    grid.modes = Modes {
        application_cursor: screen.application_cursor(),
        application_keypad: screen.application_keypad(),
        bracketed_paste: screen.bracketed_paste(),
        mouse: match screen.mouse_protocol_mode() {
            vt100::MouseProtocolMode::None => MouseMode::None,
            // Press and PressRelease differ in whether releases are reported, which
            // the encoder handles from the event itself; both mean "buttons only".
            vt100::MouseProtocolMode::Press | vt100::MouseProtocolMode::PressRelease => {
                MouseMode::Press
            }
            vt100::MouseProtocolMode::ButtonMotion => MouseMode::ButtonMotion,
            vt100::MouseProtocolMode::AnyMotion => MouseMode::AnyMotion,
        },
    };
    grid.cursor = if screen.hide_cursor() {
        None
    } else {
        Some(screen.cursor_position())
    };

    for row in 0..rows {
        for col in 0..cols {
            let Some(source) = screen.cell(row, col) else {
                continue;
            };
            let mut attrs = CellAttrs::default();
            if source.bold() {
                attrs = attrs.with(CellAttrs::BOLD);
            }
            if source.dim() {
                attrs = attrs.with(CellAttrs::DIM);
            }
            if source.italic() {
                attrs = attrs.with(CellAttrs::ITALIC);
            }
            if source.underline() {
                attrs = attrs.with(CellAttrs::UNDERLINE);
            }
            if source.is_wide() {
                attrs = attrs.with(CellAttrs::WIDE);
            }
            let continuation = source.is_wide_continuation();
            if continuation {
                attrs = attrs.with(CellAttrs::WIDE_TRAILER);
            }
            let mut fg = resolve(source.fgcolor());
            let mut bg = resolve(source.bgcolor());
            if source.inverse() {
                // Reversed here rather than left to the renderer, so there is one
                // reading of what the program asked for. When both colours are the
                // theme's own there is nothing to exchange, and the flag says so.
                if fg.is_none() && bg.is_none() {
                    attrs = attrs.with(CellAttrs::INVERSE);
                } else {
                    std::mem::swap(&mut fg, &mut bg);
                }
            }
            // A continuation cell repeats its partner's contents in `vt100`.
            // Carrying that through would paint the glyph twice, one column apart,
            // which is exactly the corruption wide cells are known for.
            let text = if continuation {
                String::new()
            } else {
                source.contents().to_string()
            };
            if let Some(target) = grid.cell_mut(row, col) {
                target.text = text;
                target.fg = fg;
                target.bg = bg;
                target.attrs = attrs;
            }
        }
    }
    grid
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blank_grid_is_the_size_it_was_asked_for() {
        let grid = Grid::blank(24, 80);
        assert_eq!(grid.cells.len(), 24 * 80);
        assert!(grid.cell(23, 79).is_some());
        assert!(grid.cell(24, 0).is_none(), "out of range must not wrap");
        assert!(grid.cell(0, 80).is_none());
    }

    #[test]
    fn scrollback_keeps_unicode_colours_and_the_newest_bounded_rows() {
        let mut parser = vt100::Parser::new(3, 16, 20);
        parser
            .process(b"\x1b[31mRED\x1b[0m\r\ncaf\xc3\xa9\r\nline-2\r\nline-3\r\nline-4\r\nline-5");
        let mut measured = parser.screen().clone();
        measured.set_scrollback(usize::MAX);
        let available = measured.scrollback();
        assert!(available >= 3, "fixture did not create scrollback");

        let (history, truncated) =
            Scrollback::from_screen_with_limits(parser.screen(), 3, 64 * 1024);
        assert_eq!(history.len(), 3);
        assert_eq!(truncated, available > 3);
        let rows = history.decode_rows().unwrap();
        let text: Vec<String> = rows
            .iter()
            .map(|row| row.iter().map(|cell| cell.text.as_str()).collect())
            .collect();
        assert_eq!(text, vec!["RED", "café", "line-2"]);
        assert!(rows[0]
            .iter()
            .filter(|cell| !cell.text.is_empty())
            .all(|cell| cell.fg == Some(indexed_rgb(1))));
    }

    #[test]
    fn scrollback_wire_budget_drops_oldest_rows_and_admits_truncation() {
        let mut parser = vt100::Parser::new(2, 20, 20);
        parser.process(b"oldest\r\nmiddle\r\nnewest\r\nlive");
        let one_row_budget = serde_json::to_vec(&Grid::from_lines(&["newest"], 20).row_runs(0))
            .unwrap()
            .len();
        let (history, truncated) =
            Scrollback::from_screen_with_limits(parser.screen(), 20, one_row_budget);
        assert!(truncated);
        assert_eq!(history.len(), 1);
        let rows = history.decode_rows().unwrap();
        let text: String = rows[0].iter().map(|cell| cell.text.as_str()).collect();
        assert_eq!(text, "middle");
    }

    #[test]
    fn an_oversized_row_does_not_discard_an_earlier_row_that_fits() {
        let mut parser = vt100::Parser::new(2, 20, 20);
        parser.process(
            b"plain\r\n\x1b[31ma\x1b[32mb\x1b[33mc\x1b[34md\x1b[35me\x1b[36mf\x1b[0m\r\nlive-1\r\nlive-2",
        );
        let plain_budget = serde_json::to_vec(&Grid::from_lines(&["plain"], 20).row_runs(0))
            .unwrap()
            .len();
        let (history, truncated) =
            Scrollback::from_screen_with_limits(parser.screen(), 20, plain_budget);

        assert!(truncated);
        let rows = history.decode_rows().unwrap();
        assert_eq!(rows.len(), 1);
        let text: String = rows[0].iter().map(|cell| cell.text.as_str()).collect();
        assert_eq!(text, "plain");
    }

    #[test]
    fn empty_scrollback_round_trips_when_serialized_explicitly() {
        let serialized = serde_json::to_string(&Scrollback::default()).unwrap();
        assert_eq!(
            serde_json::from_str::<Scrollback>(&serialized).unwrap(),
            Scrollback::default()
        );
    }

    #[test]
    fn a_zero_sized_grid_is_clamped_rather_than_empty() {
        let grid = Grid::blank(0, 0);
        assert_eq!((grid.rows, grid.cols), (1, 1));
        assert_eq!(grid.cells.len(), 1);
    }

    #[test]
    fn rows_read_back_as_text_for_the_accessibility_tree() {
        let grid = Grid::from_lines(&["hello", "  world  "], 20);
        assert_eq!(grid.row_text(0), "hello");
        assert_eq!(grid.row_text(1), "  world", "leading space is content");
        assert_eq!(grid.text(), "hello\n  world");
    }

    #[test]
    fn a_line_longer_than_the_grid_is_truncated_not_wrapped() {
        // Wrapping is the daemon's job; it has the terminal's own rules.
        let grid = Grid::from_lines(&["abcdefghij"], 4);
        assert_eq!(grid.row_text(0), "abcd");
    }

    #[test]
    fn a_cell_can_hold_a_grapheme_cluster_not_just_a_char() {
        let mut grid = Grid::blank(1, 4);
        // A family emoji is several chars in one cell.
        if let Some(cell) = grid.cell_mut(0, 0) {
            cell.text = "👩‍💻".to_string();
        }
        assert!(grid.row_text(0).starts_with("👩"));
        assert!(grid.cell(0, 0).is_some_and(|c| c.text.chars().count() > 1));
    }

    #[test]
    fn attributes_pack_and_unpack() {
        let attrs = CellAttrs::default()
            .with(CellAttrs::BOLD)
            .with(CellAttrs::UNDERLINE);
        assert!(attrs.has(CellAttrs::BOLD));
        assert!(attrs.has(CellAttrs::UNDERLINE));
        assert!(!attrs.has(CellAttrs::ITALIC));
        assert!(!attrs.is_plain());
        assert!(CellAttrs::default().is_plain());
    }

    #[test]
    fn a_grid_round_trips_through_json() {
        let mut grid = Grid::from_lines(&["ok"], 8);
        if let Some(cell) = grid.cell_mut(0, 0) {
            cell.fg = Some(Rgb::new(200, 40, 40));
            cell.attrs = CellAttrs::default().with(CellAttrs::BOLD);
        }
        grid.modes.application_cursor = true;
        grid.scrollback_len = 500;
        grid.scrollback_offset = 12;
        let json = serde_json::to_string(&grid).expect("a grid serialises");
        let back: Grid = serde_json::from_str(&json).expect("and deserialises");
        assert_eq!(back, grid);
    }

    #[test]
    fn a_blank_cell_is_recognised_so_a_redraw_can_skip_it() {
        assert!(Cell::blank().is_blank());
        assert!(Cell::plain("   ").is_blank());
        assert!(!Cell::plain("x").is_blank());
        // A cell with only a background still has to be painted.
        let mut tinted = Cell::blank();
        tinted.bg = Some(Rgb::new(0, 0, 40));
        assert!(!tinted.is_blank());
    }

    /// A wide glyph occupies two columns and the column after it is a trailer with
    /// no glyph, so nothing after it shifts.
    #[test]
    fn a_wide_cell_claims_two_columns_and_leaves_the_rest_of_the_row_in_place() {
        let mut grid = Grid::from_lines(&["ab  cd"], 6);
        assert!(grid.set_wide(0, 2, "漢"));
        assert_eq!(grid.row_text(0), "ab漢cd", "the row must not gain a column");
        assert_eq!(grid.cell(0, 2).map(Cell::columns), Some(2));
        assert!(grid.cell(0, 3).is_some_and(Cell::is_trailer));
        assert_eq!(
            grid.cell(0, 4).map(|c| c.text.as_str()),
            Some("c"),
            "the columns after a wide cell keep their own contents"
        );
    }

    #[test]
    fn a_wide_cell_that_would_not_fit_is_refused_rather_than_written_as_half() {
        let mut grid = Grid::blank(1, 4);
        assert!(
            !grid.set_wide(0, 3, "漢"),
            "there is no room for the trailer"
        );
        assert!(
            grid.cell(0, 3).is_some_and(Cell::is_blank),
            "nothing may be written when only half of it fits"
        );
    }

    #[test]
    fn the_indexed_palette_matches_the_xterm_cube_and_greys() {
        // The corners of the 6x6x6 cube, and both ends of the grey ramp.
        assert_eq!(indexed_rgb(16), Rgb(0x00, 0x00, 0x00));
        assert_eq!(indexed_rgb(231), Rgb(0xff, 0xff, 0xff));
        assert_eq!(indexed_rgb(196), Rgb(0xff, 0x00, 0x00));
        assert_eq!(indexed_rgb(46), Rgb(0x00, 0xff, 0x00));
        assert_eq!(indexed_rgb(232), Rgb(8, 8, 8));
        assert_eq!(indexed_rgb(255), Rgb(238, 238, 238));
        // And every index resolves rather than panicking on an edge.
        for index in 0..=255u8 {
            let _ = indexed_rgb(index);
        }
    }

    /// The encoding this module exists for. A blank row is one object, and a styled
    /// row is one object per style change — not one per cell.
    #[test]
    fn a_row_is_encoded_as_runs_of_one_style_rather_than_one_object_per_cell() {
        let mut grid = Grid::blank(1, 10);
        for (col, ch) in "hi".chars().enumerate() {
            if let Some(cell) = grid.cell_mut(0, col as u16) {
                cell.text = ch.to_string();
                cell.fg = Some(Rgb::new(0, 200, 0));
            }
        }
        let runs = grid.row_runs(0);
        assert_eq!(runs.len(), 2, "text run then blank run: {runs:?}");
        assert_eq!(runs[0].text, "hi");
        assert_eq!(runs[0].cells, 2);
        assert_eq!(runs[0].fg, Some(Rgb::new(0, 200, 0)));
        assert!(runs[1].text.is_empty());
        assert_eq!(runs[1].cells, 8);

        // And the blank row of an empty grid is a single run.
        let blank = Grid::blank(2, 120);
        assert_eq!(blank.row_runs(1).len(), 1);
        assert_eq!(blank.row_runs(1)[0].cells, 120);
    }

    /// The exact wire form, so a second implementation has something to write
    /// against and a change to it is a deliberate act.
    #[test]
    fn the_wire_form_of_a_small_grid_is_the_documented_shape() {
        let mut grid = Grid::blank(2, 4);
        grid.cursor = Some((1, 0));
        for (col, ch) in "ok".chars().enumerate() {
            if let Some(cell) = grid.cell_mut(0, col as u16) {
                cell.text = ch.to_string();
                cell.fg = Some(Rgb::new(200, 40, 40));
                cell.attrs = CellAttrs::default().with(CellAttrs::BOLD);
            }
        }
        let json = serde_json::to_string(&grid).expect("a grid serialises");
        assert_eq!(
            json,
            "{\"rows\":2,\"cols\":4,\"cursor\":[1,0],\
             \"runs\":[[{\"t\":\"ok\",\"n\":2,\"f\":[200,40,40],\"a\":1},{\"n\":2}],[{\"n\":4}]]}"
        );
        assert_eq!(
            serde_json::from_str::<Grid>(&json).expect("and reads back"),
            grid
        );
    }

    /// The measurement behind the encoding choice. These numbers are asserted so a
    /// change that makes the wire form dramatically bigger fails here rather than
    /// being noticed as a slow UI with thirty sessions open.
    #[test]
    fn a_full_screen_costs_about_a_kilobyte_rather_than_a_hundred() {
        // A realistic 40x120 pane: a build log, half the rows carrying text.
        let mut grid = Grid::blank(40, 120);
        for row in 0..20u16 {
            let line =
                format!("   Compiling turn-proto v0.1.0 (/Users/x/turn/crates/turn-proto) {row}");
            for (col, ch) in line.chars().enumerate().take(120) {
                if let Some(cell) = grid.cell_mut(row, col as u16) {
                    cell.text = ch.to_string();
                }
            }
        }
        let encoded = serde_json::to_string(&grid)
            .expect("a grid serialises")
            .len();
        assert!(
            (900..4_000).contains(&encoded),
            "a realistic screen costs {encoded} bytes; the run encoding has drifted"
        );

        // The same screen at one object per cell, for the comparison the module doc
        // claims. Measured rather than asserted from memory.
        let naive: usize = grid
            .cells
            .iter()
            .map(|cell| {
                serde_json::to_string(cell)
                    .expect("a cell serialises")
                    .len()
                    + 1
            })
            .sum();
        assert!(
            naive > encoded * 20,
            "runs must be a large win: {naive} against {encoded}"
        );

        // An empty screen, which is what thirty idle panes look like: one run per
        // row, so around a dozen bytes each.
        let blank = serde_json::to_string(&Grid::blank(40, 120))
            .expect("a grid serialises")
            .len();
        assert!(blank < 700, "a blank screen costs {blank} bytes");
    }

    #[test]
    fn a_grapheme_cluster_survives_the_run_encoding_as_its_own_cell() {
        let mut grid = Grid::blank(1, 5);
        for (col, text) in [(0u16, "a"), (1, "👩‍💻"), (2, "b")] {
            if let Some(cell) = grid.cell_mut(0, col) {
                cell.text = text.to_string();
            }
        }

        let json = serde_json::to_string(&grid).expect("a grid serialises");
        let back: Grid = serde_json::from_str(&json).expect("and reads back");
        assert_eq!(back, grid);
        assert_eq!(back.cell(0, 1).map(|c| c.text.as_str()), Some("👩‍💻"));
        // Three runs plus the trailing blanks: the cluster cannot share a run.
        assert_eq!(grid.row_runs(0).len(), 4, "{:?}", grid.row_runs(0));
    }

    #[test]
    fn a_run_that_does_not_account_for_its_cells_is_refused_rather_than_guessed_at() {
        // Three characters claiming two cells: unsplittable.
        let bad = CellRun {
            text: "abc".into(),
            cells: 2,
            fg: None,
            bg: None,
            attrs: CellAttrs::default(),
        };
        let mut out = Vec::new();
        assert_eq!(
            bad.expand(&mut out),
            Err(GridError::RunWidth { cells: 2, chars: 3 })
        );

        // A row that is not exactly as wide as the grid.
        let json = "{\"rows\":1,\"cols\":4,\"runs\":[[{\"t\":\"ab\",\"n\":2}]]}";
        let error = serde_json::from_str::<Grid>(json).expect_err("a short row is refused");
        assert!(error.to_string().contains("covers"), "got {error}");

        // A grid claiming more rows than it carries.
        let json = "{\"rows\":3,\"cols\":1,\"runs\":[[{\"n\":1}]]}";
        let error = serde_json::from_str::<Grid>(json).expect_err("a short grid is refused");
        assert!(error.to_string().contains("rows"), "got {error}");
    }

    /// A short line must not be able to ask the receiver for gigabytes.
    #[test]
    fn a_grid_larger_than_the_cell_limit_is_refused_before_it_is_allocated() {
        let json = "{\"rows\":65535,\"cols\":65535,\"runs\":[]}";
        let error = serde_json::from_str::<Grid>(json).expect_err("4 billion cells is refused");
        assert!(error.to_string().contains("limit"), "got {error}");

        // And a run claiming more cells than the row has is refused before the
        // expansion allocates them.
        let json = "{\"rows\":1,\"cols\":2,\"runs\":[[{\"n\":60000}]]}";
        assert!(serde_json::from_str::<Grid>(json).is_err());
    }

    #[test]
    fn rows_can_be_read_and_written_whole_for_applying_a_diff() {
        let mut grid = Grid::from_lines(&["one", "two"], 4);
        let replacement = decode_runs(
            &[CellRun {
                text: "abcd".into(),
                cells: 4,
                fg: None,
                bg: None,
                attrs: CellAttrs::default(),
            }],
            4,
            0,
        )
        .expect("the run expands");
        assert!(grid.set_row(1, &replacement));
        assert_eq!(grid.row_text(1), "abcd");
        assert_eq!(grid.row_text(0), "one", "the other row is untouched");

        // The wrong width, or a row that is not there, changes nothing.
        assert!(!grid.set_row(1, &replacement[..2]));
        assert!(!grid.set_row(9, &replacement));
        assert_eq!(grid.row_text(1), "abcd");
    }

    #[test]
    fn changed_rows_names_only_the_rows_that_moved() {
        let before = Grid::from_lines(&["one", "two", "three"], 8);
        let mut after = before.clone();
        if let Some(cell) = after.cell_mut(1, 0) {
            cell.text = "T".into();
        }
        assert_eq!(after.changed_rows(&before), vec![1]);
        assert!(before.changed_rows(&before).is_empty());

        // A different size has no row correspondence at all, so every row differs.
        let resized = Grid::blank(3, 9);
        assert_eq!(resized.changed_rows(&before), vec![0, 1, 2]);
    }

    /// The conversion the daemon and the client share, driven by a real escape
    /// stream rather than by a hand-built screen.
    #[cfg(feature = "vt100")]
    #[test]
    fn a_parsed_screen_becomes_the_grid_the_client_paints() {
        let mut parser = vt100::Parser::new(4, 20, 0);
        parser.process(b"plain\r\n\x1b[1;31mbold red\x1b[0m\r\n");
        let grid = from_screen(parser.screen());

        assert_eq!((grid.rows, grid.cols), (4, 20));
        assert_eq!(grid.row_text(0), "plain");
        assert_eq!(grid.row_text(1), "bold red");
        let cell = grid.cell(1, 0).expect("the first bold cell");
        assert!(cell.attrs.has(CellAttrs::BOLD));
        assert_eq!(
            cell.fg,
            Some(indexed_rgb(1)),
            "index 1 is the terminal's red"
        );
        assert_eq!(
            grid.cell(0, 0).and_then(|c| c.fg),
            None,
            "an unstyled cell must stay themeable rather than being pinned to a colour"
        );
        assert_eq!(grid.cursor, Some((2, 0)));
        assert!(!grid.alternate_screen);
    }

    /// The case a TUI depends on: the alternate screen, the application cursor mode
    /// that makes arrow keys work, and mouse reporting all reach the client.
    #[cfg(feature = "vt100")]
    #[test]
    fn a_full_screen_program_reports_its_alternate_screen_and_its_input_modes() {
        let mut parser = vt100::Parser::new(6, 20, 100);
        // Enter the alternate screen, application cursor keys, mouse tracking and
        // bracketed paste — what `lazygit` does on startup.
        parser.process(b"\x1b[?1049h\x1b[?1h\x1b[?1002h\x1b[?2004h");
        let grid = from_screen(parser.screen());

        assert!(grid.alternate_screen);
        assert!(
            grid.modes.application_cursor,
            "arrow keys must be encoded as the program asked, or they insert letters"
        );
        assert_eq!(grid.modes.mouse, MouseMode::ButtonMotion);
        assert!(grid.modes.bracketed_paste);
        assert!(
            !grid.can_scroll_back(),
            "a full-screen program owns its viewport; Turn must not scroll under it"
        );
    }

    #[cfg(feature = "vt100")]
    #[test]
    fn a_hidden_cursor_is_reported_as_absent_rather_than_as_a_position() {
        let mut parser = vt100::Parser::new(3, 10, 0);
        parser.process(b"\x1b[?25l");
        assert_eq!(from_screen(parser.screen()).cursor, None);
        parser.process(b"\x1b[?25h");
        assert!(from_screen(parser.screen()).cursor.is_some());
    }

    /// `vt100` repeats a wide glyph's contents in its continuation cell. Carrying
    /// that through would paint the glyph twice, one column apart — the classic
    /// emoji corruption.
    #[cfg(feature = "vt100")]
    #[test]
    fn a_wide_glyph_from_a_real_stream_is_not_painted_twice() {
        let mut parser = vt100::Parser::new(1, 10, 0);
        parser.process("漢字ok".as_bytes());
        let grid = from_screen(parser.screen());

        assert_eq!(grid.cell(0, 0).map(|c| c.text.as_str()), Some("漢"));
        assert!(grid.cell(0, 1).is_some_and(Cell::is_trailer));
        assert_eq!(
            grid.cell(0, 1).map(|c| c.text.as_str()),
            Some(""),
            "the trailer must carry no glyph of its own"
        );
        assert_eq!(grid.cell(0, 4).map(|c| c.text.as_str()), Some("o"));
        assert_eq!(grid.row_text(0), "漢字ok");
    }

    /// Reversed video is resolved where the colours are known, so a client never has
    /// to reproduce the rule — and never double-swaps.
    #[cfg(feature = "vt100")]
    #[test]
    fn reversed_video_is_swapped_when_building_the_grid_rather_than_left_to_the_client() {
        let mut parser = vt100::Parser::new(2, 12, 0);
        // Green on default, reversed: the green must end up as the background.
        parser.process(b"\x1b[32;7mA\x1b[0m");
        // Reversed with both colours left to the theme: nothing to swap.
        parser.process(b"\x1b[7mB\x1b[0m");
        let grid = from_screen(parser.screen());

        let swapped = grid.cell(0, 0).expect("the first cell");
        assert_eq!(swapped.bg, Some(indexed_rgb(2)), "green moved to the back");
        assert_eq!(swapped.fg, None, "and the foreground became the theme's");
        assert!(
            !swapped.attrs.has(CellAttrs::INVERSE),
            "a resolved swap must not be flagged, or the client would swap it again"
        );

        let unresolvable = grid.cell(0, 1).expect("the second cell");
        assert_eq!((unresolvable.fg, unresolvable.bg), (None, None));
        assert!(
            unresolvable.attrs.has(CellAttrs::INVERSE),
            "default on default is the one case only the theme's owner can reverse"
        );
    }
}
