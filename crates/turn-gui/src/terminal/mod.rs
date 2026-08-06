//! The terminal pane: where users live.
//!
//! ## Backgrounds by runs, glyphs by cells
//!
//! A row's *colour* is painted as **runs** — consecutive cells sharing a colour and an
//! attribute set — and the run encoding already exists: `Grid::row_runs` is the same
//! function the protocol uses to put the row on the wire. A prompt line is three
//! rectangles instead of a hundred and twenty.
//!
//! Its *glyphs* are painted one cell at a time, each at its own place on the pixel grid.
//! Drawing a run as a single string is cheaper and it is what Turn used to do, but it
//! hands the column positions to the font's advances: the text then drifts away from the
//! grid the borders are drawn on, and a box-drawing frame comes out as loose doubled
//! pipes — which is exactly the report this module was rewritten for. A blank cell costs
//! nothing at all, and a terminal screen is mostly blank, so the price of being right is
//! a text shape per visible character.
//!
//! Two passes per row, backgrounds before glyphs, because a wide glyph reaches into the
//! column after it and the background of that column must not be painted over its right
//! half.
//!
//! ## Only what is on screen
//!
//! A pane clipped by the window paints only the rows inside the clip rectangle.
//! [`visible_rows`] works that out, and it is a pure function so the arithmetic is
//! tested rather than eyeballed.
//!
//! ## The accessibility tree
//!
//! A GPU-drawn terminal has no DOM, so if nothing is put in the AccessKit tree the pane
//! does not exist for a screen reader. The pane therefore registers itself with
//! `Role::Terminal` and its screen as the node's value — the lines, not four thousand
//! one-character labels.

pub mod boxdraw;
pub mod feed;
pub mod geometry;
pub mod images;
pub mod keys;
pub mod links;
pub mod menu;
pub mod mouse;
pub mod search;
pub mod selection;

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;

use egui::{Align2, Color32, CursorIcon, FontId, Pos2, Rect, Response, Sense, Stroke, Ui, Vec2};
use turn_core::model::Direction;
use turn_proto::cells::{CellAttrs, CellRun, Grid, Rgb};
use turn_proto::PtySize;

use crate::panes::size_in_cells;
use crate::repaint::cursor_visible;
use crate::theme::Theme;
use geometry::CellGrid;
use images::ImageCache;
use links::{FsPaths, Link, LinkMap, LinkRequest};
use menu::{PaneCommand, PaneContext, PaneMenu, PaneShortcuts};
use selection::{advance, autoscroll_rows, cell_at, CellPos, Granularity, Motion};
use selection::{Selection, SelectionKind};

/// What a pane wants done as a result of the user's input.
///
/// Returned rather than performed, so the pane draws and the application decides. That
/// is also what keeps the product rules out of the draw code: a pane cannot move focus
/// or approve anything, it can only report that a key was pressed.
#[derive(Debug, Clone, PartialEq)]
pub enum PaneAction {
    /// Bytes typed into the pane's pty. Pending permission prompts can be answered
    /// only through this direct human-input action.
    Write(Vec<u8>),
    /// The pane is a different size in cells and the pty must be told.
    Resize(PtySize),
    /// The user clicked in this pane.
    Focus,
    /// Text to put on the clipboard.
    Copy(String),
    /// The viewport should move by this many rows, positive being backwards into
    /// history.
    Scroll(i32),
}

/// What a pane wants the *window* to do, which it cannot do itself.
///
/// Kept apart from [`PaneAction`] because these are not things that happen to a pane: they
/// change the layout, reach the platform, or open a panel the pane knows nothing about. A
/// pane reports them and decides none of them — same rule as [`PaneAction`], one level up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneRequest {
    /// Discard the history Turn kept for this pane — `feed::PaneFeed::clear_history`.
    ///
    /// Turn's record only. The *screen* belongs to the program in the pane, and the only
    /// way to clear that would be to write bytes into the pty, which is typing at whatever
    /// is running.
    ClearHistory,
    /// Open the window's search for this text — `search::PaneSearch::open_with`.
    Search(String),
    /// Follow a link: `links::open`, after whatever confirmation the request asks for.
    ///
    /// A [`links::LinkRequest`] rather than a bare string, so a link whose visible text
    /// names a different host than its target arrives with that warning attached. The pane
    /// finds the link; the window decides whether to ask first.
    FollowLink(links::LinkRequest),
    /// Split this pane.
    Split(Direction),
    /// Close this pane.
    Close,
}

/// Everything one frame of a pane produced.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PaneOutcome {
    pub actions: Vec<PaneAction>,
    pub requests: Vec<PaneRequest>,
}

/// How a pane is being shown this frame.
#[derive(Debug, Clone, Copy)]
pub struct PaneOptions {
    /// Whether this pane is visually focused. Only a focused pane blinks its cursor,
    /// and normally only a focused pane receives keystrokes. This is deliberately visual
    /// state rather than the final input gate: an onboarding sheet can sit over a pane
    /// that remains focused in the persisted Layout.
    pub focused: bool,
    /// Whether keyboard events may be encoded for the PTY in this frame.
    ///
    /// Kept separate from [`Self::focused`] so a modal sheet can hold the window's
    /// keyboard lease without lying about which pane will be focused when it closes.
    /// Callers must set this to false whenever a sensitive overlay is open.
    pub accepts_input: bool,
    /// Wall-clock milliseconds, for the cursor phase.
    pub now_ms: i64,
    /// Whether the pane is showing history rather than the live screen, which suppresses
    /// the cursor and shows a marker instead.
    pub scrolled: bool,
    /// Whether Turn's record of this pane is complete, so the marker can say where the
    /// record begins rather than implying it goes back for ever.
    pub history_complete: bool,
}

impl Default for PaneOptions {
    fn default() -> Self {
        Self {
            focused: false,
            accepts_input: false,
            now_ms: 0,
            scrolled: false,
            history_complete: true,
        }
    }
}

/// What the pane remembers between frames.
#[derive(Debug, Clone, Default)]
pub struct PaneInteraction {
    pub selection: Option<Selection>,
    /// This pane's search: the query, the matches, and which one the user is on.
    ///
    /// Kept with the pane rather than with the window because the highlights are painted
    /// here and the bar is drawn here, so the window's whole part in it is to carry the
    /// query to the daemon and the answer back. See [`search::PaneSearch`].
    pub search: search::PaneSearch,
    /// Where the keyboard's own cell cursor is, while keyboard selection is on.
    ///
    /// `None` is the normal state, in which every keystroke belongs to the program. The
    /// mode exists because in a terminal the arrow keys are not free: there is no spare
    /// gesture for "move a selection cursor", so it has to be a mode the user turns on.
    keyboard: Option<CellPos>,
    /// Set for one frame when the user asked for the menu with the keyboard.
    menu_at: Option<CellPos>,
    /// Where a keyboard-opened menu is anchored, for as long as it is on screen.
    ///
    /// Held across frames rather than derived each time, because `egui`'s context menu
    /// otherwise anchors itself to the pointer — and a menu opened with Shift+F10 would jump
    /// to wherever the mouse happened to be resting on the frame after it appeared.
    menu_anchor: Option<CellPos>,
    /// The size in cells last reported, so a resize request is sent on a change rather
    /// than every frame.
    reported_size: Option<PtySize>,
    /// The links on the grid as it was when they were last found — see [`links`].
    ///
    /// Built only while the pointer is over the pane, and reused across frames until the
    /// grid's text changes. Finding links means resolving paths against the filesystem, so
    /// doing it per frame would be a `stat` storm for a pane somebody is merely pointing at.
    link_map: LinkMap,
    /// A fingerprint of the grid the map was built from, so it is rebuilt when the text
    /// changes and not when the cursor blinked.
    link_map_for: Option<u64>,
    /// Which link the pointer is over, as an index into [`Self::link_map`], and since when.
    ///
    /// The time is what stops a tooltip appearing the instant the pointer crosses a URL and
    /// covering the text somebody is reading.
    hovered_link: Option<(usize, i64)>,
    /// Set when the user follows a link, for the window to take and act on.
    ///
    /// State rather than a [`PaneRequest`] because a link that disagrees with its own text
    /// needs a confirmation the pane cannot show, and because the window — not the pane —
    /// owns everything that leaves Turn.
    pending_link: Option<LinkRequest>,
    /// Where this pane's relative paths resolve from, and what has been resolved already.
    paths: FsPaths,
    /// The inline images this pane has uploaded, and the ids it still needs.
    ///
    /// Kept here rather than passed in for the reason this type exists — it is what the pane
    /// remembers between frames — and because a GPU texture has to outlive the frame that
    /// uploaded it. See [`images::ImageCache`].
    pub images: ImageCache,
}

impl PaneInteraction {
    /// The selected text, if any.
    ///
    /// `None` rather than an empty string for a selection covering nothing: a stray
    /// click must not silently wipe whatever the user had on the clipboard.
    pub fn selected_text(&self, grid: &Grid) -> Option<String> {
        let selection = self.selection.as_ref()?;
        let text = selection.text(grid);
        if text.is_empty() {
            None
        } else {
            Some(text)
        }
    }

    pub fn clear_selection(&mut self) {
        self.selection = None;
    }

    /// The size last reported to the pty, for a caller that wants to avoid asking twice.
    pub fn reported_size(&self) -> Option<PtySize> {
        self.reported_size
    }

    /// Tells the pane where the process behind it is running.
    ///
    /// Without it, a relative path in a compiler error is not offered as a link at all: a
    /// path is only a link when it resolves, and `src/main.rs` cannot be resolved without
    /// knowing which repository it is relative to. Absolute paths still work. Cheap to call
    /// every frame — it only does anything when the directory has actually changed.
    pub fn set_cwd(&mut self, cwd: Option<PathBuf>) {
        if self.paths.cwd() != cwd.as_deref() {
            self.paths.set_cwd(cwd);
            // The map was built against the old directory, so its paths may point elsewhere.
            self.link_map_for = None;
        }
    }

    /// The links found on the grid the pointer was last over.
    pub fn links(&self) -> &LinkMap {
        &self.link_map
    }

    /// The link the pointer is over, if any.
    pub fn hovered_link(&self) -> Option<&Link> {
        let (index, _) = self.hovered_link?;
        self.link_map.links().get(index)
    }

    /// Notes that the pointer is over a cell, and answers with when it arrived on the link
    /// there.
    ///
    /// `None` when there is no link under the cell. The arrival time is kept from the frame
    /// the pointer reached *this* link, so moving along one link does not keep restarting the
    /// wait before its target is shown, and crossing to another one does.
    ///
    /// Separate from the drawing so the part with a decision in it can be tested without a
    /// window: this is where "which link is the user pointing at" is answered.
    pub fn hover_link(&mut self, grid: &Grid, at: CellPos, now_ms: i64) -> Option<i64> {
        self.refresh_links(grid, now_ms);
        let found = self
            .link_map
            .links()
            .iter()
            .position(|link| link.covers(at.row, at.col));
        let Some(index) = found else {
            self.hovered_link = None;
            return None;
        };
        let since = match self.hovered_link {
            Some((held, since)) if held == index => since,
            _ => now_ms,
        };
        self.hovered_link = Some((index, since));
        Some(since)
    }

    /// Rebuilds the link map when the grid's text has changed since it was built.
    ///
    /// The fingerprint is over the characters on screen, not the whole grid: a blinking
    /// cursor, a colour change and a scroll position must not cost a rebuild, and a rebuild
    /// resolves paths against the filesystem.
    fn refresh_links(&mut self, grid: &Grid, now_ms: i64) {
        let fingerprint = text_fingerprint(grid);
        if self.link_map_for == Some(fingerprint) {
            return;
        }
        self.paths.begin_scan(now_ms);
        self.link_map = LinkMap::find(grid, &mut self.paths);
        self.link_map_for = Some(fingerprint);
        self.hovered_link = None;
    }

    /// Takes the link the user asked to follow, if they asked on this frame.
    ///
    /// The window calls this after drawing the pane. A request that
    /// [`LinkRequest::needs_confirmation`] must be confirmed before
    /// [`links::open`] is called with it; anything else can be opened straight away.
    pub fn take_link_request(&mut self) -> Option<LinkRequest> {
        self.pending_link.take()
    }

    /// A double-click: the word under the pointer, in the terminal's sense of "word".
    pub fn select_word(&mut self, grid: &Grid, at: CellPos, kind: SelectionKind) {
        self.selection = Some(Selection::word(grid, at, kind));
    }

    /// A triple-click: the logical line, which for a hard-wrapped line is all of its rows.
    pub fn select_line(&mut self, grid: &Grid, at: CellPos) {
        self.selection = Some(Selection::line(grid, at));
    }

    /// Everything on the screen that has anything on it.
    pub fn select_all(&mut self, grid: &Grid) {
        self.selection = Some(Selection::all(grid));
    }

    /// A press that starts a drag.
    ///
    /// Three cases, and getting the third wrong is what makes double-click-drag feel
    /// broken: Shift extends what is already selected; a press inside a word or line
    /// selection continues that gesture at its own granularity; anything else starts a new
    /// character selection.
    pub fn begin_drag(&mut self, grid: &Grid, at: CellPos, kind: SelectionKind, shift: bool) {
        if shift {
            if let Some(selection) = &mut self.selection {
                selection.extend_in_grid(grid, at);
                return;
            }
        }
        let continues = self.selection.is_some_and(|selection| {
            selection.granularity != Granularity::Character && selection.contains(at.row, at.col)
        });
        if continues {
            return;
        }
        self.selection = Some(Selection::new(at, kind));
    }

    /// The pointer moved while held.
    pub fn drag_to(&mut self, grid: &Grid, at: CellPos) {
        if let Some(selection) = &mut self.selection {
            selection.extend_in_grid(grid, at);
        }
    }

    /// A plain click. Shift extends the selection that is there; anything else clears it.
    ///
    /// Clearing rather than starting an empty selection, so the next copy does not silently
    /// produce nothing.
    pub fn click(&mut self, grid: &Grid, at: CellPos, shift: bool) {
        match (shift, &mut self.selection) {
            (true, Some(selection)) => selection.extend_in_grid(grid, at),
            _ => self.clear_selection(),
        }
    }

    /// Where the keyboard's cell cursor is, or `None` when selection mode is off.
    pub fn keyboard_cursor(&self) -> Option<CellPos> {
        self.keyboard
    }

    /// Turns keyboard selection on, starting where the program's cursor is.
    ///
    /// Starting at the cursor rather than at the top-left puts the user next to the text
    /// they were just looking at, which is nearly always the text they want.
    pub fn enter_selection_mode(&mut self, grid: &Grid) {
        let at = grid
            .cursor
            .map(|(row, col)| CellPos::new(row, col))
            .unwrap_or(CellPos::new(0, 0));
        self.keyboard = Some(CellPos::new(
            at.row.min(grid.rows.saturating_sub(1)),
            at.col.min(grid.cols.saturating_sub(1)),
        ));
    }

    /// Turns it off, and drops the selection with it.
    pub fn leave_selection_mode(&mut self) {
        self.keyboard = None;
        self.clear_selection();
    }

    /// Moves the keyboard cursor, extending the selection when Shift is held.
    ///
    /// Returns false when selection mode is off, so a caller can tell a keystroke it
    /// handled from one it must pass to the program.
    pub fn move_cursor(&mut self, grid: &Grid, motion: Motion, extend: bool) -> bool {
        let Some(from) = self.keyboard else {
            return false;
        };
        let to = advance(grid, from, motion);
        self.keyboard = Some(to);
        if !extend {
            self.clear_selection();
            return true;
        }
        match &mut self.selection {
            Some(selection) => selection.extend_in_grid(grid, to),
            None => {
                let mut selection = Selection::new(from, SelectionKind::Linear);
                selection.extend_to(to);
                self.selection = Some(selection);
            }
        }
        true
    }

    /// Asks for the menu at the keyboard cursor, for the next frame to open.
    fn request_menu(&mut self) {
        self.menu_at = Some(self.keyboard.unwrap_or(CellPos::new(0, 0)));
    }
}

/// How tall the "you are looking at history" bar is.
pub const SCROLL_MARKER_HEIGHT: f32 = 18.0;

/// Where row zero of the grid is drawn.
///
/// A scrolled pane gives the marker its own strip rather than drawing it over the top
/// row: covering the first line of output is exactly the wrong thing to do to somebody
/// who scrolled back in order to read it. Shared by the painting and the pointer
/// arithmetic, because a selection that disagreed with the paint by eighteen points
/// would put the highlight on the wrong line.
pub fn grid_origin(rect: Rect, options: PaneOptions) -> Pos2 {
    if options.scrolled {
        rect.min + Vec2::new(0.0, SCROLL_MARKER_HEIGHT)
    } else {
        rect.min
    }
}

/// The rows of `grid` that fall inside `clip`, given where row zero is drawn.
///
/// Inclusive of the partially visible rows at each edge: a row half off the bottom of
/// the window still has to be drawn, or the pane appears to end early.
pub fn visible_rows(grid: &Grid, origin: Pos2, cell: Vec2, clip: Rect) -> std::ops::Range<u16> {
    if cell.y <= 0.0 || grid.rows == 0 || clip.max.y <= clip.min.y {
        return 0..0;
    }
    let first = ((clip.min.y - origin.y) / cell.y).floor().max(0.0);
    let last = ((clip.max.y - origin.y) / cell.y).ceil().max(0.0);
    let first = (first as u32).min(grid.rows as u32) as u16;
    let last = (last as u32).min(grid.rows as u32) as u16;
    first..last.max(first)
}

/// The colours a run is painted in, with reversal, dimming, selection and search applied.
///
/// The order is the precedence: the current match wins over a selection, a selection wins
/// over the other matches, and both win over whatever the program asked for. A search that
/// could not show you which hit you were on would leave "next" looking like it did nothing,
/// which is why the current match is the one thing that overrides even the selection.
fn colours(
    run: &CellRun,
    theme: &Theme,
    selected: bool,
    mark: Option<search::Mark>,
) -> (Color32, Option<Color32>) {
    let mut fg = run.fg.map(to_colour).unwrap_or(theme.text);
    let mut bg = run.bg.map(to_colour);

    // The protocol resolves reversed video when it can, and sets this flag only for the
    // one case it cannot: both colours were the theme's own. A client that swapped a
    // cell without the flag would reverse it twice and end up invisible.
    if run.attrs.has(CellAttrs::INVERSE) {
        bg = Some(fg);
        fg = theme.background;
    }
    if run.attrs.has(CellAttrs::DIM) {
        fg = fg.gamma_multiply(0.6);
    }
    if mark == Some(search::Mark::Other) {
        bg = Some(search::match_background(theme));
    }
    if selected {
        bg = Some(theme.selection);
    }
    if mark == Some(search::Mark::Current) {
        let (text, behind) = search::current_match_colours(theme);
        fg = text;
        bg = Some(behind);
    }
    (fg, bg)
}

fn to_colour(rgb: Rgb) -> Color32 {
    Color32::from_rgb(rgb.0, rgb.1, rgb.2)
}

/// One contiguous, uniformly selected part of a run.
///
/// A run may be partly selected, so it is painted as two spans rather than highlighted
/// whole; `run_start` is kept because a span's characters are an offset into the run's
/// text.
struct Span<'a> {
    run: &'a CellRun,
    run_start: u16,
    from: u16,
    to: u16,
    selected: bool,
    mark: Option<search::Mark>,
}

/// What is drawn over a grid besides its own cells.
///
/// One struct rather than two arguments because both halves split a row the same way and a
/// renderer that took them separately would walk each row twice.
#[derive(Debug, Clone, Copy, Default)]
pub struct Decoration<'a> {
    pub selection: Option<&'a Selection>,
    /// Search matches on the rows being painted, in the grid's own coordinates —
    /// [`search::PaneSearch::highlights`].
    pub matches: &'a [search::Highlight],
}

impl<'a> Decoration<'a> {
    /// Nothing over the cells: what a preview or a snapshot of the lattice wants.
    pub fn none() -> Self {
        Self {
            selection: None,
            matches: &[],
        }
    }

    pub fn selected(selection: Option<&'a Selection>) -> Self {
        Self {
            selection,
            matches: &[],
        }
    }

    /// How this row's cell at `col` should be decorated.
    fn at(&self, row: u16, col: u16) -> (bool, Option<search::Mark>) {
        (
            self.selection.is_some_and(|s| s.contains(row, col)),
            search::mark_at(self.matches, row, col),
        )
    }
}

/// A row's runs, split wherever the selection or a match starts and stops.
fn row_spans<'a>(runs: &'a [CellRun], decoration: &Decoration<'_>, row: u16) -> Vec<Span<'a>> {
    let mut spans = Vec::with_capacity(runs.len());
    let mut col: u16 = 0;
    for run in runs {
        let width = run.cells;
        if width == 0 {
            continue;
        }
        let mut from = col;
        while from < col + width {
            let state = decoration.at(row, from);
            let mut to = from + 1;
            while to < col + width && decoration.at(row, to) == state {
                to += 1;
            }
            spans.push(Span {
                run,
                run_start: col,
                from,
                to,
                selected: state.0,
                mark: state.1,
            });
            from = to;
        }
        col = col.saturating_add(width);
    }
    spans
}

/// Paints a grid.
///
/// Pure drawing: no input and no state, so a snapshot test can exercise the painting
/// without an interaction model in the way.
///
/// Paints nothing at all when the font cannot be measured. There is no fallback cell size
/// on purpose: an invented one is the defect this module was rewritten to remove, and it
/// would be invisible until somebody put Turn next to a real terminal.
pub fn paint(
    ui: &Ui,
    theme: &Theme,
    rect: Rect,
    grid: &Grid,
    selection: Option<&Selection>,
    options: PaneOptions,
) {
    paint_decorated(
        ui,
        theme,
        rect,
        grid,
        Decoration::selected(selection),
        options,
    );
}

/// Paints a grid with everything drawn over it: the selection and the search's matches.
///
/// The same function [`paint`] is, with the decoration spelled out. Two entry points rather
/// than one so a caller that has no search — a preview, a snapshot of the lattice — does not
/// have to invent an empty one.
pub fn paint_decorated(
    ui: &Ui,
    theme: &Theme,
    rect: Rect,
    grid: &Grid,
    decoration: Decoration<'_>,
    options: PaneOptions,
) {
    paint_with_images(ui, theme, rect, grid, decoration, options, None);
}

/// Paints a grid, drawing the inline images its cells refer to.
///
/// The entry point [`show`] uses, and the only one that can draw a picture: the pixels live
/// in an [`images::ImageCache`] the pane owns across frames.
///
/// `images` is `None` for a caller that has no cache — a preview, or a snapshot test about
/// the lattice — and a picture then comes out as the framed placeholder that stands for "the
/// pixels are not here". That is deliberate rather than blank: a picture which silently did
/// not appear is indistinguishable from a defect.
pub fn paint_with_images(
    ui: &Ui,
    theme: &Theme,
    rect: Rect,
    grid: &Grid,
    decoration: Decoration<'_>,
    options: PaneOptions,
    mut images: Option<&mut ImageCache>,
) {
    let Some(cell) = theme.cell_size(ui) else {
        return;
    };
    let painter = ui.painter().with_clip_rect(rect);
    painter.rect_filled(rect, 0.0, theme.background);

    let lattice = CellGrid::new(
        grid_origin(rect, options),
        cell,
        ui.ctx().pixels_per_point(),
    );
    let clip = rect.intersect(ui.clip_rect());

    // A cache of its own for a caller that brought none, so the placeholder path is the same
    // code as the real one. Its textures are dropped with it, which is why a real pane keeps
    // one in `PaneInteraction` instead of relying on this.
    let mut scratch = ImageCache::default();
    let has_images = grid.has_images();
    for row in visible_rows(grid, lattice.origin(), cell, clip) {
        let runs = grid.row_runs(row);
        let spans = row_spans(&runs, &decoration, row);
        for span in &spans {
            paint_background(&painter, theme, &lattice, span, row);
        }
        // Pictures between the backgrounds and the glyphs. A picture covers the background
        // of the cells it occupies, and no cell is ever both a picture and a character, so
        // the two passes cannot fight over one.
        if has_images {
            let cache = match images.as_deref_mut() {
                Some(cache) => cache,
                None => &mut scratch,
            };
            let selected = |row: u16, col: u16| {
                decoration
                    .selection
                    .is_some_and(|selection| selection.contains(row, col))
            };
            images::paint_row(&painter, theme, &lattice, grid, row, cache, &selected);
        }
        for span in &spans {
            paint_glyphs(&painter, theme, &lattice, span, row);
        }
    }

    paint_cursor(&painter, theme, grid, &lattice, options);
    paint_scroll_marker(&painter, theme, rect, grid, options);
    paint_scroll_position(&painter, theme, rect, grid, options);
}

/// The colour behind a span, if it has one.
fn paint_background(
    painter: &egui::Painter,
    theme: &Theme,
    lattice: &CellGrid,
    span: &Span<'_>,
    row: u16,
) {
    let (_, bg) = colours(span.run, theme, span.selected, span.mark);
    if let Some(bg) = bg {
        painter.rect_filled(lattice.span(row, span.from, span.to - span.from), 0.0, bg);
    }
}

/// The characters of a span, each on its own cell.
fn paint_glyphs(
    painter: &egui::Painter,
    theme: &Theme,
    lattice: &CellGrid,
    span: &Span<'_>,
    row: u16,
) {
    let (fg, _) = colours(span.run, theme, span.selected, span.mark);
    // Weight comes from the family, and egui's default monospace has one face. The
    // honest rendering of bold is therefore a brighter colour, which keeps every glyph
    // on its own column — faking weight by shearing or double-drawing would break the
    // grid, which is the one thing a terminal cannot afford.
    let colour = if span.run.attrs.has(CellAttrs::BOLD) {
        fg.gamma_multiply(1.4)
    } else {
        fg
    };
    let extent = lattice.span(row, span.from, span.to - span.from);
    // An image cell's text is a marker with no glyph anywhere: drawing it would put a
    // missing-glyph box over the picture it stands for.
    if span.run.attrs.has(CellAttrs::IMAGE) {
        return;
    }
    if !span.run.text.is_empty() {
        let columns = glyph_columns(span.run);
        for col in span.from..span.to {
            let Some(text) = glyph_at(span.run, span.run_start, col) else {
                continue;
            };
            let cell = lattice.span(row, col, columns);
            paint_glyph(
                painter,
                theme,
                text,
                cell,
                colour,
                lattice.pixels_per_point(),
            );
        }
    }
    if span.run.attrs.has(CellAttrs::UNDERLINE) {
        painter.hline(
            extent.x_range(),
            extent.max.y - 2.0,
            Stroke::new(1.0, colour),
        );
    }
    if span.run.attrs.has(CellAttrs::ITALIC) {
        // Same reason as bold: no italic face, so the slant is expressed as a faint rule
        // rather than by shearing glyphs off their columns.
        painter.hline(
            extent.x_range(),
            extent.min.y + 1.0,
            Stroke::new(1.0, colour.gamma_multiply(0.4)),
        );
    }
}

/// Paints one cell's text: Turn's own geometry for the characters a font cannot place on a
/// grid, and a glyph for everything else.
fn paint_glyph(
    painter: &egui::Painter,
    theme: &Theme,
    text: &str,
    cell: Rect,
    colour: Color32,
    pixels_per_point: f32,
) {
    if let Some(single) = single_char(text) {
        if boxdraw::paint(painter, single, cell, colour, pixels_per_point) {
            return;
        }
    }
    painter.text(
        cell.left_top(),
        Align2::LEFT_TOP,
        text,
        theme.mono.clone(),
        colour,
    );
}

/// The character `text` holds, if it holds exactly one.
///
/// A grapheme cluster — an emoji with a modifier, a letter with a combining accent — is more
/// than one `char` and is never one of the characters Turn draws itself.
fn single_char(text: &str) -> Option<char> {
    let mut chars = text.chars();
    match (chars.next(), chars.next()) {
        (Some(single), None) => Some(single),
        _ => None,
    }
}

/// How many columns a run's glyph covers.
///
/// Two for a wide cell — a CJK ideograph, an emoji — whose trailer to the right holds no
/// glyph of its own. Painting such a glyph into one column is what makes an emoji shift the
/// rest of its row.
fn glyph_columns(run: &CellRun) -> u16 {
    if run.attrs.has(CellAttrs::WIDE) {
        2
    } else {
        1
    }
}

/// The text of one column of a run.
///
/// A run is one of the three shapes [`CellRun`] documents: blank, one character per cell,
/// or a single cell holding a grapheme cluster. `None` for a column with nothing to draw,
/// which includes the trailing half of a wide cell.
fn glyph_at(run: &CellRun, run_start: u16, col: u16) -> Option<&str> {
    if run.text.is_empty() {
        return None;
    }
    if run.cells == 1 {
        return if col == run_start {
            Some(run.text.as_str())
        } else {
            None
        };
    }
    let offset = col.checked_sub(run_start)? as usize;
    let mut indices = run.text.char_indices().skip(offset);
    let (start, ch) = indices.next()?;
    Some(&run.text[start..start + ch.len_utf8()])
}

fn paint_cursor(
    painter: &egui::Painter,
    theme: &Theme,
    grid: &Grid,
    lattice: &CellGrid,
    options: PaneOptions,
) {
    // No cursor in a scrolled view: the cursor is on the live screen, and drawing it at
    // the same coordinates over history would put it on an unrelated character.
    if options.scrolled {
        return;
    }
    let Some((row, col)) = grid.cursor else {
        return;
    };
    if row >= grid.rows || col >= grid.cols {
        return;
    }
    // Only a focused pane blinks. Thirty blinking cursors would be a distraction, and a
    // repaint every half second per pane.
    if options.focused && !cursor_visible(options.now_ms, true) {
        return;
    }
    let under = grid.cell(row, col);
    // A wide cell is two columns, and a cursor over half a CJK character is a cursor in the
    // wrong place.
    let columns = match under {
        Some(cell) => cell.columns(),
        None => 1,
    };
    let at = lattice.span(row, col, columns);
    if options.focused {
        // A filled block with the character knocked out of it, which is what a terminal
        // cursor looks like and is legible at any font size.
        painter.rect_filled(at, 0.0, theme.cursor);
        if let Some(under) = under {
            if !under.text.is_empty() {
                paint_glyph(
                    painter,
                    theme,
                    &under.text,
                    at,
                    theme.background,
                    lattice.pixels_per_point(),
                );
            }
        }
    } else {
        // An outline for an unfocused pane: present, so the user can see where typing
        // would go, without competing for attention.
        painter.rect_stroke(
            at,
            0.0,
            Stroke::new(1.0, theme.text_faint),
            egui::StrokeKind::Inside,
        );
    }
}

/// The words shown when a pane is scrolled back.
///
/// A function rather than inline formatting so the one thing worth checking — that an
/// incomplete record says so — is testable without a window.
pub fn scroll_marker_label(rows: usize, total: usize, history_complete: bool) -> String {
    // "1,240 rows back" says how far the user has come; "of 5,000" is what says how much is
    // left, and a position with no total is the half of the sentence people complain about.
    let position = if total > rows {
        format!("{rows} of {total} rows back")
    } else {
        format!("{rows} rows back")
    };
    if history_complete {
        format!("{position} · any key returns")
    } else {
        // Short enough to survive a narrow pane, because the half of this sentence that
        // matters is the half that says the record is partial.
        format!("{position} · record starts here")
    }
}

fn paint_scroll_marker(
    painter: &egui::Painter,
    theme: &Theme,
    rect: Rect,
    grid: &Grid,
    options: PaneOptions,
) {
    if !options.scrolled {
        return;
    }
    let bar = Rect::from_min_size(rect.min, Vec2::new(rect.width(), SCROLL_MARKER_HEIGHT));
    painter.rect_filled(bar, 0.0, theme.raised);
    painter.hline(bar.x_range(), bar.max.y, Stroke::new(1.0, theme.border));
    painter.text(
        bar.left_center() + Vec2::new(8.0, 0.0),
        Align2::LEFT_CENTER,
        scroll_marker_label(
            grid.scrollback_offset,
            grid.scrollback_len,
            options.history_complete,
        ),
        FontId::new(11.0, egui::FontFamily::Monospace),
        if options.history_complete {
            theme.text_dim
        } else {
            theme.attention
        },
    );
}

/// How wide the scroll position indicator is.
///
/// Wide enough to see against a wall of text without being wide enough to cover a column of
/// it: the pane is measured in cells and this sits inside the last one.
pub const SCROLL_INDICATOR_WIDTH: f32 = 7.0;

/// The track and the thumb of the position indicator, or `None` when there is nothing to
/// indicate.
///
/// A number of rows is not a position: "1,240 rows back" tells the user how far they have
/// come and nothing about how much is left. The thumb does, at a glance, the way a
/// scrollbar does — and it is the reason this is a pure function: where the thumb sits for a
/// given offset is arithmetic, and arithmetic is testable.
///
/// The thumb is never shorter than a few points, so a viewport that is a fortieth of a
/// five-thousand-row record is still something the eye can find.
pub fn scroll_indicator(rect: Rect, grid: &Grid, options: PaneOptions) -> Option<(Rect, Rect)> {
    if !options.scrolled || grid.alternate_screen {
        return None;
    }
    let total = grid.scrollback_len + usize::from(grid.rows);
    if total == 0 || grid.scrollback_len == 0 || rect.height() <= 0.0 {
        return None;
    }
    let track = Rect::from_min_max(
        egui::pos2(rect.max.x - SCROLL_INDICATOR_WIDTH, rect.min.y),
        rect.max,
    );
    let visible = usize::from(grid.rows).min(total) as f32 / total as f32;
    let height = (track.height() * visible).max(12.0).min(track.height());
    // The offset counts backwards from the live screen, so the top of the viewport is
    // `scrollback_len - offset` rows into the record.
    let top_row = grid.scrollback_len.saturating_sub(grid.scrollback_offset) as f32;
    let span = (total as f32 - usize::from(grid.rows) as f32).max(1.0);
    let travel = (track.height() - height).max(0.0);
    let y = track.min.y + travel * (top_row / span).clamp(0.0, 1.0);
    let thumb = Rect::from_min_size(
        egui::pos2(track.min.x, y),
        Vec2::new(SCROLL_INDICATOR_WIDTH, height),
    );
    Some((track, thumb))
}

/// Draws where in the record the viewport is.
fn paint_scroll_position(
    painter: &egui::Painter,
    theme: &Theme,
    rect: Rect,
    grid: &Grid,
    options: PaneOptions,
) {
    let Some((track, thumb)) = scroll_indicator(rect, grid, options) else {
        return;
    };
    // The track is the marker strip's colour, so the two halves of "where am I" read as one
    // piece of chrome; the thumb is text-bright, because a thumb nobody can find is a
    // decoration rather than an indicator.
    painter.rect_filled(track, 0.0, theme.raised);
    painter.rect_filled(thumb.shrink2(Vec2::new(1.0, 0.0)), 2.0, theme.text_dim);
}

/// What the window lends a pane so its menu can work.
///
/// The pane can see its own grid and selection; it cannot see whether this is the last pane
/// of a session, whether a process is behind it, or which chords the user has rebound.
/// Those come from here, and every one of them is the *reason* an item is unavailable
/// rather than a boolean, because the menu has to say why.
#[derive(Clone, Copy)]
pub struct PaneChrome<'a> {
    pub shortcuts: &'a PaneShortcuts,
    pub context: &'a PaneContext,
}

/// One frame's worth of what a pane is being asked to draw.
///
/// A struct rather than a dozen arguments, because the list grew past the point where a
/// call site could be read: `show_pane(ui, state, input)` says what it does and the fields
/// say what they are.
pub struct PaneInput<'a> {
    pub theme: &'a Theme,
    pub rect: Rect,
    pub grid: &'a Grid,
    pub options: PaneOptions,
    /// Distinguishes panes so `egui` tracks their interaction independently, which is what
    /// lets two panes hold separate selections.
    pub id: egui::Id,
    /// `None` for a caller that has not wired the pane menu: the pane then offers no menu
    /// and produces no [`PaneRequest`], which is exactly how it behaved before the menu
    /// existed.
    pub chrome: Option<PaneChrome<'a>>,
}

/// Draws a pane and collects what the user did to it.
///
/// The older entry point, kept because it is what the window still calls. It has no menu
/// and never produces a [`PaneRequest`]; [`show_pane`] is the complete one.
pub fn show(
    ui: &mut Ui,
    theme: &Theme,
    rect: Rect,
    grid: &Grid,
    state: &mut PaneInteraction,
    options: PaneOptions,
    id: egui::Id,
) -> Vec<PaneAction> {
    let outcome = show_pane(
        ui,
        state,
        PaneInput {
            theme,
            rect,
            grid,
            options,
            id,
            chrome: None,
        },
    );
    debug_assert!(
        outcome.requests.is_empty(),
        "a pane with no chrome has nothing to ask the window for"
    );
    outcome.actions
}

/// Draws a pane, with its context menu, and collects everything the user did to it.
pub fn show_pane(ui: &mut Ui, state: &mut PaneInteraction, input: PaneInput<'_>) -> PaneOutcome {
    let PaneInput {
        theme,
        rect,
        grid,
        options,
        id,
        chrome,
    } = input;
    let mut outcome = PaneOutcome::default();
    // Nothing can be drawn or reported without a measured cell, and a size reported from a
    // guessed one is the bug in the report: a program laid out for a width Turn never drew.
    let Some(cell) = theme.cell_size(ui) else {
        return outcome;
    };
    let response = ui.interact(rect, id, Sense::click_and_drag());

    // The size in cells, reported when it changes rather than every frame: a
    // `resize_pty` per frame during a window drag would be hundreds of requests. Measured
    // from the *live* geometry, without the scroll marker's strip: the pty's size must not
    // change just because the user looked at history.
    let size = size_in_cells(rect, cell);
    if state.reported_size != Some(size) {
        state.reported_size = Some(size);
        outcome.actions.push(PaneAction::Resize(size));
    }

    // The search looks at the pane it is drawn over: a record that has grown can hold
    // matches the last answer did not, and once the daemon's ring starts dropping rows every
    // line index moves.
    state.search.observe(grid, options.now_ms);
    let matches = state.search.highlights(grid);
    paint_with_images(
        ui,
        theme,
        rect,
        grid,
        Decoration {
            selection: state.selection.as_ref(),
            matches: &matches,
        },
        options,
        Some(&mut state.images),
    );
    if let Some(at) = state.keyboard_cursor() {
        paint_keyboard_selection(ui, theme, rect, grid, at, options);
    }
    // After the cells, because the hover decoration goes over them, and before the pointer is
    // collected, so a modifier-click is recorded as following a link rather than only as a
    // click in the pane.
    show_links(ui, theme, &response, cell, rect, grid, state, options);
    describe_for_screen_reader(&response, grid, state, options);

    // Drawn after the grid so it is above it, and before the input is collected so that the
    // field can take the keyboard for this frame rather than the frame after.
    search::show_bar(ui, theme, rect, &mut state.search, options.now_ms);
    let searching = state.search.is_open();

    if response.clicked() && !options.focused {
        outcome.actions.push(PaneAction::Focus);
    }
    if !(searching && pointer_in_bar(ui, rect)) {
        collect_pointer(
            ui,
            &response,
            cell,
            rect,
            grid,
            state,
            options,
            &mut outcome,
        );
    }
    // While the find field holds the keyboard every keystroke is the search's. Sending them
    // to the program as well would type the query into whatever is running.
    let field_has_keyboard = searching && ui.ctx().memory(|m| m.focused().is_some());
    if options.focused && options.accepts_input && !field_has_keyboard {
        collect_keys(ui, grid, state, chrome, &mut outcome);
    }
    if let Some(chrome) = chrome {
        // A menu asked for with the keyboard opens at the cell cursor; one asked for with
        // the pointer opens on the cell the pointer is over, which is what decides whether
        // "Open Link" has a link to open.
        let requested = state.menu_at.take();
        if requested.is_some() {
            state.menu_anchor = requested;
        }
        let anchored = state.menu_anchor;
        let at = anchored
            .or_else(|| pointer_cell(&response, rect, cell, grid, options))
            .unwrap_or(CellPos::new(0, 0));
        let lattice = CellGrid::new(
            grid_origin(rect, options),
            cell,
            ui.ctx().pixels_per_point(),
        );
        show_menu(
            ui,
            theme,
            &response,
            grid,
            state,
            chrome,
            MenuOpening {
                at,
                just_asked_for: requested.is_some(),
                anchor: anchored
                    .map(|cursor| lattice.span(cursor.row, cursor.col, 1).left_bottom()),
                focused: options.focused,
            },
            &mut outcome,
        );
    }
    outcome
}

/// Whether the pointer is over the search bar rather than over the pane's cells.
///
/// A drag that began on the find field must not also start a selection behind it: the two
/// would fight, and the visible result is text highlighting itself while the user is trying
/// to put a cursor in a text box.
fn pointer_in_bar(ui: &Ui, rect: Rect) -> bool {
    ui.input(|i| i.pointer.hover_pos())
        .is_some_and(|pos| search::bar_rect(rect).contains(pos))
}

/// The cell the pointer is over, for a menu that has to know what it was opened on.
fn pointer_cell(
    response: &Response,
    rect: Rect,
    cell: Vec2,
    grid: &Grid,
    options: PaneOptions,
) -> Option<CellPos> {
    let pos = response
        .interact_pointer_pos()
        .or_else(|| response.hover_pos())?;
    Some(cell_at(pos, grid_origin(rect, options), cell, grid))
}

/// How the menu came to be open this frame.
#[derive(Debug, Clone, Copy, PartialEq)]
struct MenuOpening {
    /// The cell it was opened over.
    at: CellPos,
    /// Whether the keystroke that asks for it arrived on *this* frame, which is the frame
    /// that has to force it open.
    just_asked_for: bool,
    /// Where to put it, for a menu that has no pointer position to sit at.
    anchor: Option<Pos2>,
    /// Whether the pane it belongs to is already focused, so choosing an item that acts on
    /// this pane knows whether it has to ask for focus first.
    focused: bool,
}

/// Opens the pane's menu when it is asked for, and performs whatever was chosen.
#[allow(clippy::too_many_arguments)]
fn show_menu(
    ui: &mut Ui,
    theme: &Theme,
    response: &Response,
    grid: &Grid,
    state: &mut PaneInteraction,
    chrome: PaneChrome<'_>,
    opening: MenuOpening,
    outcome: &mut PaneOutcome,
) {
    let popup_id = response.id.with("pane-menu");
    let keyboard = opening.anchor.is_some();
    let mut popup = egui::Popup::context_menu(response);
    if opening.just_asked_for {
        // There was no secondary click for `egui` to notice, so the frame the keystroke
        // arrived on is the frame that opens it.
        popup = popup.open_memory(Some(egui::SetOpenCommand::Bool(true)));
    }
    if let Some(anchor) = opening.anchor {
        popup = popup.at_position(anchor);
    }
    let menu = PaneMenu {
        grid,
        at: opening.at,
        selection: state.selection.as_ref(),
        context: chrome.context,
        shortcuts: chrome.shortcuts,
        // The pane's own scan, which is the same map the hover decoration reads: "Open Link"
        // and the underline under the pointer must never disagree about what is a link. The
        // pane has scanned by the time a menu can be opened over it, because a right-click is
        // itself a pointer event on this pane.
        links: Some(state.links()),
    };
    // The menu borrows the pane's selection and its link map; performing an item needs the
    // pane mutably. Both halves are settled inside this scope so neither has to be cloned to
    // satisfy the borrow checker.
    let (command, chosen, closed) = {
        let chosen = popup
            .id(popup_id)
            // A menu opened from the keyboard puts its first usable item in focus, because
            // `egui`'s arrow-key navigation moves *between* focused widgets and has nothing
            // to move from until one of them has it.
            .show(|ui| menu::show_items_focusing(ui, theme, &menu.items(), keyboard))
            .and_then(|inner| inner.inner);
        let closed = !egui::Popup::is_id_open(ui.ctx(), popup_id);
        (
            chosen,
            Chosen {
                selected: menu.selected_text(),
                link: menu.link(),
            },
            closed,
        )
    };
    if closed {
        // The menu has gone, so the anchor it was pinned to goes with it. Left behind, it
        // would put the *next* right-click's menu at the last keyboard cursor.
        state.menu_anchor = None;
    }
    let Some(command) = command else {
        return;
    };
    perform(ui, command, grid, state, chosen, opening.focused, outcome);
}

/// What the pane worked out about the cell the menu was opened over.
///
/// Computed before the item is performed, because performing it needs the selection
/// *mutably* and reading it needs it shared: taking the two strings out first is what lets
/// "Copy" put the selection on the clipboard and "Select All" replace it in the same
/// function.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Chosen {
    selected: Option<String>,
    link: Option<links::LinkRequest>,
}

/// Carries out a menu item.
///
/// Split out so the mapping from item to consequence is one readable table, and so a test
/// can drive it without a window: this is where "Copy puts the selection on the clipboard"
/// and "Clear Buffer never touches the program's screen" actually live.
#[allow(clippy::too_many_arguments)]
fn perform(
    ui: &Ui,
    command: PaneCommand,
    grid: &Grid,
    state: &mut PaneInteraction,
    chosen: Chosen,
    focused: bool,
    outcome: &mut PaneOutcome,
) {
    // An item that acts on this pane needs it focused first, because the menu may have been
    // opened on a pane that was not. Already focused, there is nothing to ask for: a
    // redundant focus request per keystroke would be a round trip to the daemon for nothing.
    if command.needs_focus() && !focused {
        outcome.actions.push(PaneAction::Focus);
    }
    match command {
        PaneCommand::Copy => {
            if let Some(text) = chosen.selected {
                outcome.actions.push(PaneAction::Copy(text));
            }
        }
        // Paste is not read from the clipboard here. Asking the *platform* for it produces
        // an `egui::Event::Paste` on the next frame, which goes through the same path a
        // Cmd+V press does — including the bracketed-paste encoding and the stripping of
        // the terminator. One path to the pty, and the user is on it.
        PaneCommand::Paste => request_clipboard_paste(ui.ctx()),
        PaneCommand::SelectAll => state.select_all(grid),
        PaneCommand::ClearBuffer => {
            state.clear_selection();
            outcome.requests.push(PaneRequest::ClearHistory);
        }
        PaneCommand::SearchSelection => {
            if let Some(text) = chosen.selected {
                outcome.requests.push(PaneRequest::Search(text));
            }
        }
        PaneCommand::OpenLink => {
            if let Some(link) = chosen.link {
                outcome.requests.push(PaneRequest::FollowLink(link));
            }
        }
        PaneCommand::SplitHorizontally => outcome
            .requests
            .push(PaneRequest::Split(Direction::Horizontal)),
        PaneCommand::SplitVertically => outcome
            .requests
            .push(PaneRequest::Split(Direction::Vertical)),
        PaneCommand::ClosePane => outcome.requests.push(PaneRequest::Close),
    }
}

/// Asks the platform for the clipboard, on the user's behalf.
///
/// The only way a pane ever gets clipboard text: the request goes to the window system,
/// which hands the contents back as [`egui::Event::Paste`] on a later frame. Turn never
/// reads the clipboard itself, and nothing a *process* emits can reach this function — OSC
/// 52 is refused in `turn_pty` and has no path into the window at all.
pub fn request_clipboard_paste(ctx: &egui::Context) {
    ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaste);
}

/// The modifier that follows a link, and the words for it.
///
/// `Modifiers::command` is Cmd on macOS and Ctrl everywhere else, which is exactly the
/// convention every terminal already uses — iTerm2 and Terminal.app open a URL on ⌘-click,
/// GNOME Terminal and Konsole on Ctrl-click. A modifier rather than a plain click for a
/// reason that is not convention: a plain click that opened things would make dragging a
/// selection across a URL ambiguous, and the selection is the gesture people use far more
/// often.
pub fn follow_modifier_label() -> &'static str {
    // Words, not `⌘`. The bundled proportional face cannot place it — see
    // `keymap::GLYPHS_THE_BUNDLED_FONTS_CANNOT_PLACE` — and a hint that renders as a hollow
    // box is worse than no hint. The rest of Turn spells chords the same way.
    if cfg!(target_os = "macos") {
        "Cmd-click to open"
    } else {
        "Ctrl-click to open"
    }
}

/// Finds the links on a grid, draws the one under the pointer, and notices a click on it.
///
/// Everything here is skipped unless the pointer is over the pane: a pane nobody is pointing
/// at has no hover to draw and no reason to ask the filesystem about anything.
#[allow(clippy::too_many_arguments)]
fn show_links(
    ui: &Ui,
    theme: &Theme,
    response: &Response,
    cell: Vec2,
    rect: Rect,
    grid: &Grid,
    state: &mut PaneInteraction,
    options: PaneOptions,
) {
    let Some(pos) = response.hover_pos().filter(|_| response.hovered()) else {
        state.hovered_link = None;
        return;
    };
    let at = cell_at(pos, grid_origin(rect, options), cell, grid);
    let Some(since) = state.hover_link(grid, at, options.now_ms) else {
        return;
    };
    let modifiers = ui.input(|i| i.modifiers);
    let Some(link) = state.hovered_link() else {
        return;
    };
    if modifiers.command {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    paint_link_underline(ui, theme, rect, cell, link, options);
    if links::target_visible(since, options.now_ms, modifiers.command) {
        paint_link_target(ui, theme, response.id, pos, link);
    }
    if follows_link(response.clicked(), response.dragged(), modifiers.command) {
        state.pending_link = Some(link.request());
    }
}

/// Whether a pointer gesture over a link follows it.
///
/// A **click** with the modifier, and nothing else. Not a plain click, because output is text
/// and a click in text places nothing but a selection. Not a drag, even with the modifier
/// held, because a drag is how a selection is made and taking that away over a URL would
/// break the gesture people use most in a terminal — which is the requirement that a link
/// under the pointer must not interfere with selecting text over it.
pub fn follows_link(clicked: bool, dragged: bool, modifier_held: bool) -> bool {
    clicked && !dragged && modifier_held
}

/// A hash of everything about a grid that could change which links are on it.
///
/// Blank cells contribute nothing at all, which is what keeps this cheap: a terminal screen
/// is mostly blank, so the cost is the number of characters on it rather than the number of
/// cells.
fn text_fingerprint(grid: &Grid) -> u64 {
    let mut hasher = DefaultHasher::new();
    grid.rows.hash(&mut hasher);
    grid.cols.hash(&mut hasher);
    for (index, cell) in grid.cells.iter().enumerate() {
        if cell.text.is_empty() {
            continue;
        }
        index.hash(&mut hasher);
        cell.text.hash(&mut hasher);
    }
    for row in 0..grid.rows {
        let meta = grid.row_meta(row);
        if meta.is_default() {
            continue;
        }
        row.hash(&mut hasher);
        meta.wrapped.hash(&mut hasher);
        for link in &meta.links {
            link.from.hash(&mut hasher);
            link.to.hash(&mut hasher);
            link.uri.hash(&mut hasher);
        }
    }
    hasher.finish()
}

/// Underlines the link the pointer is over, on every row it covers.
///
/// An underline rather than a colour change: the cell's own colour is the program's, and
/// recolouring output to say "this is a link" would misreport what the program printed.
pub fn paint_link_underline(
    ui: &Ui,
    theme: &Theme,
    rect: Rect,
    cell: Vec2,
    link: &Link,
    options: PaneOptions,
) {
    let painter = ui.painter().with_clip_rect(rect);
    let lattice = CellGrid::new(
        grid_origin(rect, options),
        cell,
        ui.ctx().pixels_per_point(),
    );
    for span in &link.spans {
        let extent = lattice.span(span.row, span.from, span.to.saturating_sub(span.from));
        painter.hline(
            extent.x_range(),
            extent.max.y - 1.0,
            Stroke::new(1.0, theme.running),
        );
    }
}

/// Shows the whole target of a hovered link.
///
/// The *whole* target, wrapped over as many lines as it needs and never elided: a shortened
/// URL is how somebody is deceived about where a click goes, so there is no width at which
/// this is allowed to give up and put an ellipsis in the middle of a host name.
pub fn paint_link_target(ui: &Ui, theme: &Theme, id: egui::Id, pointer: Pos2, link: &Link) {
    let padding = Vec2::new(8.0, 5.0);
    // The window's content area, so a panel near the bottom edge flips above the pointer
    // rather than being drawn off the screen.
    let screen = ui.ctx().input(|i| i.content_rect());
    let wrap = (screen.width() * 0.6).max(200.0);
    let painter = ui
        .ctx()
        .layer_painter(egui::LayerId::new(egui::Order::Tooltip, id.with("link")));

    let target = painter.layout(
        link.target.display(),
        FontId::new(12.0, egui::FontFamily::Monospace),
        theme.text,
        wrap,
    );
    let (note, note_colour) = match link.warning() {
        Some(warning) => (warning.describe(), theme.attention),
        None => (follow_modifier_label().to_string(), theme.text_dim),
    };
    let note = painter.layout(
        note,
        FontId::new(11.0, egui::FontFamily::Proportional),
        note_colour,
        wrap,
    );

    let width = target.size().x.max(note.size().x) + padding.x * 2.0;
    let height = target.size().y + note.size().y + padding.y * 2.0 + 2.0;
    // Below the pointer by default, above it when there is no room: a panel that ran off the
    // bottom of the window would hide the thing it exists to show.
    let mut min = pointer + Vec2::new(12.0, 18.0);
    if min.y + height > screen.max.y {
        min.y = (pointer.y - height - 8.0).max(screen.min.y);
    }
    min.x = min.x.min(screen.max.x - width).max(screen.min.x);
    let panel = Rect::from_min_size(min, Vec2::new(width, height));

    painter.rect_filled(panel, 3.0, theme.raised);
    painter.rect_stroke(
        panel,
        3.0,
        Stroke::new(1.0, theme.border),
        egui::StrokeKind::Inside,
    );
    painter.galley(panel.min + padding, target.clone(), theme.text);
    painter.galley(
        panel.min + padding + Vec2::new(0.0, target.size().y + 2.0),
        note,
        note_colour,
    );
}

/// Puts the pane in the accessibility tree.
///
/// Without this the pane does not exist for a screen reader: there is no DOM behind a
/// GPU-drawn terminal, and a wall of individually labelled cells would be unusable even
/// if there were. The value is the screen's lines, which is what a screen reader reads.
fn describe_for_screen_reader(
    response: &Response,
    grid: &Grid,
    state: &PaneInteraction,
    options: PaneOptions,
) {
    let label = pane_label(grid, state, options);
    let value = grid.text();
    response.ctx.accesskit_node_builder(response.id, |node| {
        node.set_role(egui::accesskit::Role::Terminal);
        node.set_label(label);
        node.set_value(value);
        node.add_action(egui::accesskit::Action::Focus);
    });
}

/// The accessible name of a pane.
///
/// Says the geometry and whether the view is live, because a screen-reader user has no
/// other way to know either — and reading history as though it were the live screen is
/// the kind of confusion that costs somebody an hour.
pub fn terminal_label(grid: &Grid, options: PaneOptions) -> String {
    let mut label = format!("Terminal, {} rows by {} columns", grid.rows, grid.cols);
    if options.scrolled {
        label.push_str(&format!(", scrolled back {} rows", grid.scrollback_offset));
    }
    let pictures = grid.images.len();
    if pictures > 0 {
        // A screen reader user has no other way to know a picture is there at all, and the
        // cells it occupies read as blank space.
        label.push_str(&format!(
            ", {pictures} inline image{}",
            if pictures == 1 { "" } else { "s" }
        ));
    }
    if grid.alternate_screen {
        label.push_str(", full-screen application");
    }
    if options.focused {
        label.push_str(", focused");
    }
    label
}

/// The accessible name, plus what the selection is doing.
///
/// A selection nobody can perceive is a selection only a sighted user has: the highlight is
/// the only feedback the pointer gives, so the tree has to say what is selected and, in
/// keyboard selection, where the cursor is.
pub fn pane_label(grid: &Grid, state: &PaneInteraction, options: PaneOptions) -> String {
    let mut label = terminal_label(grid, options);
    if let Some(at) = state.keyboard_cursor() {
        label.push_str(&format!(
            ", selecting with the keyboard, row {} column {}",
            at.row + 1,
            at.col + 1
        ));
    }
    if let Some(text) = state.selected_text(grid) {
        let kind = match state.selection.map(|selection| selection.kind) {
            Some(SelectionKind::Block) => "rectangular selection",
            _ => "selection",
        };
        label.push_str(&format!(", {kind} of {} characters", text.chars().count()));
        let lines = text.lines().count();
        if lines > 1 {
            label.push_str(&format!(" over {lines} lines"));
        }
    }
    label
}

/// Selection, focus, the wheel, and mouse reporting.
#[allow(clippy::too_many_arguments)]
fn collect_pointer(
    ui: &Ui,
    response: &Response,
    cell: Vec2,
    rect: Rect,
    grid: &Grid,
    state: &mut PaneInteraction,
    options: PaneOptions,
    outcome: &mut PaneOutcome,
) {
    let modifiers = ui.input(|i| i.modifiers);
    let mouse_modifiers = mouse::MouseModifiers {
        shift: modifiers.shift,
        alt: modifiers.alt,
        ctrl: modifiers.ctrl,
    };

    // A program that asked for the mouse gets the mouse — unless Shift is held, which is
    // the universal escape hatch for selecting text inside a full-screen tool.
    let reporting = grid.modes.mouse.reports() && !modifiers.shift;

    let origin = grid_origin(rect, options);
    if let Some(pos) = response.interact_pointer_pos() {
        let at = cell_at(pos, origin, cell, grid);
        if reporting {
            let event = if response.drag_started() || response.clicked() {
                Some(mouse::MouseEvent::Press(mouse::Button::Left))
            } else if response.drag_stopped() {
                Some(mouse::MouseEvent::Release(mouse::Button::Left))
            } else if response.dragged() {
                Some(mouse::MouseEvent::Drag(mouse::Button::Left))
            } else {
                None
            };
            if let Some(event) = event {
                if let Some(bytes) =
                    mouse::encode(event, at.row, at.col, mouse_modifiers, grid.modes.mouse)
                {
                    outcome.actions.push(PaneAction::Write(bytes));
                }
            }
        } else {
            // Turn's own selection. Alt makes it a block, for copying one column out of
            // tabular output.
            let kind = if modifiers.alt {
                SelectionKind::Block
            } else {
                SelectionKind::Linear
            };
            // Multi-click first, and each case returns: `egui` reports `clicked` for the
            // second and third clicks of a pair as well, so testing them in the other order
            // would clear the very selection the double-click had just made.
            if response.triple_clicked() {
                state.select_line(grid, at);
            } else if response.double_clicked() {
                state.select_word(grid, at, kind);
            } else if response.drag_started() {
                state.begin_drag(grid, at, kind, modifiers.shift);
            } else if response.dragged() {
                state.drag_to(grid, at);
                autoscroll(ui, response, rect, cell, grid, state, outcome);
            } else if response.clicked() {
                state.click(grid, at, modifiers.shift);
            }
        }
    }

    // The wheel: a mouse report if the program asked for one, otherwise Turn's history.
    if response.hovered() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll.abs() > 0.5 && cell.y > 0.0 {
            let notches = (scroll / cell.y).round() as i32;
            if notches != 0 {
                if reporting {
                    let button = if notches > 0 {
                        mouse::Button::WheelUp
                    } else {
                        mouse::Button::WheelDown
                    };
                    let at = ui
                        .input(|i| i.pointer.hover_pos())
                        .map(|pos| cell_at(pos, origin, cell, grid))
                        .unwrap_or(CellPos::new(0, 0));
                    // Bounded, so a trackpad flick cannot put a hundred reports on the
                    // socket in one frame.
                    for _ in 0..notches.abs().min(8) {
                        if let Some(bytes) = mouse::encode(
                            mouse::MouseEvent::Wheel(button),
                            at.row,
                            at.col,
                            mouse_modifiers,
                            grid.modes.mouse,
                        ) {
                            outcome.actions.push(PaneAction::Write(bytes));
                        }
                    }
                } else if !grid.alternate_screen {
                    outcome.actions.push(PaneAction::Scroll(notches));
                }
            }
        }
    }
}

/// Scrolls the viewport when a drag has left the pane, and takes the selection with it.
///
/// The selection is stored in the viewport's own rows, so scrolling under it would leave the
/// highlight sitting on whatever text arrived at those row numbers. Shifting it by the rows
/// that will actually move — never past the history that exists — is what keeps the
/// highlight on the cells the user chose.
fn autoscroll(
    ui: &Ui,
    response: &Response,
    rect: Rect,
    cell: Vec2,
    grid: &Grid,
    state: &mut PaneInteraction,
    outcome: &mut PaneOutcome,
) {
    let Some(pos) = response.interact_pointer_pos() else {
        return;
    };
    let wanted = autoscroll_rows(pos.y, rect, cell.y);
    if wanted == 0 || grid.alternate_screen {
        return;
    }
    let rows = selection::scrollable_rows(wanted, grid.scrollback_offset, grid.scrollback_len);
    if rows == 0 {
        return;
    }
    outcome.actions.push(PaneAction::Scroll(rows));
    if let Some(selection) = &mut state.selection {
        selection.shift_rows(rows, grid.rows);
    }
    // A pointer held still outside the pane produces no events, so without this the scroll
    // would happen once and stop.
    ui.ctx().request_repaint();
}

/// Keystrokes, text, copy and paste.
///
/// Three groups, in this order and for this reason: keyboard selection first, because while
/// it is on the arrow keys belong to the selection and not to the program; then the pane's
/// own chords; then everything else, which is encoded for the pty. A key that reaches the
/// end of this function untouched is a key the program is entitled to.
fn collect_keys(
    ui: &Ui,
    grid: &Grid,
    state: &mut PaneInteraction,
    chrome: Option<PaneChrome<'_>>,
    outcome: &mut PaneOutcome,
) {
    let events = ui.input(|i| i.events.clone());
    for event in events {
        match event {
            egui::Event::Text(text) => {
                // Swallowed in selection mode: typing while making a selection would send
                // characters to the program the user is only trying to read.
                if !text.is_empty() && state.keyboard_cursor().is_none() {
                    outcome
                        .actions
                        .push(PaneAction::Write(keys::encode_text(&text)));
                }
            }
            egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } => {
                if handle_scroll_key(grid, key, &modifiers, outcome) {
                    continue;
                }
                if handle_key(ui, grid, state, chrome, key, &modifiers, outcome) {
                    continue;
                }
                if let Some(bytes) = keys::encode_key(key, &modifiers, &grid.modes) {
                    outcome.actions.push(PaneAction::Write(bytes));
                }
            }
            egui::Event::Copy | egui::Event::Cut => {
                if let Some(text) = state.selected_text(grid) {
                    outcome.actions.push(PaneAction::Copy(text));
                }
            }
            egui::Event::Paste(text) => {
                outcome
                    .actions
                    .push(PaneAction::Write(keys::encode_paste(&text, &grid.modes)));
            }
            _ => {}
        }
    }
}

/// The keys that move Turn's own viewport.
///
/// Shift with the navigation keys, which is the convention every terminal shares and the
/// reason it is safe to take them: `Shift+PageUp` has meant "scroll the terminal" since
/// xterm, so no program is written to receive it. Everything unshifted still reaches the
/// program, which is what makes `PageUp` inside `less` work.
///
/// The ends are expressed as a scroll further than any history goes rather than as their own
/// action, because the clamp in `feed::PaneFeed::scroll_to` already knows how far that is —
/// the pane does not, and a pane that guessed would stop short of the top of a long record.
fn handle_scroll_key(
    grid: &Grid,
    key: egui::Key,
    modifiers: &egui::Modifiers,
    outcome: &mut PaneOutcome,
) -> bool {
    // A full-screen program owns its viewport, so these keys are the program's while it is
    // in front. Sending them on rather than swallowing them is the difference between
    // `Shift+PageUp` doing nothing in `vim` and doing what `vim` wants.
    if grid.alternate_screen {
        return false;
    }
    // Either modifier: Shift is the terminal convention and Command is what a macOS user's
    // hands do. Neither is a key a program can expect with these.
    if !(modifiers.shift || modifiers.command) {
        return false;
    }
    let page = i32::from(grid.rows.saturating_sub(1).max(1));
    // Deliberately larger than any scrollback and smaller than an overflow: the clamp turns
    // it into "as far as the record goes".
    const TO_THE_END: i32 = i32::MAX / 2;
    let rows = match key {
        egui::Key::PageUp => page,
        egui::Key::PageDown => -page,
        egui::Key::Home => TO_THE_END,
        egui::Key::End => -TO_THE_END,
        _ => return false,
    };
    outcome.actions.push(PaneAction::Scroll(rows));
    true
}

/// Handles a keystroke the pane owns, and says whether it did.
///
/// `false` means the key was not the pane's and must reach the program. That distinction is
/// the whole of "do not steal keys the terminal needs", one level below the keymap's own
/// version of the same rule.
#[allow(clippy::too_many_arguments)]
fn handle_key(
    ui: &Ui,
    grid: &Grid,
    state: &mut PaneInteraction,
    chrome: Option<PaneChrome<'_>>,
    key: egui::Key,
    modifiers: &egui::Modifiers,
    outcome: &mut PaneOutcome,
) -> bool {
    let Some(chrome) = chrome else {
        // Without the window's chrome there is no menu and no chord table, so every key
        // belongs to the program. This is the pre-menu behaviour, kept exactly.
        return false;
    };
    let shortcuts = chrome.shortcuts;

    if shortcuts.toggles_selection_mode(key, modifiers) {
        match state.keyboard_cursor() {
            Some(_) => state.leave_selection_mode(),
            None => state.enter_selection_mode(grid),
        }
        return true;
    }
    if shortcuts.opens_menu(key, modifiers) {
        state.request_menu();
        return true;
    }
    if state.keyboard_cursor().is_some()
        && handle_selection_key(grid, state, key, modifiers, outcome)
    {
        return true;
    }
    if let Some(command) = shortcuts.resolve(key, modifiers) {
        let at = state.keyboard_cursor().unwrap_or(CellPos::new(0, 0));
        let menu = PaneMenu {
            grid,
            at,
            selection: state.selection.as_ref(),
            context: chrome.context,
            shortcuts,
            links: Some(state.links()),
        };
        // The chord obeys the same availability rules as the menu item: a shortcut that
        // worked where the greyed item said it could not would make the menu a liar.
        let available = menu
            .items()
            .iter()
            .any(|item| item.command == command && item.enabled());
        let chosen = Chosen {
            selected: menu.selected_text(),
            link: menu.link(),
        };
        if available {
            // A chord only ever arrives in a focused pane, so there is nothing to focus.
            perform(ui, command, grid, state, chosen, true, outcome);
        }
        return true;
    }
    false
}

/// The keys that belong to keyboard selection while it is on.
fn handle_selection_key(
    grid: &Grid,
    state: &mut PaneInteraction,
    key: egui::Key,
    modifiers: &egui::Modifiers,
    outcome: &mut PaneOutcome,
) -> bool {
    use egui::Key;
    let motion = match key {
        Key::ArrowLeft if modifiers.alt => Some(Motion::WordLeft),
        Key::ArrowRight if modifiers.alt => Some(Motion::WordRight),
        Key::ArrowLeft => Some(Motion::Left),
        Key::ArrowRight => Some(Motion::Right),
        Key::ArrowUp => Some(Motion::Up),
        Key::ArrowDown => Some(Motion::Down),
        Key::Home if modifiers.command => Some(Motion::ScreenTop),
        Key::End if modifiers.command => Some(Motion::ScreenBottom),
        Key::Home => Some(Motion::LineStart),
        Key::End => Some(Motion::LineEnd),
        _ => None,
    };
    if let Some(motion) = motion {
        return state.move_cursor(grid, motion, modifiers.shift);
    }
    match key {
        // Enter copies and leaves, which is what a `tmux` user's hands already do.
        Key::Enter => {
            if let Some(text) = state.selected_text(grid) {
                outcome.actions.push(PaneAction::Copy(text));
            }
            state.leave_selection_mode();
            true
        }
        Key::Escape => {
            state.leave_selection_mode();
            true
        }
        // Everything else is swallowed rather than sent: a keystroke that reached the
        // program while the user was selecting would type into whatever is running.
        _ => true,
    }
}

/// The keyboard's cell cursor, and the line that says what the keys do.
///
/// Painted rather than left implicit because a mode with no visible state is a mode the user
/// is lost in: the cursor says where the next motion starts from, and the hint says how to
/// extend, copy and leave. Both are drawn over the pane, which is honest — this is a
/// temporary mode and it is showing what it costs.
pub fn paint_keyboard_selection(
    ui: &Ui,
    theme: &Theme,
    rect: Rect,
    grid: &Grid,
    at: CellPos,
    options: PaneOptions,
) {
    let Some(cell) = theme.cell_size(ui) else {
        return;
    };
    let painter = ui.painter().with_clip_rect(rect);
    let lattice = CellGrid::new(
        grid_origin(rect, options),
        cell,
        ui.ctx().pixels_per_point(),
    );
    let columns = grid
        .cell(at.row, at.col)
        .map(turn_proto::cells::Cell::columns)
        .unwrap_or(1);
    painter.rect_stroke(
        lattice.span(at.row, at.col, columns),
        0.0,
        Stroke::new(2.0, theme.cursor),
        egui::StrokeKind::Inside,
    );

    let bar = Rect::from_min_max(
        Pos2::new(rect.min.x, rect.max.y - SELECTION_HINT_HEIGHT),
        rect.max,
    );
    painter.rect_filled(bar, 0.0, theme.raised);
    painter.hline(bar.x_range(), bar.min.y, Stroke::new(1.0, theme.border));
    painter.text(
        bar.left_center() + Vec2::new(8.0, 0.0),
        Align2::LEFT_CENTER,
        SELECTION_HINT,
        FontId::new(11.0, egui::FontFamily::Monospace),
        theme.text_dim,
    );
}

/// How tall the keyboard-selection hint is.
pub const SELECTION_HINT_HEIGHT: f32 = 18.0;

/// What the hint says.
///
/// A constant so the test that keeps it honest can check the four things a user in this mode
/// needs to know, rather than a reviewer having to trust a format string.
pub const SELECTION_HINT: &str =
    "SELECTING · arrows move · shift extends · enter copies · esc leaves";

#[cfg(test)]
mod tests {
    use super::*;

    fn grid() -> Grid {
        Grid::blank(40, 120)
    }

    /// The performance rule: a pane clipped by the window paints the rows on screen and
    /// not the forty it has.
    #[test]
    fn only_the_rows_inside_the_clip_rectangle_are_painted() {
        let cell = Vec2::new(8.0, 17.0);
        // A window showing the middle of the pane.
        let clip = Rect::from_min_max(Pos2::new(0.0, 170.0), Pos2::new(960.0, 340.0));
        assert_eq!(visible_rows(&grid(), Pos2::ZERO, cell, clip), 10..20);
    }

    /// A row half off the edge still has to be drawn, or the pane looks like it ends
    /// early.
    #[test]
    fn a_partly_visible_row_is_still_painted() {
        let cell = Vec2::new(8.0, 17.0);
        let clip = Rect::from_min_max(Pos2::new(0.0, 8.0), Pos2::new(960.0, 26.0));
        assert_eq!(
            visible_rows(&grid(), Pos2::ZERO, cell, clip),
            0..2,
            "the halves of rows 0 and 1 are both on screen"
        );
    }

    #[test]
    fn the_visible_range_is_clamped_to_the_grid_rather_than_running_past_it() {
        let cell = Vec2::new(8.0, 17.0);
        let huge = Rect::from_min_max(Pos2::new(0.0, -1_000.0), Pos2::new(960.0, 100_000.0));
        assert_eq!(visible_rows(&grid(), Pos2::ZERO, cell, huge), 0..40);

        // A pane scrolled entirely off the top of the window paints nothing.
        let above = Rect::from_min_max(Pos2::new(0.0, -500.0), Pos2::new(960.0, -400.0));
        assert!(visible_rows(&grid(), Pos2::ZERO, cell, above).is_empty());
    }

    #[test]
    fn a_pane_with_no_cell_size_yet_paints_nothing_rather_than_dividing_by_zero() {
        let clip = Rect::from_min_max(Pos2::ZERO, Pos2::new(100.0, 100.0));
        assert!(visible_rows(&grid(), Pos2::ZERO, Vec2::ZERO, clip).is_empty());
        let empty = Rect::from_min_max(Pos2::ZERO, Pos2::ZERO);
        assert!(visible_rows(&grid(), Pos2::ZERO, Vec2::new(8.0, 17.0), empty).is_empty());
    }

    /// One character per column, and a cell holding a grapheme cluster taken whole. Getting
    /// this wrong shifts the rest of the row, which is the class of bug the whole rewrite is
    /// about.
    #[test]
    fn each_column_of_a_run_paints_its_own_character() {
        let text = Grid::from_lines(&["┌─┐"], 3);
        let runs = text.row_runs(0);
        let run = &runs[0];
        assert_eq!(run.cells, 3, "one run of three cells: {runs:?}");
        assert_eq!(glyph_at(run, 0, 0), Some("┌"));
        assert_eq!(glyph_at(run, 0, 1), Some("─"));
        assert_eq!(glyph_at(run, 0, 2), Some("┐"));
        assert_eq!(glyph_at(run, 0, 3), None, "there is no fourth column");

        // A cluster is one cell however many characters it is made of.
        let cluster = CellRun {
            text: "e\u{301}".into(),
            cells: 1,
            fg: None,
            bg: None,
            attrs: CellAttrs::default(),
        };
        assert_eq!(glyph_at(&cluster, 7, 7), Some("e\u{301}"));
        assert_eq!(glyph_at(&cluster, 7, 8), None);
        assert_eq!(
            single_char("e\u{301}"),
            None,
            "a cluster is not one character"
        );
        assert_eq!(single_char("─"), Some('─'));

        // A blank run has nothing to paint, whatever its width.
        let blank = CellRun {
            text: String::new(),
            cells: 40,
            ..cluster
        };
        assert_eq!(glyph_at(&blank, 0, 0), None);
    }

    /// A wide glyph gets both of its columns, and its trailer none: that is what stops an
    /// emoji from moving everything to its right.
    #[test]
    fn a_wide_cell_is_painted_across_two_columns_and_its_trailer_across_none() {
        let mut grid = Grid::blank(1, 6);
        assert!(grid.set_wide(0, 2, "🔥"));
        let runs = grid.row_runs(0);
        let wide = runs
            .iter()
            .find(|run| run.attrs.has(CellAttrs::WIDE))
            .expect("a wide run");
        assert_eq!(glyph_columns(wide), 2);
        assert_eq!(wide.cells, 1, "two columns of screen, one cell of grid");

        let trailer = runs
            .iter()
            .find(|run| run.attrs.has(CellAttrs::WIDE_TRAILER))
            .expect("a trailer run");
        assert_eq!(glyph_columns(trailer), 1);
        assert_eq!(
            glyph_at(trailer, 3, 3),
            None,
            "the trailer holds no glyph of its own"
        );

        // And a plain cell is one column.
        let plain = runs
            .iter()
            .find(|run| run.attrs.is_plain())
            .expect("a plain run");
        assert_eq!(glyph_columns(plain), 1);
    }

    /// A run that is half selected is painted as two spans, so the highlight stops where the
    /// selection does rather than swallowing the whole run.
    #[test]
    fn a_partly_selected_run_is_split_where_the_selection_ends() {
        let grid = Grid::from_lines(&["hello world"], 11);
        let runs = grid.row_runs(0);
        let mut selection = Selection::new(CellPos::new(0, 0), SelectionKind::Linear);
        selection.extend_to(CellPos::new(0, 5));

        let spans = row_spans(&runs, &Decoration::selected(Some(&selection)), 0);
        assert_eq!(spans.len(), 2, "one selected span and one not");
        assert!(spans[0].selected && !spans[1].selected);
        assert_eq!((spans[0].from, spans[0].to), (0, 5));
        assert_eq!((spans[1].from, spans[1].to), (5, 11));

        // With nothing selected the run stays whole: one rectangle, not eleven.
        let whole = row_spans(&runs, &Decoration::none(), 0);
        assert_eq!(whole.len(), 1);
        assert_eq!((whole[0].from, whole[0].to), (0, 11));
    }

    /// A row with a match in it is split so the highlight covers exactly the columns the
    /// daemon reported, and the current match is a different colour from the rest.
    #[test]
    fn a_matched_run_is_split_where_the_match_is_and_the_current_one_looks_different() {
        let grid = Grid::from_lines(&["error: nope"], 11);
        let runs = grid.row_runs(0);
        let matches = [search::Highlight {
            row: 0,
            col: 0,
            cols: 5,
            current: false,
        }];
        let spans = row_spans(
            &runs,
            &Decoration {
                selection: None,
                matches: &matches,
            },
            0,
        );
        assert_eq!(spans.len(), 2, "the match, then the rest of the row");
        assert_eq!((spans[0].from, spans[0].to), (0, 5));
        assert_eq!(spans[0].mark, Some(search::Mark::Other));
        assert_eq!(spans[1].mark, None);

        let theme = Theme::dark();
        let (_, other) = colours(&runs[0], &theme, false, Some(search::Mark::Other));
        let (current_fg, current_bg) =
            colours(&runs[0], &theme, false, Some(search::Mark::Current));
        assert_eq!(other, Some(search::match_background(&theme)));
        assert_eq!(current_bg, Some(theme.attention));
        assert_eq!(current_fg, theme.background);
        assert_ne!(
            other, current_bg,
            "a search where every hit looks the same is one where next does nothing visible"
        );
    }

    /// The current match has to be findable even inside a selection, or stepping through
    /// results while text is selected would look like nothing happened.
    #[test]
    fn the_current_match_shows_through_a_selection_and_the_others_do_not() {
        let theme = Theme::dark();
        let run = CellRun {
            text: "a".into(),
            cells: 1,
            fg: None,
            bg: None,
            attrs: CellAttrs::default(),
        };
        assert_eq!(
            colours(&run, &theme, true, Some(search::Mark::Other)).1,
            Some(theme.selection),
            "a selection covers the other matches"
        );
        assert_eq!(
            colours(&run, &theme, true, Some(search::Mark::Current)).1,
            Some(theme.attention),
            "and the current match covers the selection"
        );
    }

    /// The keys the terminal owns, and the ones it must hand over. Shift with a navigation
    /// key has meant "scroll the terminal" since xterm; unshifted, `PageUp` belongs to
    /// whatever is running.
    #[test]
    fn shift_with_the_navigation_keys_scrolls_turns_own_viewport() {
        let mut grid = Grid::blank(24, 80);
        grid.scrollback_len = 900;
        let shift = egui::Modifiers {
            shift: true,
            ..Default::default()
        };
        let plain = egui::Modifiers::default();

        let mut outcome = PaneOutcome::default();
        assert!(handle_scroll_key(
            &grid,
            egui::Key::PageUp,
            &shift,
            &mut outcome
        ));
        assert_eq!(outcome.actions, vec![PaneAction::Scroll(23)]);

        let mut outcome = PaneOutcome::default();
        assert!(handle_scroll_key(
            &grid,
            egui::Key::PageDown,
            &shift,
            &mut outcome
        ));
        assert_eq!(outcome.actions, vec![PaneAction::Scroll(-23)]);

        // The ends are a scroll further than any record goes; the feed's clamp is what turns
        // that into the top and the bottom.
        let mut outcome = PaneOutcome::default();
        assert!(handle_scroll_key(
            &grid,
            egui::Key::Home,
            &shift,
            &mut outcome
        ));
        match outcome.actions.first() {
            Some(PaneAction::Scroll(rows)) => assert!(*rows > 5_000),
            other => panic!("expected a scroll, got {other:?}"),
        }
        let mut outcome = PaneOutcome::default();
        assert!(handle_scroll_key(
            &grid,
            egui::Key::End,
            &shift,
            &mut outcome
        ));
        match outcome.actions.first() {
            Some(PaneAction::Scroll(rows)) => assert!(*rows < -5_000),
            other => panic!("expected a scroll, got {other:?}"),
        }

        // Unshifted, every one of them is the program's.
        for key in [
            egui::Key::PageUp,
            egui::Key::PageDown,
            egui::Key::Home,
            egui::Key::End,
        ] {
            let mut outcome = PaneOutcome::default();
            assert!(
                !handle_scroll_key(&grid, key, &plain, &mut outcome),
                "{key:?} without a modifier belongs to the program"
            );
            assert!(outcome.actions.is_empty());
            assert!(
                keys::encode_key(key, &plain, &grid.modes).is_some(),
                "and it still has bytes to send"
            );
        }
    }

    /// While a full-screen program is in front it owns its own paging, so these keys are
    /// handed over rather than swallowed.
    #[test]
    fn a_full_screen_program_keeps_the_scrollback_keys_for_itself() {
        let mut grid = Grid::blank(24, 80);
        grid.scrollback_len = 900;
        grid.alternate_screen = true;
        let shift = egui::Modifiers {
            shift: true,
            ..Default::default()
        };
        let mut outcome = PaneOutcome::default();
        assert!(!handle_scroll_key(
            &grid,
            egui::Key::PageUp,
            &shift,
            &mut outcome
        ));
        assert!(outcome.actions.is_empty());
    }

    /// A number of rows is not a position. The indicator is where the eye reads "how much is
    /// left", so where the thumb sits has to be arithmetic rather than a guess.
    #[test]
    fn the_position_indicator_sits_where_the_viewport_is_in_the_record() {
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 400.0));
        let scrolled = PaneOptions {
            scrolled: true,
            ..PaneOptions::default()
        };
        let mut grid = Grid::blank(40, 80);
        grid.scrollback_len = 1_000;

        // At the very top of the record the thumb is at the top of the track.
        grid.scrollback_offset = 1_000;
        let (track, thumb) = scroll_indicator(rect, &grid, scrolled).expect("a thumb");
        assert_eq!(track.max.x, rect.max.x);
        assert_eq!(thumb.min.y, track.min.y);
        assert!(thumb.height() >= 12.0, "a thumb nobody can see is not one");

        // One row back from the live screen puts it at the bottom.
        grid.scrollback_offset = 1;
        let (_, bottom) = scroll_indicator(rect, &grid, scrolled).expect("a thumb");
        assert!(
            bottom.max.y >= track.max.y - 1.0,
            "got {bottom:?} in {track:?}"
        );

        // Half way is half way.
        grid.scrollback_offset = 500;
        let (_, middle) = scroll_indicator(rect, &grid, scrolled).expect("a thumb");
        assert!(
            (middle.center().y - track.center().y).abs() < track.height() * 0.1,
            "got {middle:?} in {track:?}"
        );

        // Nothing to indicate: a live view, a pane with no history, a full-screen program.
        assert!(scroll_indicator(rect, &grid, PaneOptions::default()).is_none());
        let mut fresh = Grid::blank(40, 80);
        fresh.scrollback_offset = 3;
        assert!(scroll_indicator(rect, &fresh, scrolled).is_none());
        let mut tui = grid.clone();
        tui.alternate_screen = true;
        assert!(scroll_indicator(rect, &tui, scrolled).is_none());
    }

    /// The protocol resolves reversed video where it can and flags only the case it
    /// cannot. A client that swapped an unflagged cell would reverse it twice.
    #[test]
    fn only_a_flagged_cell_is_reversed_by_the_renderer() {
        let theme = Theme::dark();
        let plain = CellRun {
            text: "a".into(),
            cells: 1,
            fg: Some(Rgb::new(0, 200, 0)),
            bg: None,
            attrs: CellAttrs::default(),
        };
        let (fg, bg) = colours(&plain, &theme, false, None);
        assert_eq!(fg, Color32::from_rgb(0, 200, 0));
        assert_eq!(bg, None, "an unflagged cell must be painted as it arrived");

        let flagged = CellRun {
            attrs: CellAttrs::default().with(CellAttrs::INVERSE),
            fg: None,
            ..plain
        };
        let (fg, bg) = colours(&flagged, &theme, false, None);
        assert_eq!(
            bg,
            Some(theme.text),
            "the theme's foreground moves to the back"
        );
        assert_eq!(fg, theme.background);
    }

    #[test]
    fn a_selected_run_is_painted_on_the_selection_colour_whatever_it_asked_for() {
        let theme = Theme::dark();
        let run = CellRun {
            text: "a".into(),
            cells: 1,
            fg: None,
            bg: Some(Rgb::new(90, 0, 0)),
            attrs: CellAttrs::default(),
        };
        assert_eq!(colours(&run, &theme, true, None).1, Some(theme.selection));
    }

    #[test]
    fn a_pane_remembers_nothing_selected_until_something_is() {
        let mut state = PaneInteraction::default();
        assert_eq!(state.selected_text(&grid()), None);

        let text = Grid::from_lines(&["hello"], 10);
        let mut selection = Selection::new(CellPos::new(0, 0), SelectionKind::Linear);
        selection.extend_to(CellPos::new(0, 3));
        state.selection = Some(selection);
        assert_eq!(state.selected_text(&text).as_deref(), Some("hel"));

        state.clear_selection();
        assert_eq!(state.selected_text(&text), None);
    }

    /// An empty selection must not put an empty string on the clipboard: a stray click
    /// would silently wipe whatever the user had copied.
    #[test]
    fn a_selection_covering_nothing_offers_no_text_to_copy() {
        let state = PaneInteraction {
            selection: Some(Selection::new(CellPos::new(2, 2), SelectionKind::Linear)),
            ..PaneInteraction::default()
        };
        assert_eq!(state.selected_text(&Grid::from_lines(&["hello"], 10)), None);
    }

    /// A screen-reader user has no other way to know that what they are reading is
    /// history rather than what the program is doing now.
    #[test]
    fn the_accessible_name_says_whether_the_view_is_live() {
        let mut grid = Grid::blank(24, 80);
        let live = terminal_label(&grid, PaneOptions::default());
        assert!(live.contains("24 rows by 80 columns"), "got {live}");
        assert!(!live.contains("scrolled"));

        grid.scrollback_offset = 12;
        let scrolled = terminal_label(
            &grid,
            PaneOptions {
                scrolled: true,
                ..PaneOptions::default()
            },
        );
        assert!(scrolled.contains("scrolled back 12 rows"), "got {scrolled}");

        grid.alternate_screen = true;
        let tui = terminal_label(
            &grid,
            PaneOptions {
                focused: true,
                ..PaneOptions::default()
            },
        );
        assert!(tui.contains("full-screen application"), "got {tui}");

        // A screen reader user has no other way to know a picture is on the screen at all:
        // the cells it occupies read as blank space.
        let mut with_picture = Grid::blank(24, 80);
        with_picture.images.push(turn_proto::images::GridImage::new(
            0,
            turn_proto::images::ImageId(1),
            2,
            4,
            32,
            32,
        ));
        let named = terminal_label(&with_picture, PaneOptions::default());
        assert!(named.contains("1 inline image"), "got {named}");
        assert!(
            !named.contains("images"),
            "one picture is singular: {named}"
        );
        with_picture.images.push(turn_proto::images::GridImage::new(
            1,
            turn_proto::images::ImageId(2),
            2,
            4,
            32,
            32,
        ));
        assert!(
            terminal_label(&with_picture, PaneOptions::default()).contains("2 inline images"),
            "and two are plural"
        );
        assert!(tui.contains("focused"));
    }

    /// A user who scrolled back did so in order to read the top line, so the marker must
    /// not be painted over it.
    #[test]
    fn the_scroll_marker_takes_its_own_strip_rather_than_covering_the_first_row() {
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 340.0));
        let live = grid_origin(rect, PaneOptions::default());
        assert_eq!(live, rect.min);

        let scrolled = grid_origin(
            rect,
            PaneOptions {
                scrolled: true,
                ..PaneOptions::default()
            },
        );
        assert_eq!(scrolled.y, rect.min.y + SCROLL_MARKER_HEIGHT);
        assert_eq!(scrolled.x, rect.min.x);
        // A strip too short to hold its own text would be a marker nobody can read.
        const _: () = assert!(SCROLL_MARKER_HEIGHT >= 12.0);
    }

    /// Looking at history must not resize the process. A pty told it lost a row every
    /// time somebody scrolled would reflow the program under them.
    #[test]
    fn scrolling_back_does_not_change_the_size_the_pty_is_told() {
        let cell = Vec2::new(8.0, 15.0);
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(cell.x * 40.0, cell.y * 20.0));
        assert_eq!(
            size_in_cells(rect, cell),
            PtySize::new(20, 40),
            "the size is measured from the pane, not from what is left after the marker"
        );
    }

    /// The user must be told when the history they are looking at does not go all the
    /// way back, rather than being left to assume it does.
    #[test]
    fn the_scroll_marker_says_when_the_record_is_incomplete() {
        let complete = scroll_marker_label(40, 400, true);
        assert!(
            complete.contains("40 of 400 rows back"),
            "a distance with no total is the half of the sentence people notice: {complete}"
        );
        assert!(!complete.contains("not kept"));

        // A record no deeper than the distance says the distance and nothing it cannot
        // stand behind.
        assert!(scroll_marker_label(40, 0, true).starts_with("40 rows back"));

        let partial = scroll_marker_label(40, 400, false);
        assert!(
            partial.contains("starts here"),
            "a partial record must say where it begins: {partial}"
        );
        assert_ne!(
            partial, complete,
            "the two cases must not read the same, or the warning is invisible"
        );
    }

    // ---------------------------------------------------------------------------------
    // Selection gestures. Each of these is a method on `PaneInteraction` rather than
    // logic inside the draw loop, which is what makes the behaviour testable without a
    // window and the mapping from `egui`'s response flags a single line each.
    // ---------------------------------------------------------------------------------

    fn compiler_error() -> Grid {
        Grid::from_lines(&["error at src/main.rs:42 in --jobs=4"], 40)
    }

    #[test]
    fn a_double_click_selects_the_thing_under_the_pointer_and_a_triple_click_its_line() {
        let grid = compiler_error();
        let mut state = PaneInteraction::default();

        state.select_word(&grid, CellPos::new(0, 12), SelectionKind::Linear);
        assert_eq!(
            state.selected_text(&grid).as_deref(),
            Some("src/main.rs:42")
        );

        state.select_line(&grid, CellPos::new(0, 3));
        assert_eq!(
            state.selected_text(&grid).as_deref(),
            Some("error at src/main.rs:42 in --jobs=4")
        );
    }

    /// A double-click followed by a drag has to keep growing by words. The press that
    /// begins the drag lands inside the selection the double-click made, and starting a
    /// fresh character selection there is what makes the gesture feel broken.
    #[test]
    fn a_drag_that_began_as_a_double_click_keeps_extending_by_words() {
        let grid = Grid::from_lines(&["alpha beta gamma"], 20);
        let mut state = PaneInteraction::default();
        state.select_word(&grid, CellPos::new(0, 2), SelectionKind::Linear);
        state.begin_drag(&grid, CellPos::new(0, 2), SelectionKind::Linear, false);
        state.drag_to(&grid, CellPos::new(0, 8));
        assert_eq!(
            state.selected_text(&grid).as_deref(),
            Some("alpha beta"),
            "the word granularity survived the press that started the drag"
        );
    }

    #[test]
    fn shift_clicking_extends_and_a_plain_click_clears() {
        let grid = Grid::from_lines(&["alpha beta gamma"], 20);
        let mut state = PaneInteraction::default();
        state.begin_drag(&grid, CellPos::new(0, 0), SelectionKind::Linear, false);
        state.drag_to(&grid, CellPos::new(0, 5));
        assert_eq!(state.selected_text(&grid).as_deref(), Some("alpha"));

        state.click(&grid, CellPos::new(0, 10), true);
        assert_eq!(state.selected_text(&grid).as_deref(), Some("alpha beta"));

        state.click(&grid, CellPos::new(0, 3), false);
        assert_eq!(
            state.selected_text(&grid),
            None,
            "a plain click clears rather than leaving a stale selection"
        );

        // Shift with nothing selected has nothing to extend, and must not invent a
        // selection from the top-left corner.
        state.click(&grid, CellPos::new(0, 4), true);
        assert_eq!(state.selected_text(&grid), None);
    }

    /// Alt makes the drag a rectangle. Copying one column out of a table is the gesture
    /// this exists for, so the test uses a table.
    #[test]
    fn holding_alt_drags_a_rectangle_out_of_tabular_output() {
        let grid = Grid::from_lines(
            &[
                "NAME       STATUS   PORTS",
                "api        Up       8080",
                "worker     Up       9000",
            ],
            26,
        );
        let mut state = PaneInteraction::default();
        state.begin_drag(&grid, CellPos::new(1, 20), SelectionKind::Block, false);
        state.drag_to(&grid, CellPos::new(2, 24));
        assert_eq!(state.selected_text(&grid).as_deref(), Some("8080\n9000"));

        // The same drag without Alt takes the ends of the lines instead, which is what a
        // linear selection means and is not what the user asked for.
        let mut linear = PaneInteraction::default();
        linear.begin_drag(&grid, CellPos::new(1, 20), SelectionKind::Linear, false);
        linear.drag_to(&grid, CellPos::new(2, 24));
        assert_ne!(linear.selected_text(&grid).as_deref(), Some("8080\n9000"));
    }

    #[test]
    fn select_all_takes_the_screen_and_the_selection_can_be_cleared_again() {
        let grid = Grid::from_lines(&["one", "two"], 10);
        let mut state = PaneInteraction::default();
        state.select_all(&grid);
        assert_eq!(state.selected_text(&grid).as_deref(), Some("one\ntwo"));
        state.clear_selection();
        assert_eq!(state.selected_text(&grid), None);
    }

    // ---------------------------------------------------------------------------------
    // Keyboard selection. A selection only a pointer can make is a selection half the
    // users of a terminal cannot make at all.
    // ---------------------------------------------------------------------------------

    #[test]
    fn keyboard_selection_starts_at_the_cursor_and_extends_with_shift() {
        let mut grid = Grid::from_lines(&["alpha beta", "gamma"], 10);
        grid.cursor = Some((0, 6));
        let mut state = PaneInteraction::default();
        assert_eq!(state.keyboard_cursor(), None);

        state.enter_selection_mode(&grid);
        assert_eq!(
            state.keyboard_cursor(),
            Some(CellPos::new(0, 6)),
            "the mode starts next to the text the user was looking at"
        );

        assert!(state.move_cursor(&grid, Motion::WordRight, true));
        assert_eq!(state.selected_text(&grid).as_deref(), Some("beta"));

        assert!(state.move_cursor(&grid, Motion::Down, true));
        assert_eq!(state.selected_text(&grid).as_deref(), Some("beta\ngamma"));

        // Moving without Shift drops the selection: the cursor is being placed, not
        // dragged.
        assert!(state.move_cursor(&grid, Motion::Left, false));
        assert_eq!(state.selected_text(&grid), None);

        state.leave_selection_mode();
        assert_eq!(state.keyboard_cursor(), None);
        assert!(
            !state.move_cursor(&grid, Motion::Right, true),
            "with the mode off the arrow keys belong to the program"
        );
    }

    #[test]
    fn the_keyboard_hint_says_how_to_extend_copy_and_leave() {
        for fragment in ["arrows", "shift", "enter", "esc"] {
            assert!(
                SELECTION_HINT.to_lowercase().contains(fragment),
                "the hint must mention {fragment}: {SELECTION_HINT}"
            );
        }
        const _: () = assert!(SELECTION_HINT_HEIGHT >= 12.0);
    }

    // ---------------------------------------------------------------------------------
    // The menu's consequences.
    // ---------------------------------------------------------------------------------

    fn chrome_for<'a>(shortcuts: &'a PaneShortcuts, context: &'a PaneContext) -> PaneChrome<'a> {
        PaneChrome { shortcuts, context }
    }

    fn mac_shortcuts() -> PaneShortcuts {
        PaneShortcuts::from_keymap(&crate::keymap::Keymap::build(
            &crate::keymap::Overrides::new(),
            crate::keymap::Platform::MAC,
        ))
    }

    fn mod_shift() -> egui::Modifiers {
        egui::Modifiers {
            mac_cmd: true,
            command: true,
            shift: true,
            ..egui::Modifiers::default()
        }
    }

    /// The gate that keeps the pane out of the program's way. Without the window's chrome
    /// the pane has no chord table at all, so a keystroke it would otherwise act on goes to
    /// the process untouched — which is what makes wiring the menu a decision rather than a
    /// change of behaviour that arrives by surprise.
    #[test]
    fn without_the_windows_chrome_every_keystroke_belongs_to_the_program() {
        let grid = Grid::from_lines(&["output"], 20);
        let shortcuts = mac_shortcuts();
        let context = PaneContext::default();
        egui::__run_test_ui(|ui| {
            let mut bare = PaneInteraction::default();
            let mut outcome = PaneOutcome::default();
            assert!(
                !handle_key(
                    ui,
                    &grid,
                    &mut bare,
                    None,
                    egui::Key::E,
                    &mod_shift(),
                    &mut outcome
                ),
                "with no chrome the pane must not claim the keystroke"
            );
            assert_eq!(outcome, PaneOutcome::default());
            assert_eq!(bare.selected_text(&grid), None);

            let mut wired = PaneInteraction::default();
            let mut outcome = PaneOutcome::default();
            assert!(handle_key(
                ui,
                &grid,
                &mut wired,
                Some(chrome_for(&shortcuts, &context)),
                egui::Key::E,
                &mod_shift(),
                &mut outcome
            ));
            assert_eq!(
                wired.selected_text(&grid).as_deref(),
                Some("output"),
                "the pane's own chord selects the screen"
            );
            assert!(
                !outcome
                    .actions
                    .iter()
                    .any(|action| matches!(action, PaneAction::Write(_))),
                "and nothing was typed at the program"
            );
        });
    }

    /// The two keystrokes that make the whole feature reachable without a pointer.
    #[test]
    fn the_menu_and_selection_mode_are_reachable_from_the_keyboard() {
        let mut grid = Grid::from_lines(&["alpha beta"], 12);
        grid.cursor = Some((0, 2));
        let shortcuts = mac_shortcuts();
        let context = PaneContext::default();
        egui::__run_test_ui(|ui| {
            let mut state = PaneInteraction::default();
            let mut outcome = PaneOutcome::default();
            let chrome = Some(chrome_for(&shortcuts, &context));

            let shift = egui::Modifiers {
                shift: true,
                ..egui::Modifiers::default()
            };
            assert!(handle_key(
                ui,
                &grid,
                &mut state,
                chrome,
                egui::Key::F10,
                &shift,
                &mut outcome
            ));
            assert!(
                state.menu_at.is_some(),
                "Shift+F10 must ask for the menu, as it does everywhere else"
            );

            assert!(handle_key(
                ui,
                &grid,
                &mut state,
                chrome,
                egui::Key::Space,
                &mod_shift(),
                &mut outcome
            ));
            assert_eq!(
                state.keyboard_cursor(),
                Some(CellPos::new(0, 2)),
                "and the mode starts at the program's cursor"
            );

            // In the mode, an arrow moves the selection cursor and is not sent to the pty.
            assert!(handle_key(
                ui,
                &grid,
                &mut state,
                chrome,
                egui::Key::ArrowRight,
                &shift,
                &mut outcome
            ));
            assert_eq!(state.selected_text(&grid).as_deref(), Some("p"));
            assert!(
                !outcome
                    .actions
                    .iter()
                    .any(|action| matches!(action, PaneAction::Write(_))),
                "an arrow key in selection mode must not reach the program"
            );

            // Enter copies and leaves; Escape would have left without copying.
            assert!(handle_key(
                ui,
                &grid,
                &mut state,
                chrome,
                egui::Key::Enter,
                &egui::Modifiers::default(),
                &mut outcome
            ));
            assert_eq!(state.keyboard_cursor(), None);
            assert!(outcome.actions.contains(&PaneAction::Copy("p".to_string())));
        });
    }

    /// Every item's consequence, in one place. This is the table a reviewer checks
    /// against the menu: Copy reaches the clipboard, Clear Buffer never reaches the pty,
    /// and the two structural items are requests rather than things a pane does.
    #[test]
    fn each_menu_item_produces_the_one_consequence_it_promises() {
        let mut grid = Grid::from_lines(&["see https://example.com/x"], 30);
        grid.scrollback_len = 40;
        let shortcuts = PaneShortcuts::defaults();
        let context = PaneContext::default();
        let mut selection = Selection::new(CellPos::new(0, 4), SelectionKind::Linear);
        selection.extend_to(CellPos::new(0, 25));
        let normalised = links::normalise_url("https://example.com/x")
            .expect("an ordinary https URL is openable");

        let cases = [
            (
                PaneCommand::Copy,
                vec![PaneAction::Copy("https://example.com/x".into())],
                Vec::new(),
            ),
            (
                PaneCommand::SearchSelection,
                Vec::new(),
                vec![PaneRequest::Search("https://example.com/x".into())],
            ),
            (
                PaneCommand::OpenLink,
                Vec::new(),
                // The URL as `links` normalises it, because that module owns what may be
                // opened; asserting a literal here would be a second opinion about it.
                vec![PaneRequest::FollowLink(links::LinkRequest {
                    target: links::LinkTarget::Url(normalised.clone()),
                    display: normalised.clone(),
                    text: "https://example.com/x".into(),
                    warning: None,
                })],
            ),
            (
                PaneCommand::ClearBuffer,
                vec![PaneAction::Focus],
                vec![PaneRequest::ClearHistory],
            ),
            (
                PaneCommand::SplitHorizontally,
                vec![PaneAction::Focus],
                vec![PaneRequest::Split(Direction::Horizontal)],
            ),
            (
                PaneCommand::SplitVertically,
                vec![PaneAction::Focus],
                vec![PaneRequest::Split(Direction::Vertical)],
            ),
            (
                PaneCommand::ClosePane,
                vec![PaneAction::Focus],
                vec![PaneRequest::Close],
            ),
        ];

        for (command, actions, requests) in cases {
            let mut state = PaneInteraction {
                selection: Some(selection),
                ..PaneInteraction::default()
            };
            let outcome = choose(command, &grid, &mut state, &shortcuts, &context);
            assert_eq!(outcome.actions, actions, "{}", command.label());
            assert_eq!(outcome.requests, requests, "{}", command.label());
            assert!(
                !outcome
                    .actions
                    .iter()
                    .any(|action| matches!(action, PaneAction::Write(_))),
                "{} must not write to the pty",
                command.label()
            );
        }
    }

    /// Select All is the one item that changes the pane's own state rather than asking
    /// for anything.
    #[test]
    fn choosing_select_all_selects_the_screen_and_asks_for_nothing() {
        let grid = Grid::from_lines(&["one", "two"], 8);
        let shortcuts = PaneShortcuts::defaults();
        let context = PaneContext::default();
        let mut state = PaneInteraction::default();
        let outcome = choose(
            PaneCommand::SelectAll,
            &grid,
            &mut state,
            &shortcuts,
            &context,
        );
        assert_eq!(outcome.requests, Vec::new());
        assert_eq!(outcome.actions, vec![PaneAction::Focus]);
        assert_eq!(state.selected_text(&grid).as_deref(), Some("one\ntwo"));
    }

    /// Choosing an item performs it as if the user had pressed the chord, so this drives
    /// the same function the menu does.
    fn choose(
        command: PaneCommand,
        grid: &Grid,
        state: &mut PaneInteraction,
        shortcuts: &PaneShortcuts,
        context: &PaneContext,
    ) -> PaneOutcome {
        let mut outcome = PaneOutcome::default();
        egui::__run_test_ui(|ui| {
            let menu = PaneMenu {
                grid,
                at: CellPos::new(0, 10),
                selection: state.selection.as_ref(),
                context,
                shortcuts,
                links: None,
            };
            let chosen = Chosen {
                selected: menu.selected_text(),
                link: menu.link(),
            };
            outcome = PaneOutcome::default();
            // Not focused, which is the case a menu is opened in: the pane the user
            // right-clicked need not be the focused one.
            perform(ui, command, grid, state, chosen, false, &mut outcome);
        });
        outcome
    }

    /// Paste never reads the clipboard: it asks the platform, which hands the text back
    /// as an input event on a later frame. That is the same path Cmd+V takes, and it is
    /// the only path there is.
    #[test]
    fn pasting_asks_the_platform_rather_than_reading_the_clipboard_itself() {
        let ctx = egui::Context::default();
        let output = ctx.run_ui(egui::RawInput::default(), |ui| {
            request_clipboard_paste(ui.ctx());
        });
        let commands: Vec<&egui::ViewportCommand> = output
            .viewport_output
            .values()
            .flat_map(|viewport| viewport.commands.iter())
            .collect();
        assert!(
            commands
                .iter()
                .any(|command| matches!(command, egui::ViewportCommand::RequestPaste)),
            "the pane must ask the window system for the clipboard: {commands:?}"
        );
    }

    /// The pane's accessible name has to carry what the selection is doing: the highlight
    /// is the only feedback a pointer gives, and a screen reader cannot see it.
    #[test]
    fn the_accessible_name_says_what_is_selected_and_where_the_keyboard_is() {
        let mut grid = Grid::from_lines(&["alpha beta"], 12);
        grid.cursor = Some((0, 0));
        let mut state = PaneInteraction::default();
        let quiet = pane_label(&grid, &state, PaneOptions::default());
        assert!(!quiet.contains("selection"), "got {quiet}");

        state.select_word(&grid, CellPos::new(0, 1), SelectionKind::Linear);
        let selected = pane_label(&grid, &state, PaneOptions::default());
        assert!(
            selected.contains("selection of 5 characters"),
            "got {selected}"
        );
        assert!(
            !selected.contains("lines"),
            "one line is not worth saying: {selected}"
        );

        state.selection = None;
        state.begin_drag(&grid, CellPos::new(0, 0), SelectionKind::Block, false);
        state.drag_to(&grid, CellPos::new(0, 5));
        assert!(
            pane_label(&grid, &state, PaneOptions::default()).contains("rectangular"),
            "a rectangular selection reads differently, because it behaves differently"
        );

        state.enter_selection_mode(&grid);
        let keyboard = pane_label(&grid, &state, PaneOptions::default());
        assert!(
            keyboard.contains("selecting with the keyboard, row 1 column 1"),
            "got {keyboard}"
        );
    }
    // ---------------------------------------------------------------------------------
    // Links. The gesture, the caching, and the rule that the selection wins the drag.
    // ---------------------------------------------------------------------------------

    /// The rule that keeps a link out of the way of the selection: only a modifier-click
    /// follows one, so every drag — with the modifier or without it — still selects text.
    #[test]
    fn only_a_modifier_click_follows_a_link_so_a_drag_over_one_still_selects() {
        assert!(follows_link(true, false, true), "the gesture that opens");
        assert!(
            !follows_link(true, false, false),
            "a plain click in text places a selection and nothing else"
        );
        assert!(
            !follows_link(true, true, true),
            "a drag is a selection even with the modifier held"
        );
        assert!(!follows_link(false, true, true));
        assert!(!follows_link(false, false, true));
    }

    /// The words the hover uses have to be words: the bundled proportional face cannot place
    /// the Mac modifier glyphs, and a hint that renders as a hollow box is worse than none.
    #[test]
    fn the_gesture_is_spelled_out_rather_than_drawn_with_a_glyph_the_font_lacks() {
        let label = follow_modifier_label();
        assert!(label.contains("click to open"), "got {label}");
        for glyph in crate::keymap::GLYPHS_THE_BUNDLED_FONTS_CANNOT_PLACE {
            assert!(!label.contains(*glyph), "{label} contains {glyph}");
        }
    }

    /// Hovering answers with the link under the cell, and keeps the moment the pointer
    /// arrived on it so the target is not shown the instant somebody crosses a URL.
    #[test]
    fn hovering_finds_the_link_under_the_cell_and_remembers_when_it_arrived() {
        let grid = Grid::from_lines(&["see https://example.com/a now"], 30);
        let mut state = PaneInteraction::default();

        assert_eq!(
            state.hover_link(&grid, CellPos::new(0, 1), 1_000),
            None,
            "`see` is not a link"
        );
        assert!(state.hovered_link().is_none());

        let since = state
            .hover_link(&grid, CellPos::new(0, 6), 1_000)
            .expect("the URL is a link");
        assert_eq!(since, 1_000);
        assert_eq!(
            state.hovered_link().map(|link| link.target.display()),
            Some("https://example.com/a".to_string())
        );

        // Moving along the same link does not restart the wait.
        assert_eq!(
            state.hover_link(&grid, CellPos::new(0, 12), 1_200),
            Some(1_000)
        );
        // And leaving it gives up the hover rather than holding a stale one.
        assert_eq!(state.hover_link(&grid, CellPos::new(0, 27), 1_300), None);
        assert!(state.hovered_link().is_none());
    }

    /// Finding links resolves paths against the filesystem, so the map is built once per
    /// change of the text and not once per frame.
    #[test]
    fn the_link_map_is_rebuilt_when_the_text_changes_and_not_when_the_cursor_blinks() {
        let grid = Grid::from_lines(&["https://example.com/a"], 24);
        let mut state = PaneInteraction::default();
        assert!(state.hover_link(&grid, CellPos::new(0, 4), 0).is_some());
        let built = state.link_map_for;
        assert!(built.is_some());

        // A cursor move and a scroll position change nothing about the text.
        let mut blinked = grid.clone();
        blinked.cursor = Some((0, 9));
        blinked.scrollback_offset = 12;
        assert!(state
            .hover_link(&blinked, CellPos::new(0, 4), 500)
            .is_some());
        assert_eq!(state.link_map_for, built, "the map was not rebuilt");

        // A colour does not either.
        let mut recoloured = grid.clone();
        if let Some(cell) = recoloured.cell_mut(0, 0) {
            cell.fg = Some(Rgb::new(200, 40, 40));
        }
        assert!(state
            .hover_link(&recoloured, CellPos::new(0, 4), 600)
            .is_some());
        assert_eq!(state.link_map_for, built);

        // Different text does.
        let moved = Grid::from_lines(&["https://example.com/b"], 24);
        assert!(state.hover_link(&moved, CellPos::new(0, 4), 700).is_some());
        assert_ne!(state.link_map_for, built);
        assert_eq!(
            state.hovered_link().map(|link| link.target.display()),
            Some("https://example.com/b".to_string())
        );

        // And so does a hyperlink appearing over text that did not change.
        let mut declared = moved.clone();
        assert!(declared.set_row_meta(
            0,
            turn_proto::cells::RowMeta {
                wrapped: false,
                links: vec![turn_proto::cells::RowLink::new(0, 21, "ssh://elsewhere")],
            }
        ));
        let before = state.link_map_for;
        assert!(state
            .hover_link(&declared, CellPos::new(0, 4), 800)
            .is_some());
        assert_ne!(state.link_map_for, before);
        assert_eq!(
            state.hovered_link().map(|link| link.target.display()),
            Some("ssh://elsewhere".to_string())
        );
    }

    /// Pointing this pane at a different directory means the same relative path is a
    /// different file, so the answers from the old one must not survive.
    #[test]
    fn changing_the_working_directory_forgets_the_links_it_resolved() {
        let grid = Grid::from_lines(&["Cargo.toml and https://example.com/a"], 40);
        let mut state = PaneInteraction::default();
        state.set_cwd(Some(PathBuf::from(env!("CARGO_MANIFEST_DIR"))));
        assert!(state.hover_link(&grid, CellPos::new(0, 20), 0).is_some());
        assert_eq!(
            state.links().len(),
            2,
            "the URL and the file that is really there: {:?}",
            state.links().links()
        );
        let built = state.link_map_for;

        state.set_cwd(Some(PathBuf::from("/")));
        assert_eq!(state.link_map_for, None, "the map was thrown away");
        assert!(state.hover_link(&grid, CellPos::new(0, 20), 0).is_some());
        // The fingerprint comes back the same, because the *text* did not change — which is
        // exactly why the directory has to invalidate the map itself rather than relying on
        // the text to do it.
        assert_eq!(state.link_map_for, built);
        assert_eq!(
            state.links().len(),
            1,
            "`Cargo.toml` is not in the new directory, so it is no longer a link"
        );

        // Setting the same directory again is not a change and does not throw the map away.
        let settled = state.link_map_for;
        state.set_cwd(Some(PathBuf::from("/")));
        assert_eq!(state.link_map_for, settled);
    }

    /// The window takes the request once. A request left behind would be opened again on
    /// every frame until the pointer moved.
    #[test]
    fn a_followed_link_is_handed_to_the_window_exactly_once() {
        let grid = Grid::from_lines(&["https://example.com/a"], 24);
        let mut state = PaneInteraction::default();
        assert!(state.hover_link(&grid, CellPos::new(0, 4), 0).is_some());
        state.pending_link = state.hovered_link().map(Link::request);

        let request = state.take_link_request().expect("a request");
        assert_eq!(request.display, "https://example.com/a");
        assert!(!request.needs_confirmation());
        assert!(
            state.take_link_request().is_none(),
            "the request must not be handed over twice"
        );
    }
}
