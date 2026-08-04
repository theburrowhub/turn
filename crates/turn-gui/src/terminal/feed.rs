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
//! ## Scrollback, and what it can honestly be
//!
//! The protocol has no request for history: the daemon sends the *screen*, and
//! `attach_pane` hands over the screen rather than the scrollback. So the window's
//! history is what the window has watched go past, kept in [`Transcript`].
//!
//! It is recorded only when it can be proved. When a screen arrives, the feed looks
//! for the shift that exactly explains it — every row of the new screen equal to the
//! row `k` further down the old one. When it finds one, those `k` rows provably left
//! the top and are appended. When it does not, **nothing is appended**: a pane that
//! repainted rather than scrolled has no history to add, and inventing some would be
//! worse than having none. [`PaneFeed::history_complete`] says which case the user is
//! in, so the pane can mark where Turn's record begins instead of implying it goes
//! back for ever.

use std::collections::VecDeque;

use turn_proto::cells::{Cell, Grid};
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

/// How many rows of watched history a pane keeps.
///
/// Enough to scroll back through a failed build without keeping a session's whole life
/// in memory. The ring drops the oldest rows rather than growing, and says so.
pub const TRANSCRIPT_ROWS: usize = 5_000;

/// The rows a pane has been seen to scroll past.
#[derive(Debug, Default)]
pub struct Transcript {
    rows: VecDeque<Vec<Cell>>,
    /// True once the ring has dropped a row, so the pane can say its record is
    /// partial rather than implying it reaches back to the start of the session.
    dropped_any: bool,
}

impl Transcript {
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Whether nothing has been discarded, so the record reaches back to the attach.
    pub fn is_complete(&self) -> bool {
        !self.dropped_any
    }

    /// One row of history, counted backwards from the newest.
    pub fn row_from_end(&self, back: usize) -> Option<&[Cell]> {
        let index = self.rows.len().checked_sub(back + 1)?;
        self.rows.get(index).map(Vec::as_slice)
    }

    fn push(&mut self, row: Vec<Cell>) {
        if self.rows.len() >= TRANSCRIPT_ROWS {
            self.rows.pop_front();
            self.dropped_any = true;
        }
        self.rows.push_back(row);
    }

    fn clear(&mut self) {
        self.rows.clear();
    }
}

/// A pane's screen, the history behind it, and where the user is looking.
pub struct PaneFeed {
    screen: Grid,
    transcript: Transcript,
    /// How far back the viewport is, in rows. Zero is the live screen.
    offset: usize,
    /// The next `seq` this feed expects.
    next_seq: u64,
    /// Whether the daemon dropped output before it started keeping this pane's
    /// screen. The screen is correct; what is above it is incomplete.
    daemon_truncated: bool,
    /// Rebuilt only when the viewport moves or the screen changes, because a scrolled
    /// view is assembled from the transcript and that is not free.
    view: Option<Grid>,
}

impl PaneFeed {
    /// Starts a feed from an attachment.
    ///
    /// A cells attachment carries the screen the daemon has been keeping, which is
    /// what makes "processes survive UI restarts" visible rather than claimed. A pane
    /// with no screen — one with no process behind it — starts blank at the agreed
    /// size.
    pub fn attach(attachment: &PaneAttachment) -> Self {
        let screen = match &attachment.screen {
            Some(grid) => (**grid).clone(),
            None => Grid::blank(attachment.size.rows, attachment.size.cols),
        };
        PaneFeed {
            screen,
            transcript: Transcript::default(),
            offset: 0,
            next_seq: attachment.next_seq,
            daemon_truncated: attachment.scrollback_truncated,
            view: None,
        }
    }

    /// A feed for a pane the window has not attached to yet.
    pub fn blank(size: PtySize) -> Self {
        PaneFeed {
            screen: Grid::blank(size.rows, size.cols),
            transcript: Transcript::default(),
            offset: 0,
            next_seq: 0,
            daemon_truncated: false,
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

        let previous = self.screen.clone();
        let mut next = self.screen.clone();
        update
            .apply(&mut next)
            .map_err(|error| Desync::Malformed(error.to_string()))?;

        self.record_history(&previous, &next);
        self.screen = next;
        self.next_seq = seq.saturating_add(1);
        self.view = None;
        Ok(())
    }

    /// Replaces the screen wholesale, after a resync.
    ///
    /// The transcript is kept when the size is unchanged: those rows were still
    /// watched, and discarding them because a resync happened would lose history the
    /// user could see a moment ago. A resize is different — a row of the old width has
    /// no place in a grid of the new one — and clears it.
    pub fn resync(&mut self, grid: Grid, next_seq: u64) {
        if !grid.same_size(&self.screen) {
            self.transcript.clear();
            self.offset = 0;
        }
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

    /// Whether Turn's record of this pane reaches back to the moment it attached.
    ///
    /// False when the daemon had already dropped output, or when the ring has
    /// discarded rows. Either way the pane says where its record begins rather than
    /// letting the user scroll up into a lie.
    pub fn history_complete(&self) -> bool {
        !self.daemon_truncated && self.transcript.is_complete()
    }

    pub fn history_rows(&self) -> usize {
        self.transcript.len()
    }

    /// Where the viewport is, in rows above the live screen.
    pub fn offset(&self) -> usize {
        self.offset
    }

    /// Scrolls the viewport, and reports whether it moved.
    ///
    /// Positive is backwards into history, which is what a wheel-up does. Refused
    /// entirely in the alternate screen: a full-screen program owns its viewport, and
    /// scrolling Turn's record out from under `lazygit` would show the user a screen
    /// that no longer exists.
    pub fn scroll_by(&mut self, rows: i32) -> bool {
        if self.screen.alternate_screen {
            return false;
        }
        let wanted = (self.offset as i64 + rows as i64).max(0) as usize;
        let clamped = wanted.min(self.transcript.len());
        if clamped == self.offset {
            return false;
        }
        self.offset = clamped;
        self.view = None;
        true
    }

    /// Returns to the live screen. What any keystroke does in every terminal.
    pub fn scroll_to_bottom(&mut self) -> bool {
        if self.offset == 0 {
            return false;
        }
        self.offset = 0;
        self.view = None;
        true
    }

    /// The grid to paint: the live screen, or a view assembled from history.
    pub fn grid(&mut self) -> &Grid {
        if self.view.is_none() {
            self.view = Some(self.build_view());
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

    /// The live screen, whatever the viewport is doing.
    ///
    /// What a thumbnail wants: the overview should show what a session is doing now,
    /// not where somebody left a scrollbar.
    pub fn live_screen(&self) -> &Grid {
        &self.screen
    }

    /// Builds the scrolled viewport: `offset` rows of history, then the top of the
    /// live screen.
    fn build_view(&self) -> Grid {
        let mut grid = self.screen.clone();
        grid.scrollback_offset = self.offset;
        grid.scrollback_len = self.transcript.len();
        if self.offset == 0 {
            return grid;
        }
        let rows = self.screen.rows as usize;
        for target in 0..rows {
            // Row 0 of the viewport is `offset` rows back from the newest history row.
            let cells = if target < self.offset {
                let back = self.offset - target - 1;
                self.transcript.row_from_end(back).map(<[Cell]>::to_vec)
            } else {
                Some(self.screen.row((target - self.offset) as u16).to_vec())
            };
            if let Some(cells) = cells {
                grid.set_row(target as u16, &cells);
            }
        }
        // A scrolled view has no cursor: the cursor is on the live screen, and drawing
        // it at the same coordinates in a historical view would put it on an unrelated
        // character.
        grid.cursor = None;
        grid
    }

    /// Appends the rows that provably left the top of the screen.
    ///
    /// Only an exact shift counts. If every row of the new screen equals the row `k`
    /// further down the old one, those first `k` rows scrolled off and nothing else
    /// happened, so appending them is a fact. Anything else — a repaint, a TUI
    /// redrawing itself, a change in the middle of the screen — contributes nothing,
    /// because a guess in the scrollback is worse than a gap.
    fn record_history(&mut self, previous: &Grid, next: &Grid) {
        if next.alternate_screen || previous.alternate_screen {
            // A full-screen program's redraws are not history.
            return;
        }
        if !previous.same_size(next) {
            return;
        }
        let Some(shift) = exact_shift(previous, next) else {
            return;
        };
        for row in 0..shift {
            self.transcript.push(previous.row(row as u16).to_vec());
        }
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
            replay: TerminalBytes::new(Vec::new()),
            size,
            scrollback_truncated: false,
            bytes_seen: 0,
            next_seq,
        }
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
    /// program printing a line at the bottom of the screen does.
    fn scrolled(grid: &Grid, new_bottom: &str) -> Grid {
        let mut next = Grid::blank(grid.rows, grid.cols);
        for row in 1..grid.rows {
            next.set_row(row - 1, grid.row(row));
        }
        let last = grid.rows - 1;
        for (col, ch) in new_bottom.chars().enumerate().take(grid.cols as usize) {
            if let Some(cell) = next.cell_mut(last, col as u16) {
                cell.text = ch.to_string();
            }
        }
        next
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
        let update = ScreenUpdate::Rows {
            size: PtySize::new(40, 120),
            cursor: None,
            alternate_screen: false,
            rows: Vec::new(),
        };
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
        let update = ScreenUpdate::Rows {
            size: PtySize::new(4, 8),
            cursor: None,
            alternate_screen: false,
            rows: vec![GridRow {
                row: 99,
                runs: vec![CellRun {
                    text: String::new(),
                    cells: 8,
                    fg: None,
                    bg: None,
                    attrs: Default::default(),
                }],
            }],
        };
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

    /// The honest half of scrollback: a screen that scrolled contributes exactly the
    /// rows that left the top.
    #[test]
    fn rows_that_provably_scrolled_off_the_top_become_history() {
        let start = screen(&["l0", "l1", "l2", "l3"], 4, 8);
        let mut feed = PaneFeed::attach(&attachment(Some(start.clone()), 1));

        let after = scrolled(&start, "l4");
        assert_eq!(feed.apply(1, &ScreenUpdate::full(after.clone())), Ok(()));
        assert_eq!(feed.history_rows(), 1, "one row left the top");

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

        assert!(feed.scroll_to_bottom());
        assert_eq!(feed.grid().row_text(0), "l2");
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

        let mut tui = feed.live_screen().clone();
        tui.alternate_screen = true;
        assert_eq!(feed.apply(2, &ScreenUpdate::full(tui.clone())), Ok(()));

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
            1,
            "a full-screen program's redraws are not scrollback"
        );
        assert!(
            !feed.scroll_by(1),
            "a TUI must not be scrolled out from under the user"
        );
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
        assert_eq!(feed.history_rows(), 1);

        feed.resync(Grid::blank(10, 40), 2);
        assert_eq!(feed.history_rows(), 0);
        assert_eq!(feed.offset(), 0);
        assert_eq!(feed.size(), PtySize::new(10, 40));
    }

    #[test]
    fn a_resync_at_the_same_size_keeps_the_history_the_user_could_see_a_moment_ago() {
        let start = screen(&["l0", "l1", "l2", "l3"], 4, 8);
        let mut feed = PaneFeed::attach(&attachment(Some(start.clone()), 1));
        assert_eq!(
            feed.apply(1, &ScreenUpdate::full(scrolled(&start, "l4"))),
            Ok(())
        );
        feed.resync(screen(&["fresh"], 4, 8), 9);
        assert_eq!(feed.history_rows(), 1);
    }

    #[test]
    fn a_truncated_daemon_ring_makes_the_record_incomplete_so_the_pane_can_say_so() {
        let mut truncated = attachment(Some(Grid::blank(4, 8)), 1);
        truncated.scrollback_truncated = true;
        assert!(!PaneFeed::attach(&truncated).history_complete());
        assert!(PaneFeed::attach(&attachment(Some(Grid::blank(4, 8)), 1)).history_complete());
    }

    #[test]
    fn the_history_ring_drops_the_oldest_rows_rather_than_growing_without_bound() {
        let mut transcript = Transcript::default();
        for index in 0..(TRANSCRIPT_ROWS + 10) {
            transcript.push(vec![Cell::plain(format!("{index}"))]);
        }
        assert_eq!(transcript.len(), TRANSCRIPT_ROWS);
        assert!(
            !transcript.is_complete(),
            "a pane whose record has been trimmed must be able to say so"
        );
        assert_eq!(
            transcript
                .row_from_end(0)
                .and_then(|row| row.first())
                .map(|cell| cell.text.clone()),
            Some(format!("{}", TRANSCRIPT_ROWS + 9)),
            "the newest row is the last one pushed"
        );
        assert!(!transcript.is_empty());
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
    }

    /// The live screen is what a thumbnail wants, whatever the user has done with the
    /// scrollbar.
    #[test]
    fn the_live_screen_is_available_regardless_of_where_the_viewport_is() {
        let start = screen(&["l0", "l1", "l2", "l3"], 4, 8);
        let mut feed = PaneFeed::attach(&attachment(Some(start.clone()), 1));
        assert_eq!(
            feed.apply(1, &ScreenUpdate::full(scrolled(&start, "l4"))),
            Ok(())
        );
        assert!(feed.scroll_by(1));
        assert_eq!(feed.grid().row_text(0), "l0", "the viewport is in the past");
        assert_eq!(
            feed.live_screen().row_text(0),
            "l1",
            "the overview must show what the session is doing now"
        );
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
}
