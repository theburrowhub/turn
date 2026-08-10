//! Screen updates: what changed on a pane since the last one.
//!
//! A full grid on every keystroke is the thing that makes a multiplexer feel slow.
//! One 40x120 screen is about a kilobyte encoded ([`crate::cells`]), which is
//! nothing on its own and is thirty kilobytes a frame across thirty attached panes,
//! sixty times a second, for an echo of one character.
//!
//! So an update carries **changed rows**. A row is already the unit the grid is
//! encoded in, its runs are already computed, and a client applies one by replacing
//! it — no per-cell addressing, no partial row that could leave the grid ragged.
//!
//! ## Rows rather than cell runs, and why
//!
//! Cell-level runs would be smaller for the pathological case of one character
//! changing inside a dense 120-column row. Measured on realistic screens the
//! difference does not pay for the addressing it needs: a keystroke echo touches a
//! prompt row that is mostly blank, which encodes as two or three runs — around
//! forty bytes — and a scroll touches every row either way. The test
//! `a_keystroke_costs_a_row_rather_than_a_screen` in this module holds those numbers
//! down.
//!
//! ## The cap
//!
//! Past the point where more than half the rows have changed, the whole grid is
//! smaller than the rows plus their addressing, so [`ScreenUpdate::between`] sends
//! the grid. That is the cap on how much one update may carry: never more than one
//! screen, and one screen is bounded by [`crate::MAX_SCREEN_CELLS`], which is
//! refused at attach rather than truncated later.
//!
//! ## Sequence and resync
//!
//! Every update carries a `seq` that increases by one per update per attachment. A
//! client that sees a jump has missed one and can ask for
//! [`crate::Request::ResyncPane`], which answers with the whole grid. It does not
//! have to: the daemon notices its own dropped frame and makes the next update a
//! full grid. Both paths exist because they fail differently — the daemon's repair
//! needs the pane to produce output again, and the client's does not.

use serde::{Deserialize, Serialize};

use crate::cells::{decode_runs, CellRun, Grid, GridError, RowLink, RowMeta};
use crate::geometry::PtySize;

/// Which representation of a pane a client wants.
///
/// Cells are the default because that is what a renderer that does not embed a VT
/// emulator needs, and because the daemon has already parsed the screen. Bytes stay
/// available for the cases that genuinely need the stream itself: capturing a log,
/// a client that has its own emulator, a future web frontend built on one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaneStream {
    /// The parsed screen, as [`Grid`] and [`ScreenUpdate`].
    #[default]
    Cells,
    /// The raw escape stream, as `pane_output` frames.
    Bytes,
}

impl PaneStream {
    pub fn is_cells(&self) -> bool {
        matches!(self, PaneStream::Cells)
    }

    pub fn is_bytes(&self) -> bool {
        matches!(self, PaneStream::Bytes)
    }
}

/// One row of a screen, in the grid's own run encoding, with what the row carries
/// besides its cells.
///
/// The metadata travels with the row rather than with the update as a whole because it
/// changes exactly when the row does: a link appears because the text under it was
/// rewritten, and a row stops wrapping because it was reflowed. Sending it separately
/// would let a client apply cells from one frame and links from another.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GridRow {
    pub row: u16,
    pub runs: Vec<CellRun>,
    /// Whether the terminal broke this row at the margin. Absent on the wire when false,
    /// which is nearly always.
    #[serde(default, skip_serializing_if = "is_false")]
    pub wrapped: bool,
    /// OSC 8 hyperlinks over this row's columns. Absent on the wire when there are none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<RowLink>,
}

impl GridRow {
    /// One row of a grid, cells and metadata together.
    pub fn of(grid: &Grid, row: u16) -> Self {
        let meta = grid.row_meta(row);
        Self {
            row,
            runs: grid.row_runs(row),
            wrapped: meta.wrapped,
            links: meta.links.clone(),
        }
    }

    /// The metadata this row carries, as a grid stores it.
    pub fn meta(&self) -> RowMeta {
        RowMeta {
            wrapped: self.wrapped,
            links: self.links.clone(),
        }
    }
}

/// What changed on a pane's screen.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ScreenUpdate {
    /// The whole screen. Sent on attach, on resync, after a resize, and whenever a
    /// diff would not be smaller.
    ///
    /// Boxed because the other variant is the common one by a wide margin, and an
    /// enum sized for a whole grid would make every row update carry that width
    /// through the daemon's channels.
    Full { grid: Box<Grid> },
    /// The rows that differ, plus the cursor and mode that go with them.
    ///
    /// `size` is carried so a client can refuse an update meant for a geometry it is
    /// no longer rendering, rather than writing rows into the wrong shape.
    Rows {
        size: PtySize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cursor: Option<(u16, u16)>,
        #[serde(default, skip_serializing_if = "is_false")]
        alternate_screen: bool,
        /// How many rows of history sit above this screen, as the daemon holds them.
        ///
        /// Carried on every update because it is how the client knows two things it
        /// cannot work out for itself: that there is history to scroll into at all, and
        /// **how many rows just left the top**. The second is what keeps a scrolled
        /// viewport still — the offset is measured from the live screen, so when rows
        /// scroll off, an unchanged offset would show newer content and the user's place
        /// would slide out from under them. Absent on the wire when zero, which is every
        /// update for a pane that has not scrolled yet.
        #[serde(default, skip_serializing_if = "is_zero")]
        scrollback_len: usize,
        /// The screen's inline-image table, when it has changed.
        ///
        /// `None` — absent on the wire — means "the pictures are the same as they were",
        /// which is every update for the overwhelming majority of panes and every update
        /// for a pane whose picture is merely scrolling. `Some` replaces the table whole,
        /// because a table of at most eight small entries is not worth diffing and a
        /// partial one would leave a client unsure which slots it still knows about.
        ///
        /// The distinction matters: a screen that has *lost* its last picture sends
        /// `Some(vec![])`, and an empty list must not be mistaken for silence.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        images: Option<Vec<crate::images::GridImage>>,
        /// What the pane refused to draw, when that changed. Same `None`-is-silence rule as
        /// `images`, and for the same reason: a pane that has refused nothing sends nothing,
        /// and a pane whose refusals were cleared sends `Some(vec![])`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        notices: Option<Vec<crate::images::ImageNotice>>,
        rows: Vec<GridRow>,
    },
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

/// Refuses a row's links before they are placed.
///
/// [`Grid::set_row_meta`] would silently drop a span that is out of range or overlapping,
/// which is the right behaviour for a caller assembling a grid by hand and the wrong one
/// for an update off the wire: a peer sending an impossible span has a defect, and a
/// client that quietly ignored it would render fewer links than the daemon thinks it sent.
fn check_row_links(links: &[RowLink], row: u16, cols: u16) -> Result<(), GridError> {
    if links.len() > crate::cells::MAX_SCREEN_LINKS {
        return Err(GridError::TooManyLinks {
            count: links.len(),
            max: crate::cells::MAX_SCREEN_LINKS,
        });
    }
    for (index, link) in links.iter().enumerate() {
        let chars = link.uri.chars().count();
        if chars > crate::cells::MAX_LINK_URI_CHARS {
            return Err(GridError::LinkUriLength {
                chars,
                max: crate::cells::MAX_LINK_URI_CHARS,
            });
        }
        if link.from >= link.to || link.to > cols {
            return Err(GridError::LinkRange {
                row,
                from: link.from,
                to: link.to,
                cols,
            });
        }
        if let Some(clash) = links[..index]
            .iter()
            .find(|placed| link.from < placed.to && placed.from < link.to)
        {
            return Err(GridError::LinkOverlap {
                row,
                col: link.from.max(clash.from),
            });
        }
    }
    Ok(())
}

impl ScreenUpdate {
    /// The whole screen.
    pub fn full(grid: Grid) -> Self {
        ScreenUpdate::Full {
            grid: Box::new(grid),
        }
    }

    /// The smallest honest update that turns `previous` into `next`.
    ///
    /// Falls back to the whole grid when the geometry changed — there is no row
    /// correspondence across a resize — and when more than half the rows differ,
    /// because past that point the grid is the smaller message.
    pub fn between(previous: &Grid, next: &Grid) -> Self {
        if !next.same_size(previous) {
            return Self::full(next.clone());
        }
        let changed = next.changed_rows(previous);
        if changed.len() * 2 > next.rows as usize {
            return Self::full(next.clone());
        }
        ScreenUpdate::Rows {
            size: PtySize::new(next.rows, next.cols),
            cursor: next.cursor,
            alternate_screen: next.alternate_screen,
            scrollback_len: next.scrollback_len,
            // Sent only when it moved. A picture scrolling up the screen changes rows on
            // every update and its table on none of them, which is exactly the case worth
            // being cheap.
            images: if next.images == previous.images {
                None
            } else {
                Some(next.images.clone())
            },
            notices: if next.notices == previous.notices {
                None
            } else {
                Some(next.notices.clone())
            },
            rows: changed
                .into_iter()
                .map(|row| GridRow::of(next, row))
                .collect(),
        }
    }

    /// Applies the update to a client's copy of the screen.
    ///
    /// Refuses a row update whose geometry does not match rather than writing into
    /// the wrong shape: a client in that position has missed a resize, and the way
    /// back is a resync, not a best effort.
    pub fn apply(&self, target: &mut Grid) -> Result<(), GridError> {
        match self {
            ScreenUpdate::Full { grid } => {
                *target = (**grid).clone();
                Ok(())
            }
            ScreenUpdate::Rows {
                size,
                cursor,
                alternate_screen,
                scrollback_len,
                images,
                notices,
                rows,
            } => {
                if target.rows != size.rows || target.cols != size.cols {
                    return Err(GridError::SizeMismatch {
                        rows: target.rows,
                        cols: target.cols,
                        update_rows: size.rows,
                        update_cols: size.cols,
                    });
                }
                for row in rows {
                    if row.row >= target.rows {
                        return Err(GridError::RowOutOfRange {
                            row: row.row,
                            rows: target.rows,
                        });
                    }
                    let cells = decode_runs(&row.runs, target.cols, row.row)?;
                    check_row_links(&row.links, row.row, target.cols)?;
                    if !target.set_row(row.row, &cells) || !target.set_row_meta(row.row, row.meta())
                    {
                        return Err(GridError::RowOutOfRange {
                            row: row.row,
                            rows: target.rows,
                        });
                    }
                }
                // Checked before it replaces anything, so a client's copy is never left
                // holding a table it would go on to index by slot.
                if let Some(images) = images {
                    crate::images::check_table(images).map_err(GridError::Image)?;
                    target.images = images.clone();
                }
                if let Some(notices) = notices {
                    crate::images::check_notices(notices).map_err(GridError::Image)?;
                    target.notices = notices.clone();
                }
                target.cursor = *cursor;
                target.alternate_screen = *alternate_screen;
                target.scrollback_len = *scrollback_len;
                Ok(())
            }
        }
    }

    /// Whether this update carries the whole screen.
    pub fn is_full(&self) -> bool {
        matches!(self, ScreenUpdate::Full { .. })
    }

    /// The geometry this update describes.
    pub fn size(&self) -> PtySize {
        match self {
            ScreenUpdate::Full { grid } => PtySize::new(grid.rows, grid.cols),
            ScreenUpdate::Rows { size, .. } => *size,
        }
    }

    /// How many rows the update carries. A full screen carries all of them.
    pub fn row_count(&self) -> usize {
        match self {
            ScreenUpdate::Full { grid } => grid.rows as usize,
            ScreenUpdate::Rows { rows, .. } => rows.len(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cells::{CellAttrs, Rgb};

    /// A 40x120 pane with a shell prompt on the first row, which is what most panes
    /// look like most of the time.
    fn prompt_screen() -> Grid {
        let mut grid = Grid::blank(40, 120);
        write_row(&mut grid, 0, "~/turn on main $ ");
        grid.cursor = Some((0, 17));
        grid
    }

    fn write_row(grid: &mut Grid, row: u16, text: &str) {
        for col in 0..grid.cols {
            if let Some(cell) = grid.cell_mut(row, col) {
                *cell = crate::cells::Cell::blank();
            }
        }
        for (col, ch) in text.chars().enumerate().take(grid.cols as usize) {
            if let Some(cell) = grid.cell_mut(row, col as u16) {
                cell.text = ch.to_string();
            }
        }
    }

    #[test]
    fn a_keystroke_costs_a_row_rather_than_a_screen() {
        let before = prompt_screen();
        let mut after = before.clone();
        write_row(&mut after, 0, "~/turn on main $ c");
        after.cursor = Some((0, 18));

        let update = ScreenUpdate::between(&before, &after);
        assert!(!update.is_full(), "one row changed: {update:?}");
        assert_eq!(update.row_count(), 1);

        let diff_bytes = serde_json::to_string(&update)
            .expect("an update serialises")
            .len();
        let full_bytes = serde_json::to_string(&after)
            .expect("a grid serialises")
            .len();
        assert!(
            diff_bytes < 200,
            "a keystroke costs {diff_bytes} bytes, which is more than a row"
        );
        assert!(
            diff_bytes * 3 < full_bytes,
            "the diff must be much smaller than the screen: {diff_bytes} against {full_bytes}"
        );
    }

    #[test]
    fn a_cursor_that_moved_on_its_own_carries_no_rows_at_all() {
        let before = prompt_screen();
        let mut after = before.clone();
        after.cursor = Some((0, 3));

        let update = ScreenUpdate::between(&before, &after);
        assert_eq!(update.row_count(), 0);
        match &update {
            ScreenUpdate::Rows { cursor, rows, .. } => {
                assert_eq!(*cursor, Some((0, 3)));
                assert!(rows.is_empty());
            }
            other => panic!("a cursor move must not send the screen: {other:?}"),
        }

        let mut client = before.clone();
        update.apply(&mut client).expect("the update applies");
        assert_eq!(client, after);
    }

    /// Past half the screen the grid itself is the smaller message, and this is the
    /// cap on how much one update may carry.
    #[test]
    fn a_scroll_that_touches_most_rows_sends_the_whole_screen_instead() {
        let before = Grid::blank(40, 120);
        let mut after = before.clone();
        for row in 0..21u16 {
            write_row(&mut after, row, &format!("line {row}"));
        }
        let update = ScreenUpdate::between(&before, &after);
        assert!(update.is_full(), "21 of 40 rows changed: {update:?}");

        // Exactly half stays a row update: the crossover is "more than half".
        let mut half = before.clone();
        for row in 0..20u16 {
            write_row(&mut half, row, &format!("line {row}"));
        }
        assert!(!ScreenUpdate::between(&before, &half).is_full());
    }

    #[test]
    fn a_resize_sends_the_whole_screen_because_rows_no_longer_correspond() {
        let before = Grid::blank(24, 80);
        let after = Grid::blank(40, 120);
        let update = ScreenUpdate::between(&before, &after);
        assert!(update.is_full());
        assert_eq!(update.size(), PtySize::new(40, 120));
    }

    #[test]
    fn applying_a_row_update_to_the_wrong_geometry_is_refused_so_the_client_resyncs() {
        let before = Grid::blank(24, 80);
        let mut after = before.clone();
        write_row(&mut after, 0, "hello");
        let update = ScreenUpdate::between(&before, &after);

        let mut wrong = Grid::blank(40, 120);
        let error = update
            .apply(&mut wrong)
            .expect_err("a client that missed a resize must not write into the wrong shape");
        assert!(
            matches!(error, GridError::SizeMismatch { .. }),
            "got {error}"
        );
        assert_eq!(wrong, Grid::blank(40, 120), "and nothing was written");
    }

    #[test]
    fn a_row_outside_the_screen_is_refused_rather_than_ignored() {
        let mut client = Grid::blank(4, 8);
        let update = ScreenUpdate::Rows {
            size: PtySize::new(4, 8),
            cursor: None,
            alternate_screen: false,
            scrollback_len: 0,
            images: None,
            notices: None,
            rows: vec![GridRow {
                row: 9,
                runs: vec![CellRun {
                    text: String::new(),
                    cells: 8,
                    fg: None,
                    bg: None,
                    attrs: CellAttrs::default(),
                }],
                wrapped: false,
                links: Vec::new(),
            }],
        };
        assert!(matches!(
            update.apply(&mut client),
            Err(GridError::RowOutOfRange { row: 9, rows: 4 })
        ));
    }

    /// The property that matters: a client applying every update in order ends up
    /// with exactly the screen the daemon has, colours and cursor included.
    #[test]
    fn a_client_applying_every_update_in_order_ends_up_with_the_daemons_screen() {
        let mut daemon = prompt_screen();
        let mut client = daemon.clone();

        let script = [
            "~/turn on main $ cargo test",
            "~/turn on main $ cargo test\r",
            "   Compiling turn-proto v0.1.0",
        ];
        for (step, line) in script.iter().enumerate() {
            let mut next = daemon.clone();
            write_row(&mut next, step as u16, line);
            if step == 2 {
                // A colour and an attribute, which a row update has to carry too.
                let cell = next.cell_mut(2, 3).expect("the cell");
                cell.fg = Some(Rgb::new(0, 200, 0));
                cell.attrs = CellAttrs::default().with(CellAttrs::BOLD);
                next.alternate_screen = true;
            }
            next.cursor = Some((step as u16, 4));

            let update = ScreenUpdate::between(&daemon, &next);
            // Through JSON, because that is the only form the client ever sees.
            let wire = serde_json::to_string(&update).expect("an update serialises");
            let arrived: ScreenUpdate = serde_json::from_str(&wire).expect("and reads back");
            arrived.apply(&mut client).expect("the update applies");
            daemon = next;
            assert_eq!(client, daemon, "the two screens disagree after step {step}");
        }
        assert!(client.alternate_screen);
        assert_eq!(
            client.cell(2, 3).expect("the cell").fg,
            Some(Rgb::new(0, 200, 0))
        );
    }

    #[test]
    fn a_full_update_replaces_whatever_the_client_had_including_its_size() {
        let mut client = Grid::from_lines(&["stale"], 8);
        let fresh = Grid::from_lines(&["one", "two"], 12);
        ScreenUpdate::full(fresh.clone())
            .apply(&mut client)
            .expect("a full screen always applies");
        assert_eq!(client, fresh);
    }

    /// The number that keeps a scrolled viewport still. A client measures its scroll
    /// offset from the live screen, so it has to be told how much history there is on
    /// every update — not only on the full screens.
    #[test]
    fn a_row_update_carries_how_much_history_sits_above_the_screen() {
        let before = prompt_screen();
        let mut after = before.clone();
        write_row(&mut after, 1, "one line of output");
        after.scrollback_len = 41;

        let update = ScreenUpdate::between(&before, &after);
        match &update {
            ScreenUpdate::Rows { scrollback_len, .. } => assert_eq!(*scrollback_len, 41),
            other => panic!("one row changed: {other:?}"),
        }

        let mut client = before.clone();
        update.apply(&mut client).expect("the update applies");
        assert_eq!(
            client.scrollback_len, 41,
            "without this a client cannot know history exists, let alone how much"
        );
        assert!(client.can_scroll_back());

        // And it costs nothing until a pane has scrolled.
        let quiet = ScreenUpdate::between(&before, &before);
        let json = serde_json::to_string(&quiet).expect("it serialises");
        assert!(!json.contains("scrollback_len"), "got {json}");
    }

    #[test]
    fn both_shapes_of_update_round_trip_through_json_with_their_tag() {
        let full = ScreenUpdate::full(Grid::blank(2, 4));
        let json = serde_json::to_string(&full).expect("an update serialises");
        assert!(json.starts_with("{\"mode\":\"full\""), "got {json}");
        assert_eq!(
            serde_json::from_str::<ScreenUpdate>(&json).expect("and reads back"),
            full
        );

        let rows = ScreenUpdate::Rows {
            size: PtySize::new(2, 4),
            cursor: Some((1, 1)),
            alternate_screen: false,
            scrollback_len: 0,
            images: None,
            notices: None,
            rows: Vec::new(),
        };
        let json = serde_json::to_string(&rows).expect("an update serialises");
        assert_eq!(
            json,
            "{\"mode\":\"rows\",\"size\":{\"rows\":2,\"cols\":4},\"cursor\":[1,1],\"rows\":[]}"
        );
        assert_eq!(
            serde_json::from_str::<ScreenUpdate>(&json).expect("and reads back"),
            rows
        );
    }

    /// A link is text the user can act on, so an update that leaves it behind produces a
    /// pane where output has quietly stopped being clickable.
    #[test]
    fn a_row_update_carries_the_links_and_the_wrap_flag_of_the_rows_it_replaces() {
        let mut before = Grid::blank(6, 30);
        write_row(&mut before, 2, "see https://example.com/pr/9");
        let mut after = before.clone();
        assert!(after.set_row_meta(
            2,
            RowMeta {
                wrapped: true,
                links: vec![RowLink::new(4, 28, "https://example.com/pr/9")],
            }
        ));

        let update = ScreenUpdate::between(&before, &after);
        assert!(
            !update.is_full(),
            "a link appearing is one row, not a screen: {update:?}"
        );
        assert_eq!(update.row_count(), 1);

        // Through JSON, because that is the only form a client ever sees.
        let wire = serde_json::to_string(&update).expect("an update serialises");
        let arrived: ScreenUpdate = serde_json::from_str(&wire).expect("and reads back");
        let mut client = before.clone();
        arrived.apply(&mut client).expect("the update applies");
        assert_eq!(client, after);
        assert_eq!(
            client.link_at(2, 10).map(|link| link.uri.as_str()),
            Some("https://example.com/pr/9")
        );
        assert!(client.row_wrapped(2));
        assert!(!client.row_wrapped(1));
    }

    /// A screen with no links must not pay for the fields that carry them.
    #[test]
    fn a_row_with_no_links_and_no_wrap_costs_nothing_on_the_wire() {
        let before = prompt_screen();
        let mut after = before.clone();
        write_row(&mut after, 0, "~/turn on main $ x");
        let wire = serde_json::to_string(&ScreenUpdate::between(&before, &after))
            .expect("an update serialises");
        assert!(!wire.contains("wrapped"), "got {wire}");
        assert!(!wire.contains("links"), "got {wire}");
    }

    /// A peer sending a span no client could resolve has a defect. Ignoring it quietly
    /// would leave the two ends disagreeing about what is clickable.
    #[test]
    fn an_impossible_link_span_in_a_row_update_is_refused_rather_than_dropped() {
        let mut client = Grid::blank(4, 8);
        let row = |links: Vec<RowLink>| ScreenUpdate::Rows {
            size: PtySize::new(4, 8),
            cursor: None,
            alternate_screen: false,
            scrollback_len: 0,
            images: None,
            notices: None,
            rows: vec![GridRow {
                row: 1,
                runs: vec![CellRun {
                    text: String::new(),
                    cells: 8,
                    fg: None,
                    bg: None,
                    attrs: CellAttrs::default(),
                }],
                wrapped: false,
                links,
            }],
        };

        // Past the right margin.
        assert!(matches!(
            row(vec![RowLink::new(4, 40, "https://a.example")]).apply(&mut client),
            Err(GridError::LinkRange { .. })
        ));
        // Empty, so no cell could ever be under it.
        assert!(matches!(
            row(vec![RowLink::new(3, 3, "https://a.example")]).apply(&mut client),
            Err(GridError::LinkRange { .. })
        ));
        // Two links fighting over one column.
        assert!(matches!(
            row(vec![
                RowLink::new(0, 5, "https://a.example"),
                RowLink::new(4, 8, "https://b.example"),
            ])
            .apply(&mut client),
            Err(GridError::LinkOverlap { row: 1, col: 4 })
        ));
        // A URI no browser would accept, sent to make one screen expensive.
        assert!(matches!(
            row(vec![RowLink::new(
                0,
                8,
                format!("https://a.example/{}", "x".repeat(5_000))
            )])
            .apply(&mut client),
            Err(GridError::LinkUriLength { .. })
        ));
        assert_eq!(client, Grid::blank(4, 8), "and nothing was written");
    }

    /// The case the whole "images do not ride in the update" decision is about: a picture
    /// scrolling up the screen changes rows constantly and its table never, so the table
    /// must not be resent — and a client that never hears about it must keep the one it
    /// has.
    #[test]
    fn a_picture_that_only_scrolls_costs_rows_and_never_its_table_again() {
        use crate::images::{GridImage, ImageCell, ImageId};

        let table = vec![GridImage::new(0, ImageId(0xabc), 2, 4, 40, 20)];
        let mut before = Grid::blank(10, 20);
        for dy in 0..2u16 {
            for dx in 0..4u16 {
                if let Some(cell) = before.cell_mut(4 + dy, dx) {
                    *cell = crate::cells::Cell::image(ImageCell::new(0, dy, dx))
                        .expect("an addressable tile");
                }
            }
        }
        before.images = table.clone();

        // The same screen one row further up: the tiles moved, the table did not.
        let mut after = Grid::blank(10, 20);
        for row in 1..10u16 {
            let cells = before.row(row).to_vec();
            after.set_row(row - 1, &cells);
        }
        after.images = table.clone();

        let update = ScreenUpdate::between(&before, &after);
        match &update {
            ScreenUpdate::Rows { images, .. } => assert_eq!(
                *images, None,
                "an unchanged table must be silence, not a repeated payload"
            ),
            other => panic!("a two-row scroll must not send the screen: {other:?}"),
        }
        let mut client = before.clone();
        update.apply(&mut client).expect("the update applies");
        assert_eq!(client.images, table, "the client keeps the table it had");
        assert_eq!(client, after);
    }

    /// And the other direction: a screen that has lost its last picture has to say so, or
    /// a client would keep a table entry for a payload it can no longer draw.
    #[test]
    fn a_screen_that_lost_its_picture_sends_an_empty_table_rather_than_nothing() {
        use crate::images::{GridImage, ImageCell, ImageId};

        let mut before = Grid::blank(6, 10);
        if let Some(cell) = before.cell_mut(0, 0) {
            *cell = crate::cells::Cell::image(ImageCell::new(1, 0, 0)).expect("a tile");
        }
        before.images = vec![GridImage::new(1, ImageId(5), 1, 1, 8, 8)];

        let after = Grid::blank(6, 10);
        let update = ScreenUpdate::between(&before, &after);
        match &update {
            ScreenUpdate::Rows { images, .. } => assert_eq!(*images, Some(Vec::new())),
            // A one-row screen change may legitimately be sent whole; either way the
            // client must end up with no table.
            ScreenUpdate::Full { grid } => assert!(grid.images.is_empty()),
        }
        let mut client = before.clone();
        update.apply(&mut client).expect("the update applies");
        assert!(client.images.is_empty());
        assert!(!client.has_images());
    }

    /// A table a client would go on to index by slot is refused before it replaces the
    /// one the client already trusts.
    #[test]
    fn an_impossible_image_table_in_a_row_update_is_refused_and_changes_nothing() {
        use crate::images::{GridImage, ImageId, MAX_PLACED_IMAGES};

        let mut client = Grid::blank(4, 8);
        let update = ScreenUpdate::Rows {
            size: PtySize::new(4, 8),
            cursor: None,
            alternate_screen: false,
            scrollback_len: 0,
            images: Some(vec![GridImage::new(
                MAX_PLACED_IMAGES as u8,
                ImageId(1),
                1,
                1,
                8,
                8,
            )]),
            notices: None,
            rows: Vec::new(),
        };
        assert!(matches!(
            update.apply(&mut client),
            Err(GridError::Image(_))
        ));
        assert!(client.images.is_empty(), "and nothing was written");
    }

    /// The table costs nothing on a screen with no pictures, which is nearly all of them.
    #[test]
    fn an_update_for_a_screen_with_no_pictures_says_nothing_about_images() {
        let before = prompt_screen();
        let mut after = before.clone();
        write_row(&mut after, 1, "one line");
        let json = serde_json::to_string(&ScreenUpdate::between(&before, &after))
            .expect("an update serialises");
        assert!(!json.contains("images"), "got {json}");
    }

    #[test]
    fn cells_are_the_stream_a_client_gets_without_asking() {
        assert!(PaneStream::default().is_cells());
        assert!(!PaneStream::default().is_bytes());
        assert_eq!(
            serde_json::to_string(&PaneStream::Bytes).expect("it serialises"),
            "\"bytes\""
        );
        assert_eq!(
            serde_json::from_str::<PaneStream>("\"cells\"").expect("it reads back"),
            PaneStream::Cells
        );
    }
}
