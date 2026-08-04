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

use crate::cells::Grid;

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

/// A selection in progress or finished.
///
/// Stored as anchor and head rather than as start and end so that dragging backwards
/// works without the selection flipping inside out, and so the head is always where
/// the pointer is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub anchor: CellPos,
    pub head: CellPos,
    pub kind: SelectionKind,
}

impl Selection {
    pub fn new(anchor: CellPos, kind: SelectionKind) -> Self {
        Self {
            anchor,
            head: anchor,
            kind,
        }
    }

    /// Moves the loose end.
    pub fn extend_to(&mut self, head: CellPos) {
        self.head = head;
    }

    /// The two ends in reading order.
    pub fn ordered(&self) -> (CellPos, CellPos) {
        if (self.anchor.row, self.anchor.col) <= (self.head.row, self.head.col) {
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
        let (start, end) = self.ordered();
        match self.kind {
            SelectionKind::Block => {
                let (left, right) = if start.col <= end.col {
                    (start.col, end.col)
                } else {
                    (end.col, start.col)
                };
                row >= start.row && row <= end.row && col >= left && col < right.max(left + 1)
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

    /// The selected text, ready for the clipboard.
    ///
    /// Rows are joined with newlines and each row's trailing blanks are dropped: a
    /// terminal pads every row to the full width, and copying that padding is the
    /// difference between a usable clipboard and one full of spaces.
    pub fn text(&self, grid: &Grid) -> String {
        if self.is_empty() {
            return String::new();
        }
        let (start, end) = self.ordered();
        let mut lines: Vec<String> = Vec::new();
        for row in start.row..=end.row.min(grid.rows.saturating_sub(1)) {
            let mut line = String::new();
            for col in 0..grid.cols {
                if !self.contains(row, col) {
                    continue;
                }
                match grid.cell(row, col) {
                    // A wide cell's trailer carries no glyph; including a space for it
                    // would put a gap inside every emoji.
                    Some(cell) if cell.is_trailer() => {}
                    Some(cell) if cell.text.is_empty() => line.push(' '),
                    Some(cell) => line.push_str(&cell.text),
                    None => {}
                }
            }
            lines.push(line.trim_end().to_string());
        }
        lines.join("\n")
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

#[cfg(test)]
mod tests {
    use super::*;

    fn grid() -> Grid {
        Grid::from_lines(&["hello world", "second line", "third"], 20)
    }

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

    /// A wide glyph is one glyph, and its trailer must not become a space.
    #[test]
    fn a_wide_glyph_is_copied_once_and_not_padded_by_its_trailer() {
        let mut grid = Grid::from_lines(&["a  b"], 6);
        assert!(grid.set_wide(0, 1, "漢"));
        let mut selection = Selection::new(CellPos::new(0, 0), SelectionKind::Linear);
        selection.extend_to(CellPos::new(0, 4));
        assert_eq!(selection.text(&grid), "a漢b");
    }

    #[test]
    fn an_empty_selection_copies_nothing_so_a_click_does_not_clear_the_clipboard() {
        let selection = Selection::new(CellPos::new(1, 3), SelectionKind::Linear);
        assert!(selection.is_empty());
        assert_eq!(selection.text(&grid()), "");
        assert!(!selection.contains(1, 3));
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
}
