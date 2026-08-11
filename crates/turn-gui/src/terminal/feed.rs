//! One pane's screen, kept up to date, plus the history the window has watched
//! scroll past.
//!
//! The window renders grids and nothing else. The daemon parses the pty once and
//! sends [`ScreenUpdate`]s, so there is no VT emulator on this side of the socket and
//! no second reading of the stream to disagree with the first. This module owns the
//! client's copy of a pane's screen and the two things a copy needs: applying updates
//! in order, and noticing when it has fallen out of step.
//!
//! ## Falling out of step
//!
//! `seq` increases by one per update per attachment. A jump means an update was
//! missed, and applying a row diff on top of a stale screen would leave the window and
//! the daemon disagreeing about what the user is looking at — the exact bug the cells
//! design exists to remove. So a gap is reported as a [`Desync`], nothing is applied,
//! and the caller asks for `resync_pane`. The daemon repairs its own dropped frames
//! too, by making the next update a full screen; both paths exist because they fail
//! differently.
//!
//! ## Where the scrollback lives
//!
//! **The daemon's parser owns the history.** It has to: a client is sent the *screen*, and
//! a pane that printed five hundred lines between two coalesced updates never sent the four
//! hundred and eighty in the middle. A window that reconstructed history only from the
//! frames it happened to watch would have gaps exactly where a build log is most
//! interesting, and nothing at all for output that arrived before the window started. So
//! history is read with [`Request::GetPaneHistory`](turn_proto::Request::GetPaneHistory),
//! and what this module keeps is a **cache** of rows the daemon still has. `attach_pane`
//! also seeds that cache from the bounded durable scrollback checkpoint, so output from
//! before a daemon restart is visible immediately and later windows remain fetchable.
//!
//! Rows are numbered by **line index**: `0` is the oldest row the daemon holds and
//! [`PaneFeed::history_len`] is the line index of the live screen's top row. The offset the
//! user scrolls by is measured from the live screen, as the protocol's is, so the top of the
//! viewport is always `history_len - offset`.
//!
//! ## Why new output does not move the view
//!
//! Because the offset is measured from the live screen, leaving it alone while rows scroll
//! off would show *newer* content: the user's place would slide out from under them a row at
//! a time, which is the behaviour that makes people distrust a terminal. Every update
//! carries `scrollback_len`, and its increase is exactly how many rows left the top, so a
//! scrolled viewport moves its offset by that much and the line at the top of the screen
//! does not change. Where the daemon reports nothing — an older daemon — the shift is taken
//! from the proof in the screens themselves ([`exact_shift`]), which covers the ordinary
//! case of output arriving a line at a time.
//!
//! ## What is bounded
//!
//! Rows are cached in their **run-encoded** form rather than as cells: a blank row is one
//! run and a line of a build log is three or four, and the difference between forty bytes a
//! row and five kilobytes a row decides whether thirty attached panes cost thirty megabytes
//! or seven hundred. At most [`MAX_HISTORY_ROWS`] rows are held per pane, oldest dropped
//! first — and a dropped row is not lost, because scrolling back to it fetches it again.

use std::collections::BTreeMap;

use turn_proto::cells::{decode_runs, Cell, CellRun, Grid, RowMeta};
use turn_proto::{PaneAttachment, PtySize, ScreenUpdate};

/// Why a feed can no longer be trusted and has to be resynchronised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Desync {
    /// Updates were missed. The numbers are worth logging: one is a busy moment,
    /// forty is a client that cannot keep up.
    Missed { expected: u64, got: u64 },
    /// A row update arrived for a geometry this feed is not rendering, which means a
    /// resize was missed.
    WrongSize { have: PtySize, update: PtySize },
    /// The update was structurally impossible — a run that does not account for its
    /// cells, a row outside the screen. Reported rather than repaired, because a
    /// protocol that quietly fixes its own input is one whose two implementations will
    /// eventually disagree.
    Malformed(String),
}

/// How many rows of history one pane caches in the window.
///
/// The same depth as the daemon's own scrollback, so scrolling through a pane the window has
/// been watching costs no round trips at all. Kept as runs rather than cells, which is what
/// makes that affordable: five thousand rows of a build log is around a megabyte a pane
/// instead of twenty-four.
pub const MAX_HISTORY_ROWS: usize = 5_000;

/// One row of history.
///
/// The row's metadata travels with its cells because both halves are needed to read the
/// row back: whether the terminal broke it at the margin decides whether a selection
/// spanning it is one line or two, and its OSC 8 spans decide what is a link. A history
/// that kept only the cells would turn every scrolled-back wrapped line into two lines the
/// moment the user tried to copy it.
///
/// The cells are held in the grid's own run encoding — the same form the row arrived in —
/// and expanded when a viewport actually needs them.
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryRow {
    pub runs: Vec<CellRun>,
    pub meta: RowMeta,
    /// The inline-image placements this row's own cells refer to.
    ///
    /// Carried per row rather than per window, because a scrolled view is assembled from rows
    /// of several windows *and* of the live screen — and a slot number means nothing outside
    /// the grid it came from. Eight slots are reused as pictures scroll past, so a row filed
    /// an hour ago whose markers say "slot 0" refers to a picture that is not the one slot 0
    /// holds now. Keeping the placement beside the row is what makes scrolling back show the
    /// picture that was there rather than the picture that is there.
    ///
    /// Filtered to the slots the row actually uses, which for all but a handful of rows is
    /// none — so it costs an empty `Vec` per row and nothing else.
    pub images: Vec<turn_proto::images::GridImage>,
}

impl HistoryRow {
    /// One row of a grid, taken whole.
    fn of(grid: &Grid, row: u16) -> Self {
        Self {
            runs: grid.row_runs(row),
            meta: grid.row_meta(row).clone(),
            images: images_used_by(grid, row),
        }
    }

    /// The row's cells, or `None` when the runs cannot be expanded to this width — which
    /// means the row was captured at another geometry and must not be painted.
    fn cells(&self, cols: u16) -> Option<Vec<Cell>> {
        decode_runs(&self.runs, cols, 0).ok()
    }

    /// One decoded durable row, converted back to the cache's compact representation.
    fn from_cells(cells: Vec<Cell>, cols: u16) -> Self {
        let mut grid = Grid::blank(1, cols);
        for (col, cell) in cells.into_iter().enumerate().take(cols as usize) {
            if let Some(target) = grid.cell_mut(0, col as u16) {
                *target = cell;
            }
        }
        Self::of(&grid, 0)
    }
}

/// The placements a row's image cells refer to, by the slots that row uses.
fn images_used_by(grid: &Grid, row: u16) -> Vec<turn_proto::images::GridImage> {
    let mut used: Vec<turn_proto::images::GridImage> = Vec::new();
    for col in 0..grid.cols {
        let Some(tile) = grid.cell(row, col).and_then(Cell::image_tile) else {
            continue;
        };
        if used.iter().any(|placed| placed.slot == tile.slot) {
            continue;
        }
        if let Some(placed) = grid.image_in_slot(tile.slot) {
            used.push(*placed);
        }
    }
    used
}

/// Rewrites a row's image markers to the view's own slot numbering.
///
/// A scrolled view is one grid, and a grid has [`turn_proto::MAX_PLACED_IMAGES`] slots keyed
/// by number — so rows that came from different grids cannot keep their original slots
/// without two pictures claiming the same one. Each distinct picture in the view is given a
/// slot of its own here, and `table` accumulates them.
///
/// A cell whose placement is unknown, or which arrives after the view's slots are full,
/// becomes blank rather than keeping a marker that would resolve to somebody else's picture.
/// Blank is the honest outcome: drawing the wrong picture is worse than drawing none.
fn remap_images(
    cells: &mut [Cell],
    source: &[turn_proto::images::GridImage],
    table: &mut Vec<turn_proto::images::GridImage>,
) {
    for cell in cells.iter_mut() {
        let Some(tile) = cell.image_tile() else {
            continue;
        };
        let Some(placed) = source.iter().find(|placed| placed.slot == tile.slot) else {
            *cell = Cell::blank();
            continue;
        };
        let existing = table.iter().position(|held| {
            held.id == placed.id && held.rows == placed.rows && held.cols == placed.cols
        });
        let slot = match existing {
            Some(index) => index,
            None if table.len() < turn_proto::MAX_PLACED_IMAGES => {
                table.push(turn_proto::images::GridImage {
                    slot: table.len() as u8,
                    ..*placed
                });
                table.len() - 1
            }
            None => {
                *cell = Cell::blank();
                continue;
            }
        };
        match Cell::image(turn_proto::images::ImageCell::new(
            slot as u8, tile.dy, tile.dx,
        )) {
            Some(replacement) => *cell = replacement,
            None => *cell = Cell::blank(),
        }
    }
}

/// The rows of history this window is holding, by line index.
///
/// A cache rather than a record: the daemon has these rows whether or not this map does, so
/// dropping the oldest is a decision about memory and not a loss of history.
#[derive(Debug, Default)]
pub struct History {
    rows: BTreeMap<usize, HistoryRow>,
    /// The width the cached rows were captured at. A row of the old width cannot be painted
    /// into a grid of the new one.
    cols: u16,
}

impl History {
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// One row, if this window has it.
    pub fn row(&self, line: usize) -> Option<&HistoryRow> {
        self.rows.get(&line)
    }

    /// Whether every line in `range` is held, which is what decides whether a viewport can
    /// be drawn without asking the daemon for anything.
    pub fn holds_all(&self, range: std::ops::Range<usize>) -> bool {
        range.clone().count() == self.rows.range(range).count()
    }

    fn insert(&mut self, line: usize, row: HistoryRow, cols: u16) {
        if cols != self.cols {
            // A resize invalidates every cached row at once: keeping them would mean
            // painting rows of the wrong width into the viewport.
            self.rows.clear();
            self.cols = cols;
        }
        self.rows.insert(line, row);
        while self.rows.len() > MAX_HISTORY_ROWS {
            // The oldest line goes. It is the furthest from where anybody is looking, and
            // the daemon still has it.
            let Some(oldest) = self.rows.keys().next().copied() else {
                break;
            };
            self.rows.remove(&oldest);
        }
    }

    fn clear(&mut self) {
        self.rows.clear();
    }

    /// Forgets lines at or after `line`, for the case where the daemon's record turns out
    /// to be shorter than this cache assumed and a row would be attributed to the wrong
    /// output.
    fn forget_from(&mut self, line: usize) {
        let doomed: Vec<usize> = self.rows.range(line..).map(|(key, _)| *key).collect();
        for key in doomed {
            self.rows.remove(&key);
        }
    }
}

/// A pane's screen, the history behind it, and where the user is looking.
pub struct PaneFeed {
    screen: Grid,
    history: History,
    /// How far back the viewport is, in rows. Zero is the live screen.
    offset: usize,
    /// Line index of the live screen's top row: how much history exists, as the daemon
    /// reports it.
    history_len: usize,
    /// The line index below which Turn no longer offers history, because the user asked for
    /// it to be cleared. Never a claim that the rows are gone from the daemon — only that
    /// this window has stopped showing them.
    floor: usize,
    /// The next `seq` this feed expects.
    next_seq: u64,
    /// Whether the daemon dropped output before it started keeping this pane's
    /// screen. The screen is correct; what is above it is incomplete.
    daemon_truncated: bool,
    /// Whether anything was discarded at the user's request, so the record can admit it no
    /// longer reaches back to the attach.
    cleared: bool,
    /// A history window the view needs and does not hold, waiting to be asked for.
    wanted: Option<usize>,
    /// Whether a request for history is outstanding, so only one is in flight per pane.
    fetching: bool,
    /// Rebuilt only when the viewport moves or the screen changes, because a scrolled
    /// view is assembled from the history and that is not free.
    view: Option<Grid>,
}

impl PaneFeed {
    /// Starts a feed from an attachment.
    ///
    /// A cells attachment carries the screen the daemon has been keeping, which is
    /// what makes "processes survive UI restarts" visible rather than claimed. A pane
    /// with no screen — one with no process behind it — starts blank at the agreed
    /// size.
    ///
    /// The history behind that screen is reachable from this moment, because it is the
    /// daemon's rather than something this window had to watch happen.
    pub fn attach(attachment: &PaneAttachment) -> Self {
        let screen = match &attachment.screen {
            Some(grid) => (**grid).clone(),
            None => Grid::blank(attachment.size.rows, attachment.size.cols),
        };
        let decoded =
            if attachment.scrollback.is_empty() || attachment.scrollback.cols() == screen.cols {
                attachment.scrollback.decode_rows().ok()
            } else {
                None
            };
        let malformed_history = decoded.is_none() && !attachment.scrollback.is_empty();
        let decoded = decoded.unwrap_or_default();
        let history_len = screen.scrollback_len.max(decoded.len());
        let first_line = history_len.saturating_sub(decoded.len());
        let mut history = History::default();
        for (offset, cells) in decoded.into_iter().enumerate() {
            history.insert(
                first_line + offset,
                HistoryRow::from_cells(cells, screen.cols),
                screen.cols,
            );
        }
        PaneFeed {
            screen,
            history,
            offset: 0,
            history_len,
            floor: 0,
            next_seq: attachment.next_seq,
            daemon_truncated: attachment.scrollback_truncated || malformed_history,
            cleared: false,
            wanted: None,
            fetching: false,
            view: None,
        }
    }

    /// A feed for a pane the window has not attached to yet.
    pub fn blank(size: PtySize) -> Self {
        PaneFeed {
            screen: Grid::blank(size.rows, size.cols),
            history: History::default(),
            offset: 0,
            history_len: 0,
            floor: 0,
            next_seq: 0,
            daemon_truncated: false,
            cleared: false,
            wanted: None,
            fetching: false,
            view: None,
        }
    }

    /// Applies an update, in sequence.
    pub fn apply(&mut self, seq: u64, update: &ScreenUpdate) -> Result<(), Desync> {
        if seq < self.next_seq {
            // A straggler from a previous attachment. The screen already reflects it,
            // so dropping it is right rather than a desync.
            return Ok(());
        }
        if seq != self.next_seq {
            return Err(Desync::Missed {
                expected: self.next_seq,
                got: seq,
            });
        }
        if let ScreenUpdate::Rows { size, .. } = update {
            if size.rows != self.screen.rows || size.cols != self.screen.cols {
                return Err(Desync::WrongSize {
                    have: PtySize::new(self.screen.rows, self.screen.cols),
                    update: *size,
                });
            }
        }

        // One snapshot, not two. This is the hot path for a noisy terminal: keep
        // the previous screen for history/recovery and apply the validated diff in
        // place. On malformed input the previous clone restores atomicity.
        let previous = self.screen.clone();
        if let Err(error) = update.apply(&mut self.screen) {
            self.screen = previous;
            return Err(Desync::Malformed(error.to_string()));
        }

        self.absorb(&previous);
        self.next_seq = seq.saturating_add(1);
        self.view = None;
        Ok(())
    }

    /// Caches what left the top of the screen, and keeps the viewport where it was.
    fn absorb(&mut self, previous: &Grid) {
        let next = &self.screen;
        if next.alternate_screen || previous.alternate_screen {
            // A full-screen program owns its viewport. Its redraws are not history, and
            // Turn stands down entirely: no scrolling, no recording. The daemon reports no
            // history at all while the alternate screen is in front, because the alternate
            // grid has none of its own.
            self.offset = 0;
            self.history_len = next.scrollback_len;
            self.floor = self.floor.min(self.history_len);
            return;
        }
        if !previous.same_size(next) {
            return;
        }
        let shift = self.shift_between(previous, next);
        for row in 0..shift.min(usize::from(previous.rows)) {
            let line = self.history_len + row;
            self.history
                .insert(line, HistoryRow::of(previous, row as u16), previous.cols);
        }
        let reported = next.scrollback_len;
        let grown = self.history_len.saturating_add(shift);
        self.history_len = reported.max(grown);
        if reported > 0 && reported < grown {
            // The record is shorter than this cache assumed, which happens when the
            // daemon's ring drops rows. Cached lines past the end would be attributed to
            // the wrong output, so they go.
            self.history.forget_from(reported);
        }
        self.floor = self.floor.min(self.history_len);
        if self.offset > 0 && shift > 0 {
            // The offset is measured from the live screen, so it has to grow by exactly
            // what scrolled off, or the line the user is reading slides away.
            self.offset = (self.offset + shift).min(self.history_rows());
            self.view = None;
            self.note_missing_rows();
        }
    }

    /// How many rows left the top of the screen.
    ///
    /// Exact when the daemon says how much history it holds: the increase *is* the number
    /// of rows that scrolled off, however many screens' worth arrived at once. The proof in
    /// the screens themselves is the fallback, and it can only see a shift smaller than the
    /// screen — which is the ordinary case of output arriving a line at a time.
    fn shift_between(&self, previous: &Grid, next: &Grid) -> usize {
        let reported = next.scrollback_len;
        if reported > self.history_len {
            return reported - self.history_len;
        }
        if reported == 0 {
            return exact_shift(previous, next).unwrap_or(0);
        }
        0
    }

    /// Replaces the screen wholesale, after a resync.
    ///
    /// The cache is kept when the size is unchanged: those rows are still the same rows, and
    /// discarding them because a resync happened would cost a round trip to fetch them back.
    /// A resize is different — a row of the old width has no place in a grid of the new one —
    /// and clears it.
    pub fn resync(&mut self, grid: Grid, next_seq: u64) {
        if !grid.same_size(&self.screen) {
            self.history.clear();
            self.offset = 0;
            self.floor = 0;
        }
        if grid.scrollback_len > 0 || self.history_len == 0 {
            self.history_len = grid.scrollback_len;
        }
        self.floor = self.floor.min(self.history_len);
        self.offset = self.offset.min(self.history_rows());
        self.screen = grid;
        self.next_seq = next_seq;
        self.view = None;
    }

    /// The sequence number the next update is expected to carry.
    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }

    pub fn size(&self) -> PtySize {
        PtySize::new(self.screen.rows, self.screen.cols)
    }

    /// Whether Turn's record of this pane reaches back to the start of the process.
    ///
    /// False once the daemon has dropped output, or once the user has cleared what Turn was
    /// showing. Either way the pane says so rather than letting somebody scroll to the top
    /// and read it as the beginning.
    pub fn history_complete(&self) -> bool {
        !self.daemon_truncated && !self.cleared
    }

    /// How many rows of history can be scrolled into.
    ///
    /// What the daemon holds, less anything the user asked Turn to stop showing.
    pub fn history_rows(&self) -> usize {
        self.history_len.saturating_sub(self.floor)
    }

    /// The line index of the live screen's top row: how much history the daemon reports.
    pub fn history_len(&self) -> usize {
        self.history_len
    }

    /// The oldest line index Turn will show, which is not zero once history was cleared.
    pub fn oldest_line(&self) -> usize {
        self.floor
    }

    /// How many rows of it this window is holding, so a caller can tell a cache miss from
    /// an empty record.
    pub fn cached_rows(&self) -> usize {
        self.history.len()
    }

    /// Where the viewport is, in rows above the live screen.
    pub fn offset(&self) -> usize {
        self.offset
    }

    /// Whether the user is looking at history rather than at the live screen.
    pub fn is_scrolled(&self) -> bool {
        self.offset > 0
    }

    /// Scrolls the viewport, and reports whether it moved.
    ///
    /// Positive is backwards into history, which is what a wheel-up does. Clamped to the
    /// history the *daemon* holds rather than to what this window has cached, so scrolling is
    /// never limited by what happens to be in memory here: a window that has just attached
    /// can scroll straight into output it never watched. A very large number is therefore
    /// also how "go to the top" is expressed, and the clamp does the rest.
    ///
    /// Refused entirely in the alternate screen: a full-screen program owns its viewport, and
    /// scrolling Turn's record out from under `lazygit` would show the user a screen that no
    /// longer exists.
    pub fn scroll_by(&mut self, rows: i32) -> bool {
        if self.screen.alternate_screen {
            return false;
        }
        let wanted = (self.offset as i64 + i64::from(rows)).max(0) as usize;
        self.scroll_to(wanted)
    }

    /// Scrolls to an exact offset, clamped, and reports whether it moved.
    pub fn scroll_to(&mut self, offset: usize) -> bool {
        if self.screen.alternate_screen {
            return false;
        }
        let clamped = offset.min(self.history_rows());
        if clamped == self.offset {
            return false;
        }
        self.offset = clamped;
        self.view = None;
        self.note_missing_rows();
        true
    }

    /// Notes that the viewport needs rows this window does not hold.
    ///
    /// Decided when the viewport moves rather than while painting, so that a caller can drain
    /// the request without having had to draw a frame first — and so the decision is one
    /// place, tested, rather than a side effect of building a view.
    fn note_missing_rows(&mut self) {
        if self.offset == 0 {
            return;
        }
        let top = self.history_len.saturating_sub(self.offset);
        let end = (top + usize::from(self.screen.rows)).min(self.history_len);
        if end > top && !self.history.holds_all(top..end) {
            self.wanted = Some(self.offset);
        }
    }

    /// A page is a screen less one row, so the line the user was reading stays visible.
    pub fn page(&self) -> i32 {
        i32::from(self.screen.rows.saturating_sub(1).max(1))
    }

    pub fn page_up(&mut self) -> bool {
        let page = self.page();
        self.scroll_by(page)
    }

    pub fn page_down(&mut self) -> bool {
        let page = self.page();
        self.scroll_by(-page)
    }

    /// The oldest row Turn will show.
    pub fn scroll_to_top(&mut self) -> bool {
        let top = self.history_rows();
        self.scroll_to(top)
    }

    /// Returns to the live screen. What any keystroke does in every terminal.
    pub fn scroll_to_bottom(&mut self) -> bool {
        self.scroll_to(0)
    }

    /// Moves the viewport so that `line` is visible, roughly centred, and reports the offset
    /// it settled on.
    ///
    /// This is how a search result is shown: the daemon reports matches as line indices, and
    /// the arithmetic that turns one into a viewport lives in the protocol so that both ends
    /// agree about where a match is.
    pub fn reveal_line(&mut self, line: usize) -> usize {
        let offset = turn_proto::viewport_offset(line, self.screen.rows, self.history_len);
        self.scroll_to(offset);
        self.offset
    }

    /// The grid to paint: the live screen, or a view assembled from history.
    pub fn grid(&mut self) -> &Grid {
        if self.view.is_none() {
            let view = self.build_view();
            self.view = Some(view);
        }
        // Assigned above when absent, so the fallback is unreachable. Written as a
        // fallback rather than an unwrap because a panic inside a draw loop takes the
        // whole window with it.
        self.view
            .get_or_insert_with(|| Grid::blank(self.screen.rows, self.screen.cols))
    }

    /// The screen built by the last call to [`PaneFeed::grid`], without building one.
    ///
    /// Exists so a caller can build every visible screen behind a mutable borrow and
    /// then hand out shared references to all of them — which is what lets the view
    /// borrow grids rather than clone one per pane per frame.
    pub fn peek(&self) -> Option<&Grid> {
        self.view.as_ref()
    }

    /// The offset of a history window this feed needs and does not hold.
    ///
    /// Taken by the window, which turns it into
    /// [`Request::GetPaneHistory`](turn_proto::Request::GetPaneHistory). `None` while a
    /// request is already outstanding, so a flick of the wheel cannot put fifty of them on
    /// the socket.
    pub fn take_history_request(&mut self) -> Option<usize> {
        if self.fetching {
            return None;
        }
        let offset = self.wanted.take()?;
        self.fetching = true;
        Some(offset)
    }

    /// Files the rows of a history window the daemon sent.
    ///
    /// The window says which offset it is and how deep the record goes, so a client that
    /// asked for more than exists still files the rows against the right lines.
    pub fn receive_history(&mut self, window: &Grid) {
        self.fetching = false;
        if window.cols != self.screen.cols {
            // A window of the wrong width arrived across a resize. Nothing can be done with
            // it, and keeping it would paint rows that do not fit.
            return;
        }
        if window.scrollback_len > 0 || self.history_len == 0 {
            self.history_len = window.scrollback_len;
        }
        let Some(first) = window.scrollback_len.checked_sub(window.scrollback_offset) else {
            return;
        };
        for row in 0..window.rows {
            let line = first + usize::from(row);
            if line >= self.history_len {
                break;
            }
            self.history
                .insert(line, HistoryRow::of(window, row), window.cols);
        }
        self.offset = self.offset.min(self.history_rows());
        self.view = None;
        // A window that did not cover the whole viewport — the record grew while it was in
        // flight — leaves the rest to ask for again.
        self.note_missing_rows();
    }

    /// Whether a fetch is outstanding, so a pane can say it is still loading rather than
    /// showing blank rows as though the output had been empty.
    pub fn is_fetching(&self) -> bool {
        self.fetching
    }

    /// Builds the scrolled viewport: history rows down to the top of the live screen.
    fn build_view(&self) -> Grid {
        let mut grid = self.screen.clone();
        grid.scrollback_offset = self.offset;
        grid.scrollback_len = self.history_rows();
        if self.offset == 0 {
            return grid;
        }
        let rows = usize::from(self.screen.rows);
        let top = self.history_len - self.offset;
        let blank = vec![Cell::blank(); usize::from(grid.cols)];
        // The view's own image table, filled as rows that carry pictures are assembled. Every
        // row came from a different grid, so the slot numbers have to be reassigned — see
        // [`remap_images`].
        let mut images: Vec<turn_proto::images::GridImage> = Vec::new();
        for target in 0..rows {
            let line = top + target;
            let row = if line < self.history_len {
                self.history.row(line).and_then(|row| {
                    row.cells(self.screen.cols).map(|mut cells| {
                        remap_images(&mut cells, &row.images, &mut images);
                        (cells, row.meta.clone())
                    })
                })
            } else {
                let live = (line - self.history_len) as u16;
                let mut cells = self.screen.row(live).to_vec();
                remap_images(&mut cells, &self.screen.images, &mut images);
                Some((cells, self.screen.row_meta(live).clone()))
            };
            // The metadata is written as well as the cells: every row of a scrolled view
            // comes from somewhere other than the row it lands on, so a wrap flag left
            // where it was would say a line continues into text that is not its own.
            match row {
                Some((cells, meta)) => {
                    grid.set_row(target as u16, &cells);
                    grid.set_row_meta(target as u16, meta);
                }
                // A row this window has not been given yet is left blank rather than filled
                // with something plausible. It is a gap for one frame, and asking for it is
                // what happens next.
                None => {
                    grid.set_row(target as u16, &blank);
                    grid.set_row_meta(target as u16, RowMeta::default());
                }
            }
        }
        // A scrolled view has no cursor: the cursor is on the live screen, and drawing
        // it at the same coordinates in a historical view would put it on an unrelated
        // character.
        grid.cursor = None;
        grid.images = images;
        grid
    }

    /// Stops showing the history Turn had for this pane.
    ///
    /// What "Clear Buffer" means here, and the whole of what it means: the *screen* belongs
    /// to the program, and clearing that would mean writing bytes into the pty — typing at
    /// whatever is running — which is not something a menu item may do behind the user's
    /// back. What Turn can do is stop offering the scrollback, and it does so from the
    /// current end of the record: nothing above this point is shown again, searched again or
    /// scrolled into, and the pane admits its record no longer reaches back.
    ///
    /// The daemon's own parser still ages its ring out on its own schedule. Turn does not
    /// pretend to have erased it — only to have stopped looking at it.
    pub fn clear_history(&mut self) {
        let discarded = self.history_rows() > 0;
        self.history.clear();
        self.floor = self.history_len;
        self.cleared |= discarded;
        self.offset = 0;
        self.wanted = None;
        self.view = None;
    }
}

/// How many rows the screen scrolled up by, when that alone explains the change.
///
/// `None` when no shift does, which is the common case for a pane that repainted. A
/// shift is only believed when there is overlap to check it against: a shift equal to
/// the height of the screen would "explain" any change at all, which is why the search
/// stops one short of it.
fn exact_shift(previous: &Grid, next: &Grid) -> Option<usize> {
    let rows = previous.rows as usize;
    for shift in 1..rows {
        let overlap = rows - shift;
        let matches =
            (0..overlap).all(|row| previous.row((row + shift) as u16) == next.row(row as u16));
        if matches {
            return Some(shift);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use turn_core::ids::{PaneId, SessionId};
    use turn_proto::cells::CellRun;
    use turn_proto::{GridRow, PaneStream, TerminalBytes};

    fn attachment(screen: Option<Grid>, next_seq: u64) -> PaneAttachment {
        let size = screen
            .as_ref()
            .map(|g| PtySize::new(g.rows, g.cols))
            .unwrap_or(PtySize::new(6, 20));
        PaneAttachment {
            session_id: SessionId::from_stored("sess_feed0001"),
            pane_id: PaneId::new(),
            node_id: None,
            stream: PaneStream::Cells,
            screen: screen.map(Box::new),
            scrollback: turn_proto::Scrollback::default(),
            replay: TerminalBytes::new(Vec::new()),
            size,
            scrollback_truncated: false,
            bytes_seen: 0,
            next_seq,
        }
    }

    fn scrollback(lines: &[&str], cols: u16) -> turn_proto::Scrollback {
        let grid = screen(lines, lines.len().max(1) as u16, cols);
        let rows: Vec<Vec<CellRun>> = (0..lines.len())
            .map(|row| grid.row_runs(row as u16))
            .collect();
        serde_json::from_value(serde_json::json!({ "cols": cols, "rows": rows })).unwrap()
    }

    /// A screen of a fixed height, padded like a real terminal's.
    fn screen(lines: &[&str], rows: u16, cols: u16) -> Grid {
        let mut grid = Grid::blank(rows, cols);
        for (row, line) in lines.iter().enumerate().take(rows as usize) {
            for (col, ch) in line.chars().enumerate().take(cols as usize) {
                if let Some(cell) = grid.cell_mut(row as u16, col as u16) {
                    cell.text = ch.to_string();
                }
            }
        }
        grid
    }

    /// Scrolls a screen up by one row and writes a new bottom line, which is what a
    /// program printing a line at the bottom of the screen does. The daemon's history grows
    /// by one row, and it says so.
    fn scrolled(grid: &Grid, new_bottom: &str) -> Grid {
        let mut next = Grid::blank(grid.rows, grid.cols);
        for row in 1..grid.rows {
            next.set_row(row - 1, grid.row(row));
            next.set_row_meta(row - 1, grid.row_meta(row).clone());
        }
        let last = grid.rows - 1;
        for (col, ch) in new_bottom.chars().enumerate().take(grid.cols as usize) {
            if let Some(cell) = next.cell_mut(last, col as u16) {
                cell.text = ch.to_string();
            }
        }
        next.scrollback_len = grid.scrollback_len + 1;
        next
    }

    /// A screen-shaped window of history, as the daemon serves one.
    fn history_window(lines: &[&str], rows: u16, cols: u16, offset: usize, len: usize) -> Grid {
        let mut grid = screen(lines, rows, cols);
        grid.scrollback_offset = offset;
        grid.scrollback_len = len;
        grid.cursor = None;
        grid
    }

    /// A row update, built in one place so that a new field on the wire form is one edit
    /// rather than one per test.
    fn rows_update(size: PtySize, rows: Vec<GridRow>) -> ScreenUpdate {
        ScreenUpdate::Rows {
            size,
            cursor: None,
            alternate_screen: false,
            scrollback_len: 0,
            images: None,
            notices: None,
            rows,
        }
    }

    /// The feature made visible: the daemon held the pty, and attaching hands over
    /// the screen it was keeping.
    #[test]
    fn attaching_shows_the_screen_the_daemon_was_holding() {
        let held = screen(&["$ cargo test", "running 3 tests"], 6, 20);
        let mut feed = PaneFeed::attach(&attachment(Some(held), 1));
        assert_eq!(feed.grid().row_text(0), "$ cargo test");
        assert_eq!(feed.grid().row_text(1), "running 3 tests");
        assert_eq!(feed.next_seq(), 1);
        assert_eq!(feed.size(), PtySize::new(6, 20));
    }

    /// And the history behind it is reachable from the moment of the attach, because it
    /// belongs to the daemon rather than to what this window happened to watch.
    #[test]
    fn a_freshly_attached_pane_can_scroll_into_output_it_never_watched() {
        let mut held = screen(&["the live screen"], 4, 20);
        held.scrollback_len = 900;
        let mut feed = PaneFeed::attach(&attachment(Some(held), 1));

        assert_eq!(feed.history_rows(), 900);
        assert!(feed.scroll_by(500), "the daemon has those rows");
        assert_eq!(feed.offset(), 500);
        assert_eq!(
            feed.cached_rows(),
            0,
            "and this window holds none of them yet"
        );
        // The view is a gap for one frame, and asking for it is what happens next.
        assert!(feed.grid().row_text(0).is_empty());
        assert_eq!(feed.take_history_request(), Some(500));
        assert!(feed.is_fetching());
        assert_eq!(
            feed.take_history_request(),
            None,
            "one request at a time, or a flick of the wheel puts fifty on the socket"
        );

        feed.receive_history(&history_window(&["four hundred back"], 4, 20, 500, 900));
        assert!(!feed.is_fetching());
        assert_eq!(feed.grid().row_text(0), "four hundred back");
        assert_eq!(feed.cached_rows(), 4);
    }

    #[test]
    fn attaching_seeds_the_transcript_from_durable_scrollback() {
        let live = screen(&["live-1", "live-2", "live-3"], 3, 12);
        let mut attached = attachment(Some(live), 0);
        attached.scrollback = scrollback(&["old-1", "old-2"], 12);
        let mut feed = PaneFeed::attach(&attached);

        assert_eq!(feed.history_rows(), 2);
        assert!(feed.history_complete());
        assert!(feed.scroll_by(2));
        assert_eq!(
            feed.grid()
                .text()
                .lines()
                .map(str::trim_end)
                .collect::<Vec<_>>(),
            vec!["old-1", "old-2", "live-1"]
        );
    }

    #[test]
    fn a_pane_with_no_screen_yet_still_has_something_to_draw() {
        let mut feed = PaneFeed::attach(&attachment(None, 0));
        assert_eq!(feed.grid().text().trim(), "");
        assert_eq!(feed.size(), PtySize::new(6, 20));
    }

    #[test]
    fn updates_are_applied_in_sequence_and_the_screens_agree() {
        let start = screen(&["one"], 4, 12);
        let mut feed = PaneFeed::attach(&attachment(Some(start.clone()), 7));

        let mut daemon = start;
        for (seq, line) in [(7u64, "two"), (8, "three")] {
            let mut next = daemon.clone();
            for (col, ch) in line.chars().enumerate() {
                if let Some(cell) = next.cell_mut(1, col as u16) {
                    cell.text = ch.to_string();
                }
            }
            let update = ScreenUpdate::between(&daemon, &next);
            assert_eq!(feed.apply(seq, &update), Ok(()));
            daemon = next;
        }
        assert_eq!(feed.grid().row_text(1), "three");
        assert_eq!(feed.next_seq(), 9);
    }

    /// The whole point of the sequence number: applying a row diff on top of a stale
    /// screen would leave the window and the daemon disagreeing.
    #[test]
    fn a_missed_update_is_reported_and_nothing_is_applied() {
        let start = screen(&["stable"], 4, 12);
        let mut feed = PaneFeed::attach(&attachment(Some(start.clone()), 1));
        let mut next = start;
        if let Some(cell) = next.cell_mut(0, 0) {
            cell.text = "X".into();
        }
        let update = ScreenUpdate::full(next);

        assert_eq!(
            feed.apply(5, &update),
            Err(Desync::Missed {
                expected: 1,
                got: 5
            })
        );
        assert_eq!(
            feed.grid().row_text(0),
            "stable",
            "a desynced feed must not apply the update it could not place"
        );
    }

    #[test]
    fn a_straggler_from_a_previous_attachment_is_dropped_rather_than_treated_as_a_desync() {
        let mut feed = PaneFeed::attach(&attachment(Some(screen(&["current"], 4, 12)), 10));
        let update = ScreenUpdate::full(screen(&["stale"], 4, 12));
        assert_eq!(feed.apply(3, &update), Ok(()));
        assert_eq!(feed.grid().row_text(0), "current");
        assert_eq!(
            feed.next_seq(),
            10,
            "the expectation must not move backwards"
        );
    }

    /// A client that missed a resize must resync rather than write rows into the wrong
    /// shape.
    #[test]
    fn a_row_update_for_another_geometry_is_refused() {
        let mut feed = PaneFeed::attach(&attachment(Some(Grid::blank(6, 20)), 1));
        let update = rows_update(PtySize::new(40, 120), Vec::new());
        assert_eq!(
            feed.apply(1, &update),
            Err(Desync::WrongSize {
                have: PtySize::new(6, 20),
                update: PtySize::new(40, 120),
            })
        );
    }

    #[test]
    fn a_structurally_impossible_update_is_refused_rather_than_repaired() {
        let mut feed = PaneFeed::attach(&attachment(Some(Grid::blank(4, 8)), 1));
        let update = rows_update(
            PtySize::new(4, 8),
            vec![GridRow {
                row: 99,
                runs: vec![CellRun {
                    text: String::new(),
                    cells: 8,
                    fg: None,
                    bg: None,
                    attrs: Default::default(),
                }],
                wrapped: false,
                links: Vec::new(),
            }],
        );
        assert!(matches!(feed.apply(1, &update), Err(Desync::Malformed(_))));
    }

    #[test]
    fn a_resync_replaces_the_screen_and_resumes_the_sequence() {
        let mut feed = PaneFeed::attach(&attachment(Some(screen(&["old"], 4, 12)), 1));
        feed.resync(screen(&["fresh"], 4, 12), 40);
        assert_eq!(feed.grid().row_text(0), "fresh");
        assert_eq!(feed.next_seq(), 40);
        assert_eq!(
            feed.apply(40, &ScreenUpdate::full(screen(&["newer"], 4, 12))),
            Ok(()),
            "and the next update is accepted straight away"
        );
    }

    /// The rows that scroll off the live screen are cached as they go, so scrolling back
    /// through what this window has been watching costs no round trip at all.
    #[test]
    fn rows_that_scrolled_off_the_top_are_kept_so_scrolling_back_needs_no_request() {
        let start = screen(&["l0", "l1", "l2", "l3"], 4, 8);
        let mut feed = PaneFeed::attach(&attachment(Some(start.clone()), 1));

        let after = scrolled(&start, "l4");
        assert_eq!(feed.apply(1, &ScreenUpdate::full(after.clone())), Ok(()));
        assert_eq!(feed.history_rows(), 1, "one row left the top");
        assert_eq!(feed.cached_rows(), 1);

        assert_eq!(
            feed.apply(2, &ScreenUpdate::full(scrolled(&after, "l5"))),
            Ok(())
        );
        assert_eq!(feed.history_rows(), 2);

        assert!(feed.scroll_by(2));
        assert_eq!(feed.offset(), 2);
        let view = feed.grid().clone();
        assert_eq!(view.row_text(0), "l0");
        assert_eq!(view.row_text(1), "l1");
        assert_eq!(view.row_text(2), "l2", "then the top of the live screen");
        assert_eq!(
            view.cursor, None,
            "the cursor is on the live screen, not in the history"
        );
        assert_eq!(view.scrollback_offset, 2);
        assert_eq!(view.scrollback_len, 2);
        assert_eq!(
            feed.take_history_request(),
            None,
            "everything the viewport needs is already here"
        );

        assert!(feed.scroll_to_bottom());
        assert_eq!(feed.grid().row_text(0), "l2");
    }

    /// The behaviour that decides whether people trust a terminal: output arriving while
    /// the user reads history must not move what they are reading, and must not throw them
    /// back to the bottom.
    #[test]
    fn new_output_while_scrolled_back_leaves_the_line_the_user_is_reading_where_it_was() {
        let mut daemon = screen(&["l0", "l1", "l2", "l3"], 4, 8);
        let mut feed = PaneFeed::attach(&attachment(Some(daemon.clone()), 1));
        // Twenty lines of output, so there is history to be reading.
        for line in 4..24 {
            let next = scrolled(&daemon, &format!("l{line}"));
            let update = ScreenUpdate::between(&daemon, &next);
            assert_eq!(feed.apply((line - 3) as u64, &update), Ok(()));
            daemon = next;
        }
        assert_eq!(feed.history_rows(), 20);

        assert!(feed.scroll_by(12));
        let reading = feed.grid().row_text(0);
        assert_eq!(reading, "l8", "the view starts at line 8");
        let offset = feed.offset();

        // More output arrives while the user is reading.
        for line in 24..30 {
            let next = scrolled(&daemon, &format!("l{line}"));
            let update = ScreenUpdate::between(&daemon, &next);
            assert_eq!(feed.apply((line - 3) as u64, &update), Ok(()));
            daemon = next;
        }

        assert!(
            feed.is_scrolled(),
            "and the view was not yanked to the bottom"
        );
        assert_eq!(
            feed.offset(),
            offset + 6,
            "the offset grew by exactly what scrolled off"
        );
        assert_eq!(
            feed.grid().row_text(0),
            reading,
            "which is what keeps the same line at the top of the screen"
        );
    }

    /// The case a client cannot prove for itself: a burst that scrolls the screen several
    /// times over between two frames. What the daemon says about its own history is what
    /// makes this exact.
    #[test]
    fn a_burst_bigger_than_the_screen_is_absorbed_from_what_the_daemon_reports() {
        let start = screen(&["l0", "l1", "l2", "l3"], 4, 8);
        let mut feed = PaneFeed::attach(&attachment(Some(start.clone()), 1));
        let after = scrolled(&start, "l4");
        assert_eq!(feed.apply(1, &ScreenUpdate::full(after)), Ok(()));
        assert!(feed.scroll_by(1));
        assert_eq!(feed.grid().row_text(0), "l0");

        // A hundred lines at once: nothing on the new screen overlaps the old one, so no
        // shift could be proved from the screens alone.
        let mut burst = screen(&["l100", "l101", "l102", "l103"], 4, 8);
        burst.scrollback_len = 101;
        assert_eq!(feed.apply(2, &ScreenUpdate::full(burst)), Ok(()));

        assert_eq!(feed.history_rows(), 101);
        assert_eq!(
            feed.offset(),
            101,
            "the offset moved by the whole hundred, so the top line is unchanged"
        );
        assert_eq!(
            feed.grid().row_text(0),
            "l0",
            "the row the user was reading is still the row on screen"
        );
        // The rows in the middle were never on this window's screen, so they come from the
        // daemon rather than being invented.
        assert_eq!(feed.take_history_request(), None, "row l0 is cached");
        assert!(feed.scroll_to(60));
        assert!(feed.grid().row_text(0).is_empty());
        assert_eq!(feed.take_history_request(), Some(60));
    }

    /// Where the daemon says nothing about its history — an older daemon — the shift is
    /// still taken from the proof in the screens, so ordinary scrolling holds the view still.
    #[test]
    fn a_shift_proved_by_the_screens_themselves_is_used_when_the_daemon_reports_nothing() {
        let start = screen(&["l0", "l1", "l2", "l3"], 4, 8);
        let mut feed = PaneFeed::attach(&attachment(Some(start.clone()), 1));
        let plain = |feed: &mut PaneFeed, seq: u64, previous: &Grid, bottom: &str| -> Grid {
            let mut next = scrolled(previous, bottom);
            // As an older daemon would send it: no history count at all.
            next.scrollback_len = 0;
            assert_eq!(feed.apply(seq, &ScreenUpdate::full(next.clone())), Ok(()));
            next
        };
        let one = plain(&mut feed, 1, &start, "l4");
        assert_eq!(feed.history_rows(), 1);
        assert!(feed.scroll_by(1));
        assert_eq!(feed.grid().row_text(0), "l0");
        let _ = plain(&mut feed, 2, &one, "l5");
        assert_eq!(feed.offset(), 2, "the proved shift moved the offset");
        assert_eq!(feed.grid().row_text(0), "l0");
    }

    /// The other half, and the one that matters more: a screen that repainted rather
    /// than scrolled contributes nothing. Inventing history would be worse than not
    /// having any.
    #[test]
    fn a_screen_that_repainted_rather_than_scrolled_contributes_no_history() {
        let start = screen(&["l0", "l1", "l2", "l3"], 4, 8);
        let mut feed = PaneFeed::attach(&attachment(Some(start), 1));
        let unrelated = screen(&["zz", "yy", "xx", "ww"], 4, 8);
        assert_eq!(feed.apply(1, &ScreenUpdate::full(unrelated)), Ok(()));
        assert_eq!(
            feed.history_rows(),
            0,
            "no shift explains this change, so nothing may be recorded"
        );
        assert!(!feed.scroll_by(1), "there is nothing to scroll into");
    }

    #[test]
    fn a_change_inside_the_screen_is_not_mistaken_for_a_scroll() {
        let start = screen(&["prompt $", "", "", ""], 4, 10);
        let mut feed = PaneFeed::attach(&attachment(Some(start.clone()), 1));
        let mut typed = start.clone();
        if let Some(cell) = typed.cell_mut(0, 8) {
            cell.text = "c".into();
        }
        assert_eq!(
            feed.apply(1, &ScreenUpdate::between(&start, &typed)),
            Ok(())
        );
        assert_eq!(feed.history_rows(), 0, "a keystroke is not a scroll");
        assert_eq!(feed.cached_rows(), 0);
    }

    /// The rule a TUI depends on: while a full-screen program is in control, its
    /// redraws are not history and Turn's viewport does not move.
    #[test]
    fn the_alternate_screen_records_no_history_and_refuses_to_scroll() {
        let start = screen(&["l0", "l1", "l2", "l3"], 4, 8);
        let mut feed = PaneFeed::attach(&attachment(Some(start.clone()), 1));
        // Some real history first, so the refusal is about the mode and not about
        // there being nothing to scroll to.
        assert_eq!(
            feed.apply(1, &ScreenUpdate::full(scrolled(&start, "l4"))),
            Ok(())
        );
        assert_eq!(feed.history_rows(), 1);
        assert!(feed.scroll_by(1));

        let mut tui = feed.screen.clone();
        tui.alternate_screen = true;
        // What the daemon reports while a full-screen program is in front: the alternate
        // grid has no scrollback of its own.
        tui.scrollback_len = 0;
        assert_eq!(feed.apply(2, &ScreenUpdate::full(tui.clone())), Ok(()));
        assert_eq!(
            feed.offset(),
            0,
            "Turn stands down: the program owns the viewport"
        );

        let mut redrawn = tui;
        for row in 0..4u16 {
            for (col, ch) in "####".chars().enumerate() {
                if let Some(cell) = redrawn.cell_mut(row, col as u16) {
                    cell.text = ch.to_string();
                }
            }
        }
        assert_eq!(feed.apply(3, &ScreenUpdate::full(redrawn)), Ok(()));
        assert_eq!(
            feed.history_rows(),
            0,
            "a full-screen program's redraws are not scrollback"
        );
        assert!(
            !feed.scroll_by(1),
            "a TUI must not be scrolled out from under the user"
        );
        assert!(!feed.page_up());
        assert!(!feed.scroll_to_top());
        assert!(!feed.grid().can_scroll_back());
    }

    #[test]
    fn a_resize_clears_the_history_because_its_rows_no_longer_fit() {
        let start = screen(&["l0", "l1", "l2", "l3"], 4, 8);
        let mut feed = PaneFeed::attach(&attachment(Some(start.clone()), 1));
        assert_eq!(
            feed.apply(1, &ScreenUpdate::full(scrolled(&start, "l4"))),
            Ok(())
        );
        assert_eq!(feed.cached_rows(), 1);

        feed.resync(Grid::blank(10, 40), 2);
        assert_eq!(feed.cached_rows(), 0);
        assert_eq!(feed.offset(), 0);
        assert_eq!(feed.size(), PtySize::new(10, 40));
    }

    #[test]
    fn a_history_window_of_the_wrong_width_is_discarded_rather_than_painted() {
        let mut held = screen(&["live"], 4, 8);
        held.scrollback_len = 40;
        let mut feed = PaneFeed::attach(&attachment(Some(held), 1));
        feed.receive_history(&history_window(&["from before the resize"], 4, 30, 10, 40));
        assert_eq!(feed.cached_rows(), 0);
    }

    #[test]
    fn a_resync_at_the_same_size_keeps_the_history_it_already_holds() {
        let start = screen(&["l0", "l1", "l2", "l3"], 4, 8);
        let mut feed = PaneFeed::attach(&attachment(Some(start.clone()), 1));
        assert_eq!(
            feed.apply(1, &ScreenUpdate::full(scrolled(&start, "l4"))),
            Ok(())
        );
        let mut fresh = screen(&["fresh"], 4, 8);
        fresh.scrollback_len = 1;
        feed.resync(fresh, 9);
        assert_eq!(feed.cached_rows(), 1);
        assert_eq!(feed.history_rows(), 1);
    }

    #[test]
    fn a_truncated_daemon_ring_makes_the_record_incomplete_so_the_pane_can_say_so() {
        let mut truncated = attachment(Some(Grid::blank(4, 8)), 1);
        truncated.scrollback_truncated = true;
        assert!(!PaneFeed::attach(&truncated).history_complete());
        assert!(PaneFeed::attach(&attachment(Some(Grid::blank(4, 8)), 1)).history_complete());
    }

    /// History has to remember that a row was wrapped, or a selection over scrolled-back
    /// output turns one broken line into two.
    #[test]
    fn a_row_that_wrapped_is_still_wrapped_after_it_scrolls_into_history() {
        let mut start = screen(&["long line th", "at wrapped", "l2", "l3"], 4, 12);
        assert!(start.set_row_wrapped(0, true));
        let mut feed = PaneFeed::attach(&attachment(Some(start.clone()), 1));

        // Two scrolls, so both halves of the wrapped line end up in the history.
        let once = scrolled(&start, "l4");
        assert_eq!(feed.apply(1, &ScreenUpdate::full(once.clone())), Ok(()));
        assert_eq!(
            feed.apply(2, &ScreenUpdate::full(scrolled(&once, "l5"))),
            Ok(())
        );
        assert_eq!(feed.history_rows(), 2);

        assert!(feed.scroll_by(2));
        let view = feed.grid().clone();
        assert_eq!(view.row_text(0), "long line th");
        assert!(
            view.row_wrapped(0),
            "the wrap flag has to travel with the row into the scrolled view"
        );
        assert!(!view.row_wrapped(1), "and the row below it did not wrap");
    }

    /// A scrolled view assembles rows from two places, so a wrap flag left where it was
    /// would claim a line continues into text that is not its own.
    #[test]
    fn a_scrolled_view_does_not_leave_a_wrap_flag_on_the_wrong_row() {
        let start = screen(&["l0", "l1", "l2", "l3"], 4, 8);
        let mut feed = PaneFeed::attach(&attachment(Some(start.clone()), 1));
        let mut next = scrolled(&start, "l4");
        // The live screen's first row wraps; after scrolling back it is no longer row 0.
        assert!(next.set_row_wrapped(0, true));
        assert_eq!(feed.apply(1, &ScreenUpdate::full(next)), Ok(()));
        assert!(feed.scroll_by(1));

        let view = feed.grid().clone();
        assert_eq!(view.row_text(0), "l0", "row 0 came from history");
        assert!(!view.row_wrapped(0), "and history says it did not wrap");
        assert!(view.row_wrapped(1), "the live row kept its own flag");
    }

    /// Clearing the buffer stops Turn showing its record and touches nothing else: the
    /// screen belongs to the program, and reaching it would mean typing into it.
    #[test]
    fn clearing_the_buffer_drops_the_history_and_returns_to_the_live_screen() {
        let start = screen(&["l0", "l1", "l2", "l3"], 4, 8);
        let mut feed = PaneFeed::attach(&attachment(Some(start.clone()), 1));
        assert_eq!(
            feed.apply(1, &ScreenUpdate::full(scrolled(&start, "l4"))),
            Ok(())
        );
        assert!(feed.scroll_by(1));
        assert_eq!(feed.history_rows(), 1);
        assert!(feed.history_complete());

        feed.clear_history();
        assert_eq!(feed.history_rows(), 0);
        assert_eq!(feed.cached_rows(), 0);
        assert_eq!(
            feed.offset(),
            0,
            "the viewport cannot point into what is no longer shown"
        );
        assert!(!feed.scroll_by(1), "and there is nothing to scroll into");
        assert_eq!(
            feed.grid().row_text(0),
            "l1",
            "the live screen is untouched: it is the program's, not Turn's"
        );
        assert!(
            !feed.history_complete(),
            "rows were discarded, so the record no longer reaches back to the attach"
        );

        // Clearing an empty history discards nothing and must not claim otherwise.
        let mut fresh = PaneFeed::attach(&attachment(Some(start), 1));
        fresh.clear_history();
        assert!(fresh.history_complete());
    }

    /// The memory bound. Rows are cached as runs rather than cells, which is the difference
    /// between a megabyte a pane and twenty-four.
    #[test]
    fn the_history_cache_drops_its_oldest_rows_rather_than_growing_without_bound() {
        let mut history = History::default();
        for line in 0..(MAX_HISTORY_ROWS + 100) {
            let row = Grid::from_lines(&[&format!("line {line}")], 120);
            history.insert(line, HistoryRow::of(&row, 0), 120);
        }
        assert_eq!(history.len(), MAX_HISTORY_ROWS);
        assert!(
            history.row(0).is_none(),
            "the oldest went, and the daemon still has it"
        );
        assert!(history.row(MAX_HISTORY_ROWS + 99).is_some());
        assert!(history.holds_all(200..400));
        assert!(!history.holds_all(0..400));
        assert!(!history.is_empty());

        // A row is a handful of runs, not a hundred and twenty cells.
        let row = history.row(MAX_HISTORY_ROWS + 99).expect("the newest row");
        assert!(row.runs.len() <= 3, "{:?}", row.runs);
        assert_eq!(
            row.cells(120).map(|cells| cells.len()),
            Some(120),
            "and it expands back to the row it was"
        );
    }

    /// A cached row of the old width would be painted into a grid of the new one, so a
    /// change of width empties the cache rather than mixing them.
    #[test]
    fn a_history_row_of_another_width_replaces_the_cache_rather_than_joining_it() {
        let mut history = History::default();
        let narrow = Grid::from_lines(&["old"], 20);
        history.insert(0, HistoryRow::of(&narrow, 0), 20);
        let wide = Grid::from_lines(&["new"], 120);
        history.insert(1, HistoryRow::of(&wide, 0), 120);
        assert_eq!(history.len(), 1);
        assert!(history.row(0).is_none());
    }

    #[test]
    fn scrolling_past_the_end_of_the_history_stops_there_rather_than_going_blank() {
        let start = screen(&["l0", "l1", "l2", "l3"], 4, 8);
        let mut feed = PaneFeed::attach(&attachment(Some(start.clone()), 1));
        assert_eq!(
            feed.apply(1, &ScreenUpdate::full(scrolled(&start, "l4"))),
            Ok(())
        );
        assert!(feed.scroll_by(500));
        assert_eq!(feed.offset(), 1, "clamped to what history exists");
        assert!(
            !feed.scroll_by(500),
            "and it does not keep reporting a move"
        );
        assert!(!feed.scroll_to_top(), "which is already the top");
    }

    /// Paging, and the two ends. A page is a screen less one row so the line the user was
    /// reading is still on screen after it moves.
    #[test]
    fn paging_moves_a_screen_less_a_row_and_the_ends_go_all_the_way() {
        let mut held = screen(&["live"], 10, 20);
        held.scrollback_len = 500;
        let mut feed = PaneFeed::attach(&attachment(Some(held), 1));
        assert_eq!(feed.page(), 9);

        assert!(feed.page_up());
        assert_eq!(feed.offset(), 9);
        assert!(feed.page_up());
        assert_eq!(feed.offset(), 18);
        assert!(feed.page_down());
        assert_eq!(feed.offset(), 9);

        assert!(feed.scroll_to_top());
        assert_eq!(feed.offset(), 500);
        assert!(!feed.page_up(), "the top is the top");
        assert!(feed.scroll_to_bottom());
        assert_eq!(feed.offset(), 0);
        assert!(!feed.page_down());
        assert!(
            !feed.scroll_to_bottom(),
            "and the live screen reports no further move"
        );
    }

    /// The one-row pane: a page must still be a row, not zero, or paging would do nothing.
    #[test]
    fn a_pane_one_row_tall_still_pages_by_a_row() {
        let mut held = screen(&["only"], 1, 20);
        held.scrollback_len = 10;
        let mut feed = PaneFeed::attach(&attachment(Some(held), 1));
        assert_eq!(feed.page(), 1);
        assert!(feed.page_up());
        assert_eq!(feed.offset(), 1);
    }

    /// How a search result is shown: a line index becomes a viewport, centred, with the
    /// arithmetic shared with the daemon rather than repeated here.
    #[test]
    fn revealing_a_line_centres_it_and_clamps_at_both_ends() {
        let mut held = screen(&["live"], 10, 20);
        held.scrollback_len = 1_000;
        let mut feed = PaneFeed::attach(&attachment(Some(held), 1));

        assert_eq!(feed.reveal_line(500), 505, "five rows of context above it");
        assert_eq!(
            feed.reveal_line(0),
            1_000,
            "the oldest row cannot be centred"
        );
        assert_eq!(feed.reveal_line(1_009), 0, "a match on the live screen");
    }

    #[test]
    fn a_shift_is_only_recognised_when_it_explains_the_whole_screen() {
        let previous = screen(&["a", "b", "c", "d"], 4, 4);
        assert_eq!(exact_shift(&previous, &scrolled(&previous, "e")), Some(1));

        let two = scrolled(&scrolled(&previous, "e"), "f");
        assert_eq!(exact_shift(&previous, &two), Some(2));

        assert_eq!(exact_shift(&previous, &previous), None, "nothing moved");
        assert_eq!(
            exact_shift(&previous, &screen(&["w", "x", "y", "z"], 4, 4)),
            None,
            "every row differs, so there is no overlap to prove a shift with"
        );
    }

    // ------------------------------------------------------------------ inline images

    use turn_proto::images::{GridImage, ImageCell, ImageId};

    /// Puts one picture's tiles on a row of a grid, and its placement in the table.
    fn picture(grid: &mut Grid, slot: u8, row: u16, from: u16, cols: u16, id: u64) -> GridImage {
        for dx in 0..cols {
            if let Some(cell) = grid.cell_mut(row, from + dx) {
                *cell = Cell::image(ImageCell::new(slot, 0, dx)).expect("an addressable tile");
            }
        }
        let placed = GridImage::new(slot, ImageId(id), 1, cols, cols as u32 * 8, 16);
        grid.images.retain(|held| held.slot != slot);
        grid.images.push(placed);
        placed
    }

    /// The picture the daemon was holding arrives with the attachment, which is what makes a
    /// re-attaching window show the same pane it was showing before it restarted.
    #[test]
    fn attaching_hands_over_the_pictures_that_are_still_on_the_screen() {
        let mut held = screen(&["plot:"], 6, 20);
        let placed = picture(&mut held, 0, 0, 6, 4, 0xabc);
        let mut feed = PaneFeed::attach(&attachment(Some(held), 1));

        let grid = feed.grid().clone();
        assert_eq!(grid.images, vec![placed]);
        let (image, tile) = grid.image_at(0, 6).expect("a tile of the picture");
        assert_eq!(*image, placed);
        assert_eq!(tile, ImageCell::new(0, 0, 0));
    }

    /// A picture that only scrolls costs row updates and never its table again, and the
    /// client's copy keeps the table it was given.
    #[test]
    fn a_picture_survives_the_updates_that_scroll_it_without_being_described_again() {
        let mut start = screen(&["one", "two"], 5, 20);
        let placed = picture(&mut start, 1, 2, 0, 5, 0xfeed);
        let mut feed = PaneFeed::attach(&attachment(Some(start.clone()), 1));

        let mut daemon = start;
        for seq in 1..=2u64 {
            let mut next = scrolled(&daemon, &format!("line {seq}"));
            // The daemon's table is unchanged: the picture is the same picture, one row up.
            next.images = daemon.images.clone();
            let update = ScreenUpdate::between(&daemon, &next);
            if let ScreenUpdate::Rows { images, .. } = &update {
                assert_eq!(*images, None, "an unchanged table must not be resent");
            }
            assert_eq!(feed.apply(seq, &update), Ok(()));
            daemon = next;
        }
        let grid = feed.grid().clone();
        assert_eq!(grid.images, vec![placed], "the client kept the table");
        // Two rows up from where it was.
        assert!(grid.cell(0, 0).is_some_and(Cell::is_image));
    }

    /// Scrolling back into history has to show the picture that *was* there, and the slot
    /// numbers cannot be trusted to say which one that is: eight slots are reused as pictures
    /// go past, so a row filed an hour ago saying "slot 0" means a different picture from the
    /// one slot 0 holds now.
    #[test]
    fn scrolling_back_shows_the_picture_that_was_there_and_not_the_one_there_now() {
        // The live screen has a picture in slot 0: id 2.
        let mut live = screen(&["now"], 4, 20);
        live.scrollback_len = 2;
        let recent = picture(&mut live, 0, 0, 4, 3, 2);
        let mut feed = PaneFeed::attach(&attachment(Some(live), 1));

        // Two rows of history, whose own slot 0 was a *different* picture: id 1. Scrolled
        // fully back, the view is those two rows and then the top of the live screen — so
        // both pictures are on it at once, in the same source slot.
        feed.receive_history(&{
            let mut window = history_window(&["then", ""], 2, 20, 2, 2);
            picture(&mut window, 0, 0, 5, 3, 1);
            window
        });

        assert!(feed.scroll_by(2), "there is history to scroll into");
        let view = feed.grid().clone();
        assert_eq!(
            view.images.len(),
            2,
            "both pictures are in the view, so they cannot share a slot: {:?}",
            view.images
        );

        // The history row shows the old picture.
        let (old, tile) = view.image_at(0, 5).expect("a tile of the old picture");
        assert_eq!(old.id, ImageId(1));
        assert_eq!(tile.dy, 0);
        // The live screen's first row, two rows further down, shows the new one.
        let (new, _) = view.image_at(2, 4).expect("a tile of the live picture");
        assert_eq!(new.id, recent.id);
        assert_ne!(old.slot, new.slot, "two pictures may not share a slot");
        // And every marker in the view resolves, or the renderer would draw a placeholder
        // where a picture is.
        for row in 0..view.rows {
            for col in 0..view.cols {
                if view.cell(row, col).is_some_and(Cell::is_image) {
                    assert!(
                        view.image_at(row, col).is_some(),
                        "the tile at ({row}, {col}) resolves to nothing"
                    );
                }
            }
        }
    }

    /// A history row whose placement the window never carried must not keep a marker: it
    /// would resolve against somebody else's slot and draw the wrong picture.
    #[test]
    fn a_history_row_with_no_placement_of_its_own_is_blanked_rather_than_guessed_at() {
        let mut live = screen(&["now"], 4, 12);
        live.scrollback_len = 2;
        picture(&mut live, 0, 0, 0, 3, 2);
        let mut feed = PaneFeed::attach(&attachment(Some(live), 1));

        // A window carrying marker cells and *no* table, which is what an older daemon that
        // has not been taught about images would send.
        let mut window = history_window(&["", ""], 2, 12, 2, 2);
        for dx in 0..3u16 {
            if let Some(cell) = window.cell_mut(0, dx) {
                *cell = Cell::image(ImageCell::new(0, 0, dx)).expect("a tile");
            }
        }
        feed.receive_history(&window);

        assert!(feed.scroll_by(2));
        let view = feed.grid().clone();
        for col in 0..3u16 {
            assert!(
                !view.cell(0, col).is_some_and(Cell::is_image),
                "column {col} kept a marker that resolves to the wrong picture"
            );
        }
        // The live picture, two rows further down, is untouched.
        assert!(view.image_at(2, 0).is_some());
    }

    /// A view holding more distinct pictures than a grid has slots keeps the first eight and
    /// blanks the rest, rather than letting two pictures claim one slot.
    #[test]
    fn a_view_with_more_pictures_than_slots_keeps_the_ones_it_can_address() {
        let mut live = screen(&["live"], 12, 40);
        live.scrollback_len = 10;
        let mut feed = PaneFeed::attach(&attachment(Some(live), 1));
        // Ten rows of history, each with its own picture, all in source slot 0.
        for line in 0..10usize {
            let mut window = history_window(&[""], 1, 40, 10 - line, 10);
            picture(&mut window, 0, 0, 0, 2, 100 + line as u64);
            feed.receive_history(&window);
        }
        assert!(feed.scroll_by(10));
        let view = feed.grid().clone();
        assert!(
            view.images.len() <= turn_proto::MAX_PLACED_IMAGES,
            "{} placements",
            view.images.len()
        );
        // Every marker that survived resolves.
        for row in 0..view.rows {
            for col in 0..view.cols {
                if view.cell(row, col).is_some_and(Cell::is_image) {
                    assert!(view.image_at(row, col).is_some());
                }
            }
        }
    }

    /// The live view is not remapped at all: it is the daemon's own grid, and rewriting slots
    /// it did not ask to have rewritten would be work for nothing.
    #[test]
    fn the_live_view_carries_the_daemons_own_table_untouched() {
        let mut live = screen(&["x"], 4, 20);
        let placed = picture(&mut live, 5, 0, 2, 3, 77);
        let mut feed = PaneFeed::attach(&attachment(Some(live), 1));
        assert_eq!(feed.offset(), 0);
        let grid = feed.grid().clone();
        assert_eq!(grid.images, vec![placed]);
        assert_eq!(
            grid.cell(0, 2).and_then(Cell::image_tile).map(|t| t.slot),
            Some(5),
            "the marker keeps the slot the daemon gave it"
        );
    }
}
