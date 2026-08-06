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
//! ## Images are cells
//!
//! A pane may hold pictures, and they travel as cells too: each cell an image covers
//! carries an image marker as its text and the [`CellAttrs::IMAGE`] flag, and the small
//! table mapping those markers to payload ids is [`Grid::images`]. The pixels are not
//! here — they are fetched once per image and cached by the client. [`crate::images`]
//! explains why placement lives in the cells rather than in a side table of rectangles:
//! it is the only way scrolling, clearing and partial overwrites keep working without a
//! second implementation of a terminal.
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
//!
//! An OSC 8 URI ([`RowLink`]) is carried the same way — as the program wrote it, bounded
//! in length — because deciding what may be *opened* belongs at the point of opening and
//! nowhere else. A filter here would be a second, weaker gate that a reader could mistake
//! for the real one.
//!
//! ## What a row carries besides its cells
//!
//! Two things, and both exist so a client can find links without re-parsing the stream:
//!
//! * **Whether the row hard-wrapped** ([`RowMeta::wrapped`]). A terminal breaks a long
//!   line at the margin, so the boundary between two grid rows is often not a boundary in
//!   the text at all. Without this flag a client has to guess from "the last column is
//!   occupied", which is wrong for a program that printed exactly `cols` characters and
//!   then a newline — and being wrong there joins two unrelated lines into one bogus URL.
//! * **OSC 8 hyperlinks** ([`RowMeta::links`]), as spans over columns rather than as a
//!   property of each cell. A span carries its URI once instead of once per cell, which is
//!   the difference between a hundred bytes and four thousand for one link; and it is also
//!   the truer model, since OSC 8 declares a *region* of text and the region is what the
//!   user hovers.

use std::sync::LazyLock;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

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

/// Longest OSC 8 URI a grid will carry.
///
/// Well past what any browser accepts — the practical limits are around 2,000 — and short
/// enough that a program cannot make a screen expensive by declaring one enormous link per
/// row. A URI over the limit is refused when it is captured, so nothing is truncated: half
/// a URL is a different URL, and offering one would be worse than offering none.
pub const MAX_LINK_URI_CHARS: usize = 4_096;

/// Most OSC 8 hyperlink spans one grid may describe.
///
/// A screen where every other cell starts a new link is the worst case, and it is a case a
/// hostile program can produce deliberately. The cap is generous for real output — a
/// directory listing of clickable files is a few hundred — and bounds what one screen can
/// cost.
pub const MAX_SCREEN_LINKS: usize = 1_024;

/// One OSC 8 hyperlink, over a half-open range of columns in a single row.
///
/// A link that wraps across rows arrives as one of these per row it touches; joining them
/// back into one link is the client's job, because only the client knows which rows the
/// user is looking at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RowLink {
    /// First column, inclusive.
    pub from: u16,
    /// Last column, exclusive.
    pub to: u16,
    /// The URI exactly as the program declared it. Never acted on here: see the module
    /// documentation on where opening is decided.
    pub uri: String,
}

impl RowLink {
    pub fn new(from: u16, to: u16, uri: impl Into<String>) -> Self {
        Self {
            from,
            to,
            uri: uri.into(),
        }
    }

    /// Whether a column falls inside this span.
    pub fn covers(&self, col: u16) -> bool {
        col >= self.from && col < self.to
    }
}

/// What a row carries besides its cells.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RowMeta {
    /// True when the terminal broke this row at the margin, so the text continues on the
    /// row below without a newline having been printed.
    pub wrapped: bool,
    /// OSC 8 hyperlinks over this row's columns, in column order and never overlapping.
    ///
    /// The order and the absence of overlap are guaranteed on the way in, which is what
    /// makes "which link is under this column" a question with one answer.
    pub links: Vec<RowLink>,
}

impl RowMeta {
    /// Whether this row carries nothing at all, so the wire form can leave it out.
    pub fn is_default(&self) -> bool {
        !self.wrapped && self.links.is_empty()
    }

    /// The link covering a column, if any.
    pub fn link_at(&self, col: u16) -> Option<&RowLink> {
        self.links.iter().find(|link| link.covers(col))
    }
}

/// The metadata of a row that is not there.
///
/// A shared constant rather than a fresh allocation per miss, so a renderer walking a grid
/// can ask about a row past the end without paying for the answer.
static ABSENT_ROW: LazyLock<RowMeta> = LazyLock::new(RowMeta::default);

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
    /// This cell is one tile of an inline image, and its text is the image marker that
    /// says which tile of which image ([`crate::images::ImageCell`]).
    ///
    /// A flag as well as a recognisable character so a renderer can branch on the
    /// attributes it is already comparing, and so the run encoding splits image cells
    /// from text cells without being asked: two cells only share a run when their
    /// attributes match, which is exactly the property a picture needs.
    pub const IMAGE: u8 = 1 << 7;

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

    /// One tile of an inline image, ready to be written into a grid.
    ///
    /// Returns `None` for a tile outside the marker alphabet, so a caller cannot produce
    /// an image cell that names a tile no reader could decode.
    pub fn image(tile: crate::images::ImageCell) -> Option<Self> {
        Some(Self {
            text: tile.to_marker()?.to_string(),
            attrs: CellAttrs::default().with(CellAttrs::IMAGE),
            ..Self::blank()
        })
    }

    pub fn is_blank(&self) -> bool {
        // An image cell is never blank whatever its colours: it has pixels to paint, and
        // a renderer that skipped it would leave a hole in the picture.
        !self.is_image() && self.text.trim().is_empty() && self.bg.is_none()
    }

    /// Whether this cell is a tile of an inline image rather than a character.
    pub fn is_image(&self) -> bool {
        self.attrs.has(CellAttrs::IMAGE)
    }

    /// Which tile of which image this cell shows, if it shows one.
    ///
    /// Both the flag and a well-formed marker are required. A cell carrying one without
    /// the other arrived from something that does not agree with this module about what
    /// an image cell is, and drawing it as a picture would be guessing.
    pub fn image_tile(&self) -> Option<crate::images::ImageCell> {
        if !self.is_image() {
            return None;
        }
        crate::images::marker_of(&self.text)
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
    /// One entry per row, top to bottom.
    ///
    /// Private, unlike [`Self::cells`], because its length is an invariant rather than a
    /// convenience: a grid whose metadata is shorter than its rows would answer "is this
    /// row wrapped" with a panic. Everything that reads or writes it goes through
    /// [`Grid::row_meta`] and [`Grid::set_row_meta`], which cannot break that.
    meta: Vec<RowMeta>,
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
    /// The inline images this screen's marker cells refer to, at most
    /// [`crate::images::MAX_PLACED_IMAGES`] of them.
    ///
    /// A table of *metadata*, keyed by the slot the markers carry: id, cell box and
    /// intrinsic pixel size. The pixels are deliberately absent — a grid crosses the
    /// socket many times a second and a megabyte of picture must not — and are fetched
    /// once per id with [`crate::Request::PaneImage`].
    pub images: Vec<crate::images::GridImage>,
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
            meta: vec![RowMeta::default(); rows as usize],
            cursor: Some((0, 0)),
            alternate_screen: false,
            modes: Modes::default(),
            scrollback_offset: 0,
            scrollback_len: 0,
            images: Vec::new(),
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

    /// What a row carries besides its cells.
    ///
    /// A row that is not there reads as carrying nothing, rather than as an absence the
    /// caller has to handle: "is row 400 wrapped" has an obvious answer and making every
    /// caller unwrap it would be noise.
    pub fn row_meta(&self, row: u16) -> &RowMeta {
        self.meta.get(row as usize).unwrap_or(&ABSENT_ROW)
    }

    /// Whether the terminal broke this row at the margin, so the text continues below.
    pub fn row_wrapped(&self, row: u16) -> bool {
        self.row_meta(row).wrapped
    }

    /// The OSC 8 hyperlinks declared over this row's columns.
    pub fn row_links(&self, row: u16) -> &[RowLink] {
        &self.row_meta(row).links
    }

    /// The OSC 8 hyperlink under a cell, if the program declared one there.
    pub fn link_at(&self, row: u16, col: u16) -> Option<&RowLink> {
        self.row_meta(row).link_at(col)
    }

    /// Replaces what a row carries besides its cells.
    ///
    /// Returns false for a row that is not there. Sorts the links and drops any that
    /// overlap an earlier one or fall outside the grid, so the invariant
    /// [`RowMeta::links`] documents holds however the metadata was assembled — a caller
    /// building a grid by hand cannot leave two links fighting over one column.
    pub fn set_row_meta(&mut self, row: u16, meta: RowMeta) -> bool {
        let cols = self.cols;
        let Some(slot) = self.meta.get_mut(row as usize) else {
            return false;
        };
        let RowMeta { wrapped, mut links } = meta;
        links.retain(|link| link.from < link.to && link.to <= cols);
        links.sort_by_key(|link| link.from);
        let mut kept: Vec<RowLink> = Vec::with_capacity(links.len());
        for link in links {
            if kept.last().is_some_and(|last| link.from < last.to) {
                continue;
            }
            kept.push(link);
        }
        *slot = RowMeta {
            wrapped,
            links: kept,
        };
        true
    }

    /// Marks a row as having wrapped into the next, leaving its links alone.
    pub fn set_row_wrapped(&mut self, row: u16, wrapped: bool) -> bool {
        match self.meta.get_mut(row as usize) {
            Some(slot) => {
                slot.wrapped = wrapped;
                true
            }
            None => false,
        }
    }

    /// Replaces one row's cells. `false` when the row is out of range or the wrong
    /// width, so a caller cannot half-write a row and leave the grid ragged.
    ///
    /// Leaves the row's metadata alone: a caller replacing cells from a row diff sets the
    /// metadata from the same update, and a caller writing cells in a test has no reason
    /// to lose the row's links by doing so.
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
    /// A tile of an image contributes a space, so the words on either side of a picture
    /// do not run together and the columns of the rest of the line still line up. The
    /// marker character itself must never escape into text a human or a screen reader
    /// reads: it is Turn's internal bookkeeping and it has no glyph.
    pub fn row_text(&self, row: u16) -> String {
        let mut out = String::new();
        for col in 0..self.cols {
            match self.cell(row, col) {
                Some(cell) if cell.is_trailer() => {}
                Some(cell) if cell.is_image() => out.push(' '),
                Some(cell) if !cell.text.is_empty() => out.push_str(&cell.text),
                _ => out.push(' '),
            }
        }
        out.trim_end().to_string()
    }

    /// The image a cell belongs to, and which tile of it the cell shows.
    ///
    /// `None` when the cell is not an image tile, and also when it names a slot this
    /// screen's table does not fill — which is what a client sees when a marker has
    /// outlived its placement, and is why a renderer must handle it rather than index.
    pub fn image_at(
        &self,
        row: u16,
        col: u16,
    ) -> Option<(&crate::images::GridImage, crate::images::ImageCell)> {
        let tile = self.cell(row, col)?.image_tile()?;
        let placed = self.images.iter().find(|image| image.slot == tile.slot)?;
        Some((placed, tile))
    }

    /// The placement in a slot, if the screen has one.
    pub fn image_in_slot(&self, slot: u8) -> Option<&crate::images::GridImage> {
        self.images.iter().find(|image| image.slot == slot)
    }

    /// Attaches an inline-image table to a grid built from a screen.
    ///
    /// Separate from [`from_screen_with_images`] so a grid produced any other way — a window
    /// of scrollback, above all — can be given its pictures by the same rule.
    ///
    /// The table is **filtered to the slots that are actually on this grid**, so a placement
    /// whose markers have scrolled away or been overwritten is not described: asking a client
    /// to fetch a megabyte of pixels it has nowhere to draw would be worse than saying
    /// nothing. Duplicated slots, slots outside the budget and placements no screen could hold
    /// are dropped rather than repaired, for the same reason the wire decoder refuses them.
    pub fn attach_images(&mut self, images: &[crate::images::GridImage]) {
        let mut occupied = [false; crate::images::MAX_PLACED_IMAGES];
        for cell in self.cells.iter() {
            if let Some(tile) = cell.image_tile() {
                if let Some(slot) = occupied.get_mut(tile.slot as usize) {
                    *slot = true;
                }
            }
        }
        let mut taken = [false; crate::images::MAX_PLACED_IMAGES];
        let mut out = Vec::new();
        for image in images {
            let slot = image.slot as usize;
            if slot >= crate::images::MAX_PLACED_IMAGES || !occupied[slot] || taken[slot] {
                continue;
            }
            if !image.is_valid() {
                continue;
            }
            taken[slot] = true;
            out.push(*image);
        }
        self.images = out;
    }

    /// Whether any cell of this screen is part of a picture.
    ///
    /// Cheap and honest: it asks the cells rather than the table, because a table entry
    /// whose markers have all been overwritten describes an image that is no longer on
    /// screen.
    pub fn has_images(&self) -> bool {
        self.cells.iter().any(Cell::is_image)
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
            .filter(|row| self.row_differs(previous, *row))
            .collect()
    }

    /// Whether a row differs from the same row of another grid, metadata included.
    ///
    /// The metadata counts: a link appearing over text that did not change is still a
    /// change the client has to be told about, and comparing only cells would leave the
    /// user with output that has quietly stopped being clickable.
    fn row_differs(&self, other: &Self, row: u16) -> bool {
        self.row(row) != other.row(row) || self.row_meta(row) != other.row_meta(row)
    }

    /// Every OSC 8 hyperlink on the grid, row by row.
    pub fn links(&self) -> impl Iterator<Item = (u16, &RowLink)> {
        self.meta
            .iter()
            .enumerate()
            .flat_map(|(row, meta)| meta.links.iter().map(move |link| (row as u16, link)))
    }
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
    #[error("a link on row {row} covers columns {from}..{to}, outside a row of {cols}")]
    LinkRange {
        row: u16,
        from: u16,
        to: u16,
        cols: u16,
    },
    #[error("two links on row {row} both cover column {col}")]
    LinkOverlap { row: u16, col: u16 },
    #[error("a link URI of {chars} characters is over the limit of {max}")]
    LinkUriLength { chars: usize, max: usize },
    #[error("{count} links on one screen is over the limit of {max}")]
    TooManyLinks { count: usize, max: usize },
    #[error("an update for {update_rows}x{update_cols} cannot apply to {rows}x{cols}")]
    SizeMismatch {
        rows: u16,
        cols: u16,
        update_rows: u16,
        update_cols: u16,
    },
    /// The screen's inline-image table is not one a screen could hold.
    ///
    /// Wrapped rather than restated so there is one description of what a valid image is,
    /// in [`crate::images`], where the bounds live.
    #[error("the screen's images are not usable: {0}")]
    Image(#[source] crate::images::ImageError),
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
    /// The rows the terminal broke at the margin, by index.
    ///
    /// A sparse list rather than one boolean per row: on a normal screen no row is
    /// wrapped, and forty `false`s per frame is forty bytes of nothing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    wrapped: Vec<u16>,
    /// Every OSC 8 hyperlink on the screen.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    links: Vec<GridLink>,
    /// The inline images this screen's marker cells refer to, by slot.
    ///
    /// Absent on the overwhelming majority of screens, which have no pictures on them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    images: Vec<crate::images::GridImage>,
    /// One entry per row, top to bottom.
    runs: Vec<Vec<CellRun>>,
}

/// One OSC 8 hyperlink and the row it is on, as a grid carries it on the wire.
///
/// Spelled out rather than abbreviated to single letters like [`CellRun`]'s fields: there
/// are thousands of runs on a screen and a handful of links, so the trade that makes runs
/// terse does not apply, and a frame in a bug report should be readable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct GridLink {
    row: u16,
    from: u16,
    to: u16,
    uri: String,
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
            wrapped: (0..self.rows)
                .filter(|row| self.row_wrapped(*row))
                .collect(),
            links: self
                .links()
                .map(|(row, link)| GridLink {
                    row,
                    from: link.from,
                    to: link.to,
                    uri: link.uri.clone(),
                })
                .collect(),
            images: self.images.clone(),
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
        let mut meta = vec![RowMeta::default(); wire.rows as usize];
        for row in wire.wrapped {
            match meta.get_mut(row as usize) {
                Some(slot) => slot.wrapped = true,
                None => {
                    return Err(GridError::RowOutOfRange {
                        row,
                        rows: wire.rows,
                    })
                }
            }
        }
        decode_links(wire.links, &mut meta, wire.rows, wire.cols)?;
        // Checked before a renderer can index by slot, and for the same reason a row's
        // width is: an impossible placement is a peer disagreeing about the format, not a
        // state to repair by clamping something into range.
        crate::images::check_table(&wire.images).map_err(GridError::Image)?;
        Ok(Self {
            rows: wire.rows,
            cols: wire.cols,
            cells,
            meta,
            cursor: wire.cursor,
            alternate_screen: wire.alternate_screen,
            modes: wire.modes,
            scrollback_offset: wire.scrollback_offset,
            scrollback_len: wire.scrollback_len,
            images: wire.images,
        })
    }
}

/// Places a screen's links on their rows, refusing anything a client could not resolve.
///
/// Every clause is a peer sending something structurally impossible rather than a state
/// worth repairing: a span outside the row, a span overlapping one already placed, a URI
/// longer than [`MAX_LINK_URI_CHARS`], or more links than [`MAX_SCREEN_LINKS`]. The
/// overlap check is what makes "which link is under this column" a question with exactly
/// one answer, which is the property the hover depends on.
fn decode_links(
    links: Vec<GridLink>,
    meta: &mut [RowMeta],
    rows: u16,
    cols: u16,
) -> Result<(), GridError> {
    if links.len() > MAX_SCREEN_LINKS {
        return Err(GridError::TooManyLinks {
            count: links.len(),
            max: MAX_SCREEN_LINKS,
        });
    }
    for link in links {
        let chars = link.uri.chars().count();
        if chars > MAX_LINK_URI_CHARS {
            return Err(GridError::LinkUriLength {
                chars,
                max: MAX_LINK_URI_CHARS,
            });
        }
        if link.from >= link.to || link.to > cols {
            return Err(GridError::LinkRange {
                row: link.row,
                from: link.from,
                to: link.to,
                cols,
            });
        }
        let Some(slot) = meta.get_mut(link.row as usize) else {
            return Err(GridError::RowOutOfRange {
                row: link.row,
                rows,
            });
        };
        if let Some(clash) = slot
            .links
            .iter()
            .find(|placed| link.from < placed.to && placed.from < link.to)
        {
            return Err(GridError::LinkOverlap {
                row: link.row,
                col: link.from.max(clash.from),
            });
        }
        slot.links.push(RowLink::new(link.from, link.to, link.uri));
    }
    for row in meta.iter_mut() {
        row.links.sort_by_key(|link| link.from);
    }
    Ok(())
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

/// One OSC 8 hyperlink as the terminal buffer that captured it can describe it:
/// `(row, from, to, uri)`, with `to` exclusive.
///
/// A tuple, and deliberately so. `vt100` does not implement OSC 8, so the capture lives
/// with the other side-channel escapes in `turn_pty`'s callbacks — and that crate is
/// documented as knowing nothing about the protocol's grid types. A borrowed tuple is the
/// narrowest thing the two sides can agree on without one of them depending on the other.
#[cfg(feature = "vt100")]
pub type ScreenLink<'a> = (u16, u16, u16, &'a str);

/// Turns a parsed screen into cells.
///
/// The single definition of that conversion, which is what makes the daemon's screen
/// and the client's agree by construction rather than by discipline. Palette indices
/// are resolved here, so no reader of a [`Grid`] ever has to guess which convention a
/// program meant.
///
/// Behind the `vt100` feature — on by default, since the daemon needs it — so a client
/// that only ever renders cells can build this crate without a terminal parser.
///
/// Carries no OSC 8 hyperlinks: [`from_screen_with_links`] is the entry point for a caller
/// that captured them, and this one exists for the callers — previews, tests — that have a
/// screen and nothing else.
#[cfg(feature = "vt100")]
pub fn from_screen(screen: &vt100::Screen) -> Grid {
    from_screen_with_links(screen, std::iter::empty())
}

/// Turns a parsed screen into cells, with the hyperlinks the buffer captured beside it.
///
/// A span outside the screen, or one overlapping a span already placed, is dropped rather
/// than allowed to make the grid ambiguous — the same rule the wire decoder enforces, for
/// the same reason. Nothing is truncated to fit: half a link is a link over the wrong text.
#[cfg(feature = "vt100")]
pub fn from_screen_with_links<'a>(
    screen: &vt100::Screen,
    links: impl IntoIterator<Item = ScreenLink<'a>>,
) -> Grid {
    let (rows, cols) = screen.size();
    let mut grid = Grid::blank(rows, cols);
    grid.alternate_screen = screen.alternate_screen();
    for row in 0..rows {
        grid.set_row_wrapped(row, screen.row_wrapped(row));
    }
    let mut placed = 0usize;
    for (row, from, to, uri) in links {
        if placed >= MAX_SCREEN_LINKS || row >= rows || from >= to || to > cols {
            continue;
        }
        if uri.is_empty() || uri.chars().count() > MAX_LINK_URI_CHARS {
            continue;
        }
        let mut meta = grid.row_meta(row).clone();
        if meta
            .links
            .iter()
            .any(|link| from < link.to && link.from < to)
        {
            continue;
        }
        meta.links.push(RowLink::new(from, to, uri));
        if grid.set_row_meta(row, meta) {
            placed += 1;
        }
    }
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
    flag_image_cells(&mut grid);
    grid
}

/// Marks the cells whose text is an inline-image marker.
///
/// A pass over the finished cells rather than a branch inside the loop above, because the
/// question is only about the text and asking it once per cell at the end keeps the
/// colour-and-attribute reading in one piece.
///
/// A program can print a private-use character itself and have it flagged here. That is
/// harmless by construction: a marker naming a slot the screen has no placement for is
/// drawn as a missing image, and a marker naming one it does have can only show a tile of
/// a picture that program printed in the first place.
#[cfg(feature = "vt100")]
fn flag_image_cells(grid: &mut Grid) {
    for cell in grid.cells.iter_mut() {
        if crate::images::marker_of(&cell.text).is_some() {
            cell.attrs = cell.attrs.with(CellAttrs::IMAGE);
        }
    }
}

/// Turns a parsed screen into cells, with its hyperlinks and its inline images.
///
/// The entry point the daemon uses, and the only one that produces a grid a client can
/// draw pictures from: the marker cells are already in the parsed screen — the terminal
/// parser has been moving them around with the text — and this adds the table that says
/// which payload each marker's slot refers to.
///
/// The table is **filtered to the slots that are actually on screen**. An image whose
/// markers have all scrolled away or been overwritten is no longer placed, so describing
/// it would ask a client to fetch a megabyte of pixels it has nowhere to draw.
#[cfg(feature = "vt100")]
pub fn from_screen_with_images<'a>(
    screen: &vt100::Screen,
    links: impl IntoIterator<Item = ScreenLink<'a>>,
    images: &[crate::images::GridImage],
) -> Grid {
    let mut grid = from_screen_with_links(screen, links);
    grid.attach_images(images);
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

    /// An OSC 8 hyperlink is a span, and the span has to survive the wire intact: the
    /// user hovers a region of text, not a cell.
    #[test]
    fn a_hyperlink_span_round_trips_and_answers_which_link_is_under_a_column() {
        let mut grid = Grid::from_lines(&["see the PR now", "and this one"], 20);
        assert!(grid.set_row_meta(
            0,
            RowMeta {
                wrapped: false,
                links: vec![RowLink::new(4, 10, "https://example.com/pull/42")],
            }
        ));
        assert!(grid.set_row_wrapped(1, true));

        assert_eq!(
            grid.link_at(0, 4).map(|link| link.uri.as_str()),
            Some("https://example.com/pull/42")
        );
        assert_eq!(grid.link_at(0, 9).map(|link| link.from), Some(4));
        assert_eq!(grid.link_at(0, 10), None, "`to` is exclusive");
        assert_eq!(grid.link_at(0, 3), None);
        assert_eq!(grid.link_at(1, 4), None, "a link belongs to its own row");
        assert!(grid.row_wrapped(1) && !grid.row_wrapped(0));
        assert!(
            !grid.row_wrapped(9),
            "a row that is not there carries nothing rather than panicking"
        );

        let json = serde_json::to_string(&grid).expect("a grid serialises");
        assert_eq!(
            serde_json::from_str::<Grid>(&json).expect("and reads back"),
            grid
        );
        assert_eq!(grid.links().count(), 1);
    }

    /// A grid assembled by hand must not be able to hold two links over one column: the
    /// hover asks "which link is here" and there has to be one answer.
    #[test]
    fn overlapping_or_impossible_spans_are_dropped_when_a_grid_is_assembled_by_hand() {
        let mut grid = Grid::blank(1, 10);
        assert!(grid.set_row_meta(
            0,
            RowMeta {
                wrapped: false,
                links: vec![
                    RowLink::new(5, 9, "https://second.example"),
                    RowLink::new(0, 6, "https://first.example"),
                    RowLink::new(3, 3, "https://empty.example"),
                    RowLink::new(8, 40, "https://past-the-margin.example"),
                ],
            }
        ));
        let links = grid.row_links(0);
        assert_eq!(links.len(), 1, "got {links:?}");
        assert_eq!(links[0].uri, "https://first.example");
        assert_eq!((links[0].from, links[0].to), (0, 6));
    }

    /// A link appearing over text that did not change is still a change: comparing only
    /// cells would leave a client rendering output that has stopped being clickable.
    #[test]
    fn a_row_whose_only_change_is_a_link_is_reported_as_changed() {
        let before = Grid::from_lines(&["run make check", "ok"], 20);
        let mut after = before.clone();
        assert!(after.set_row_meta(
            1,
            RowMeta {
                wrapped: false,
                links: vec![RowLink::new(0, 2, "https://ci.example/build/7")],
            }
        ));
        assert_eq!(after.changed_rows(&before), vec![1]);

        let mut wrapped = before.clone();
        assert!(wrapped.set_row_wrapped(0, true));
        assert_eq!(wrapped.changed_rows(&before), vec![0]);
    }

    /// A screen with no links must cost exactly what it cost before they existed.
    #[test]
    fn the_wire_form_of_a_grid_without_links_is_unchanged() {
        let json = serde_json::to_string(&Grid::blank(2, 4)).expect("a grid serialises");
        assert!(!json.contains("wrapped"), "got {json}");
        assert!(!json.contains("links"), "got {json}");

        let mut linked = Grid::from_lines(&["ok"], 4);
        assert!(linked.set_row_meta(
            0,
            RowMeta {
                wrapped: true,
                links: vec![RowLink::new(0, 2, "ssh://build.example")],
            }
        ));
        let json = serde_json::to_string(&linked).expect("a grid serialises");
        assert!(
            json.contains("\"wrapped\":[0]")
                && json.contains(
                    "\"links\":[{\"row\":0,\"from\":0,\"to\":2,\"uri\":\"ssh://build.example\"}]"
                ),
            "got {json}"
        );
    }

    /// A short line must not be able to make one screen expensive, and a span that could
    /// not be resolved must be refused rather than repaired.
    #[test]
    fn a_hostile_link_table_is_refused_on_the_way_in() {
        let over_the_cap = (0..(MAX_SCREEN_LINKS + 1))
            .map(|i| format!("{{\"row\":0,\"from\":0,\"to\":1,\"uri\":\"https://a{i}.example\"}}"))
            .collect::<Vec<_>>()
            .join(",");
        let json = format!(
            "{{\"rows\":1,\"cols\":4,\"links\":[{over_the_cap}],\"runs\":[[{{\"n\":4}}]]}}"
        );
        let error = serde_json::from_str::<Grid>(&json).expect_err("a thousand links is refused");
        assert!(error.to_string().contains("over the limit"), "got {error}");

        for (json, expected) in [
            (
                "{\"rows\":1,\"cols\":4,\"links\":[{\"row\":0,\"from\":0,\"to\":9,\"uri\":\"https://a.example\"}],\"runs\":[[{\"n\":4}]]}",
                "outside a row",
            ),
            (
                "{\"rows\":1,\"cols\":4,\"links\":[{\"row\":7,\"from\":0,\"to\":2,\"uri\":\"https://a.example\"}],\"runs\":[[{\"n\":4}]]}",
                "outside a grid",
            ),
            (
                "{\"rows\":1,\"cols\":4,\"links\":[{\"row\":0,\"from\":0,\"to\":3,\"uri\":\"https://a.example\"},{\"row\":0,\"from\":2,\"to\":4,\"uri\":\"https://b.example\"}],\"runs\":[[{\"n\":4}]]}",
                "both cover column",
            ),
            (
                "{\"rows\":1,\"cols\":4,\"wrapped\":[6],\"runs\":[[{\"n\":4}]]}",
                "outside a grid",
            ),
        ] {
            let error = serde_json::from_str::<Grid>(json).expect_err("malformed input is refused");
            assert!(
                error.to_string().contains(expected),
                "expected {expected:?}, got {error}"
            );
        }
    }

    /// The wrap flag is the difference between finding one URL and inventing one: a
    /// terminal breaks a long line at the margin, and the break is not in the text.
    #[cfg(feature = "vt100")]
    #[test]
    fn a_line_the_terminal_broke_at_the_margin_says_so_on_the_row_it_broke() {
        let mut parser = vt100::Parser::new(4, 10, 0);
        // Eighteen characters into ten columns: row 0 wrapped, row 1 did not.
        parser.process(b"abcdefghijklmnopqr");
        let grid = from_screen(parser.screen());
        assert!(grid.row_wrapped(0), "row 0 ran off the margin");
        assert!(!grid.row_wrapped(1));

        // A program that printed exactly ten characters and then a newline did not wrap,
        // which is the case a "last column is occupied" guess gets wrong.
        let mut parser = vt100::Parser::new(4, 10, 0);
        parser.process(b"0123456789\r\nnext");
        let grid = from_screen(parser.screen());
        assert!(
            !grid.row_wrapped(0),
            "a full row followed by a newline is not a wrap"
        );
    }

    /// `vt100` does not implement OSC 8, so the capture happens in `turn_pty` and arrives
    /// here as spans. This is the seam between the two.
    #[cfg(feature = "vt100")]
    #[test]
    fn captured_hyperlink_spans_are_placed_on_the_screen_they_were_captured_from() {
        let mut parser = vt100::Parser::new(3, 12, 0);
        parser.process(b"the PR here");
        let grid = from_screen_with_links(
            parser.screen(),
            [
                (0u16, 4u16, 6u16, "https://example.com/pull/1"),
                // Overlapping the one already placed: dropped.
                (0, 5, 8, "https://evil.example"),
                // Off the screen entirely: dropped.
                (9, 0, 2, "https://nowhere.example"),
                // Past the right margin: dropped.
                (1, 10, 40, "https://wide.example"),
                // Empty URI: dropped, because a link to nothing is worse than no link.
                (2, 0, 2, ""),
            ],
        );
        assert_eq!(grid.links().count(), 1);
        assert_eq!(
            grid.link_at(0, 4).map(|link| link.uri.as_str()),
            Some("https://example.com/pull/1")
        );
        assert_eq!(grid.row_text(0), "the PR here");
        assert!(
            from_screen(parser.screen()).links().count() == 0,
            "a screen converted without links carries none"
        );
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

    // -------------------------------------------------------------------- inline images

    use crate::images::{GridImage, ImageCell, ImageId, MAX_PLACED_IMAGES};

    /// A grid with one image occupying `rows` by `cols` cells at `(top, left)`.
    fn with_image(
        grid: &mut Grid,
        slot: u8,
        top: u16,
        left: u16,
        rows: u16,
        cols: u16,
    ) -> GridImage {
        for dy in 0..rows {
            for dx in 0..cols {
                let tile = ImageCell::new(slot, dy, dx);
                let cell = Cell::image(tile).expect("an addressable tile");
                if let Some(target) = grid.cell_mut(top + dy, left + dx) {
                    *target = cell;
                }
            }
        }
        let placed = GridImage::new(slot, ImageId(0xfeed), rows, cols, 64, 32);
        grid.images.push(placed);
        placed
    }

    #[test]
    fn an_image_cell_carries_its_tile_and_is_never_treated_as_blank() {
        let cell = Cell::image(ImageCell::new(3, 5, 7)).expect("an addressable tile");
        assert!(cell.is_image());
        assert_eq!(cell.image_tile(), Some(ImageCell::new(3, 5, 7)));
        assert!(
            !cell.is_blank(),
            "a renderer that skipped image cells would leave holes in the picture"
        );
        assert_eq!(cell.columns(), 1);
        assert!(!cell.is_trailer());

        // A tile no marker can name produces no cell at all.
        assert!(Cell::image(ImageCell::new(MAX_PLACED_IMAGES as u8, 0, 0)).is_none());

        // And a plain cell is not an image however odd its text.
        assert_eq!(Cell::plain("x").image_tile(), None);
        // The flag without a marker is a disagreement about the format, not a picture.
        let forged = Cell {
            attrs: CellAttrs::default().with(CellAttrs::IMAGE),
            ..Cell::plain("x")
        };
        assert_eq!(forged.image_tile(), None);
    }

    /// The marker must never reach anything a human reads. It has no glyph and it is
    /// Turn's own bookkeeping.
    #[test]
    fn the_text_of_a_row_holding_a_picture_shows_spaces_rather_than_markers() {
        let mut grid = Grid::from_lines(&["a    b", "      "], 6);
        with_image(&mut grid, 0, 0, 1, 2, 4);

        assert_eq!(grid.row_text(0), "a    b");
        assert_eq!(
            grid.row_text(1),
            "",
            "a row of nothing but picture is a row of nothing to read"
        );
        assert!(
            !grid.text().chars().any(crate::images::is_marker),
            "a marker leaked into text: {:?}",
            grid.text()
        );
    }

    #[test]
    fn a_cell_resolves_to_the_placement_its_slot_names_and_to_nothing_when_there_is_none() {
        let mut grid = Grid::blank(4, 8);
        let placed = with_image(&mut grid, 2, 1, 1, 2, 3);

        let (image, tile) = grid.image_at(2, 3).expect("a tile of the picture");
        assert_eq!(*image, placed);
        assert_eq!(tile, ImageCell::new(2, 1, 2));
        assert_eq!(grid.image_in_slot(2), Some(&placed));
        assert!(grid.has_images());

        assert_eq!(grid.image_at(0, 0), None, "an ordinary cell is not a tile");
        // A marker whose slot the table does not fill: the case a client meets when a
        // placement has been forgotten, and it must be an absence rather than an index.
        grid.images.clear();
        assert_eq!(grid.image_at(2, 3), None);
        assert!(
            grid.has_images(),
            "the cells still carry markers even with no table"
        );
    }

    #[test]
    fn a_grid_with_a_picture_round_trips_through_json_with_its_table() {
        let mut grid = Grid::blank(3, 10);
        with_image(&mut grid, 1, 0, 2, 2, 4);
        let json = serde_json::to_string(&grid).expect("a grid serialises");
        assert!(json.contains("\"images\""), "got {json}");
        let back: Grid = serde_json::from_str(&json).expect("and reads back");
        assert_eq!(back, grid);
        assert_eq!(
            back.image_at(1, 4).map(|(_, tile)| tile),
            Some(ImageCell::new(1, 1, 2))
        );

        // A screen with no pictures says nothing about images at all.
        let plain = serde_json::to_string(&Grid::blank(2, 4)).expect("a grid serialises");
        assert!(!plain.contains("images"), "got {plain}");
    }

    /// A run of tiles is one run, because they share their attributes. That is what keeps
    /// a full-width picture from costing a hundred and twenty objects a frame.
    #[test]
    fn a_row_of_tiles_encodes_as_one_run_rather_than_one_object_per_cell() {
        let mut grid = Grid::blank(1, 40);
        with_image(&mut grid, 0, 0, 0, 1, 40);
        let runs = grid.row_runs(0);
        assert_eq!(runs.len(), 1, "got {runs:?}");
        assert_eq!(runs[0].cells, 40);
        assert!(runs[0].attrs.has(CellAttrs::IMAGE));
        assert_eq!(runs[0].text.chars().count(), 40);

        // And it expands back into forty distinct tiles, in order.
        let cells = decode_runs(&runs, 40, 0).expect("the run expands");
        for (dx, cell) in cells.iter().enumerate() {
            assert_eq!(cell.image_tile(), Some(ImageCell::new(0, 0, dx as u16)));
        }
    }

    /// The table is what a client indexes by slot, so an impossible one is refused rather
    /// than clamped into range.
    #[test]
    fn a_screen_claiming_an_impossible_image_is_refused_off_the_wire() {
        let json = "{\"rows\":1,\"cols\":1,\"images\":[{\"slot\":99,\"id\":1,\"rows\":1,\
                    \"cols\":1,\"width\":1,\"height\":1}],\"runs\":[[{\"n\":1}]]}";
        let error = serde_json::from_str::<Grid>(json).expect_err("slot 99 does not exist");
        assert!(error.to_string().contains("slot"), "got {error}");

        // A decompression bomb declared in the table rather than in a payload.
        let json = "{\"rows\":1,\"cols\":1,\"images\":[{\"slot\":0,\"id\":1,\"rows\":1,\
                    \"cols\":1,\"width\":60000,\"height\":60000}],\"runs\":[[{\"n\":1}]]}";
        let error = serde_json::from_str::<Grid>(json).expect_err("3.6 gigapixels is refused");
        assert!(error.to_string().contains("limit"), "got {error}");

        // Two placements in one slot would make "which picture is this" ambiguous.
        let json = "{\"rows\":1,\"cols\":1,\"images\":[{\"slot\":0,\"id\":1,\"rows\":1,\
                    \"cols\":1,\"width\":2,\"height\":2},{\"slot\":0,\"id\":2,\"rows\":1,\
                    \"cols\":1,\"width\":2,\"height\":2}],\"runs\":[[{\"n\":1}]]}";
        assert!(serde_json::from_str::<Grid>(json).is_err());
    }

    /// The markers are in the parsed screen, so the parser has already been moving them
    /// with the text. This is the proof that reading them back out works.
    #[cfg(feature = "vt100")]
    #[test]
    fn a_picture_written_into_a_real_terminal_comes_back_as_image_cells() {
        let mut parser = vt100::Parser::new(4, 12, 0);
        let mut painted = String::from("ab");
        for dx in 0..3u16 {
            painted.push(
                ImageCell::new(0, 0, dx)
                    .to_marker()
                    .expect("an addressable tile"),
            );
        }
        painted.push_str("cd");
        parser.process(painted.as_bytes());

        let table = [GridImage::new(0, ImageId(7), 1, 3, 30, 10)];
        let grid = from_screen_with_images(parser.screen(), std::iter::empty(), &table);

        assert_eq!(
            grid.row_text(0),
            "ab   cd",
            "the picture reads as its width"
        );
        for dx in 0..3u16 {
            let cell = grid.cell(0, 2 + dx).expect("a tile");
            assert!(cell.is_image(), "column {} is not a tile", 2 + dx);
            assert_eq!(cell.image_tile(), Some(ImageCell::new(0, 0, dx)));
            assert_eq!(
                cell.columns(),
                1,
                "a marker must be one column wide, or it would shift the rest of the row"
            );
        }
        assert_eq!(grid.cell(0, 5).map(|c| c.text.as_str()), Some("c"));
        assert_eq!(grid.images, table.to_vec());
    }

    /// Clearing the screen has to drop the picture, and it does so without this module
    /// knowing anything about `clear`: the markers were cells, and the cells are gone.
    #[cfg(feature = "vt100")]
    #[test]
    fn clearing_the_screen_drops_the_picture_and_its_table_entry() {
        let mut parser = vt100::Parser::new(4, 12, 0);
        let marker = ImageCell::new(0, 0, 0)
            .to_marker()
            .expect("an addressable tile");
        parser.process(marker.to_string().as_bytes());
        let table = [GridImage::new(0, ImageId(7), 1, 1, 8, 16)];
        assert!(from_screen_with_images(parser.screen(), std::iter::empty(), &table).has_images());

        parser.process(b"\x1b[2J");
        let cleared = from_screen_with_images(parser.screen(), std::iter::empty(), &table);
        assert!(!cleared.has_images());
        assert!(
            cleared.images.is_empty(),
            "a table entry for a picture nobody can see would ask a client to fetch it"
        );
    }

    /// Scrolling has to move the picture, for the same reason: the parser moves rows.
    #[cfg(feature = "vt100")]
    #[test]
    fn scrolling_moves_a_picture_up_a_row_at_a_time_and_then_off_the_screen() {
        let mut parser = vt100::Parser::new(3, 8, 0);
        let marker = ImageCell::new(1, 0, 0)
            .to_marker()
            .expect("an addressable tile");
        parser.process(marker.to_string().as_bytes());
        let table = [GridImage::new(1, ImageId(7), 1, 1, 8, 16)];

        let at = |parser: &vt100::Parser| -> Option<(u16, u16)> {
            let grid = from_screen_with_images(parser.screen(), std::iter::empty(), &table);
            (0..grid.rows)
                .flat_map(|row| (0..grid.cols).map(move |col| (row, col)))
                .find(|(row, col)| grid.cell(*row, *col).is_some_and(Cell::is_image))
        };
        assert_eq!(at(&parser), Some((0, 0)));

        // Three newlines at the bottom of a three-row screen scroll it out of existence.
        parser.process(b"\r\n\r\n");
        assert_eq!(at(&parser), Some((0, 0)), "still on the top row");
        parser.process(b"\r\n");
        assert_eq!(at(&parser), None, "the picture scrolled off the top");
        assert!(
            from_screen_with_images(parser.screen(), std::iter::empty(), &table)
                .images
                .is_empty()
        );
    }

    /// A program printing over half a picture punches a hole in it, and the surviving
    /// tiles still say which part of the image they are.
    #[cfg(feature = "vt100")]
    #[test]
    fn text_printed_over_a_picture_erases_exactly_the_cells_it_covers() {
        let mut parser = vt100::Parser::new(2, 8, 0);
        let mut painted = String::new();
        for dx in 0..6u16 {
            painted.push(
                ImageCell::new(0, 0, dx)
                    .to_marker()
                    .expect("an addressable tile"),
            );
        }
        parser.process(painted.as_bytes());
        // Back to column three and print two characters over the middle.
        parser.process(b"\x1b[1;4Hxy");

        let table = [GridImage::new(0, ImageId(7), 1, 6, 60, 10)];
        let grid = from_screen_with_images(parser.screen(), std::iter::empty(), &table);
        let tiles: Vec<Option<u16>> = (0..6)
            .map(|col| grid.cell(0, col).and_then(Cell::image_tile).map(|t| t.dx))
            .collect();
        assert_eq!(
            tiles,
            vec![Some(0), Some(1), Some(2), None, None, Some(5)],
            "the surviving tiles must still name their own columns"
        );
        assert_eq!(grid.row_text(0), "   xy");
    }

    #[test]
    fn a_table_entry_for_a_slot_with_no_cells_is_dropped_rather_than_sent() {
        let mut grid = Grid::blank(2, 4);
        with_image(&mut grid, 0, 0, 0, 1, 2);
        let table = vec![
            GridImage::new(0, ImageId(1), 1, 2, 16, 16),
            // Slot 5 has no markers anywhere on this screen.
            GridImage::new(5, ImageId(2), 1, 2, 16, 16),
        ];
        grid.attach_images(&table);
        assert_eq!(grid.images.len(), 1);
        assert_eq!(grid.images[0].slot, 0);
    }
}
