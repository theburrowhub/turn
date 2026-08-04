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

use crate::cells::{decode_runs, CellRun, Grid, GridError};
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

/// One row of a screen, in the grid's own run encoding.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GridRow {
    pub row: u16,
    pub runs: Vec<CellRun>,
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
        rows: Vec<GridRow>,
    },
}

fn is_false(value: &bool) -> bool {
    !*value
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
            rows: changed
                .into_iter()
                .map(|row| GridRow {
                    row,
                    runs: next.row_runs(row),
                })
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
                    if !target.set_row(row.row, &cells) {
                        return Err(GridError::RowOutOfRange {
                            row: row.row,
                            rows: target.rows,
                        });
                    }
                }
                target.cursor = *cursor;
                target.alternate_screen = *alternate_screen;
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
            rows: vec![GridRow {
                row: 9,
                runs: vec![CellRun {
                    text: String::new(),
                    cells: 8,
                    fg: None,
                    bg: None,
                    attrs: CellAttrs::default(),
                }],
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
