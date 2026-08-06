//! Where each cell of a pane lands on the screen.
//!
//! A terminal is a lattice, not a paragraph. If a row is drawn as text and left to the
//! font's own advances, the row's columns land wherever the accumulated float arithmetic
//! puts them: two panes disagree, a border drawn on row 3 misses the border drawn on row
//! 4, and a box-drawing frame comes out as loose doubled pipes. So every cell is placed
//! from its own `(row, col)` — never by advancing a cursor — and every edge is rounded to
//! a whole physical pixel.
//!
//! Rounding **per cell** is the point. `origin.x + col * cell.x`, rounded, gives column
//! `col` the same left edge as column `col - 1`'s right edge, for every column, at any
//! cell size and any scale factor. Accumulating `x += cell.x` cannot promise that, and the
//! gap it leaves is exactly what a box-drawing character shows up.

use egui::emath::GuiRounding as _;
use egui::{Pos2, Rect, Vec2};

/// The placement of a pane's cells: where row and column zero begin, how big a cell is,
/// and how many physical pixels a point is worth.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CellGrid {
    origin: Pos2,
    cell: Vec2,
    pixels_per_point: f32,
}

impl CellGrid {
    /// `cell` comes from [`crate::theme::Theme::cell_size`], which measures it from the
    /// font; a caller with no measurement has nothing to construct this with, which is
    /// how "paint nothing rather than guess" is enforced by the types.
    pub fn new(origin: Pos2, cell: Vec2, pixels_per_point: f32) -> Self {
        Self {
            origin,
            cell,
            // A non-positive scale would put every edge at zero. Ignoring it rather than
            // dividing by it keeps the lattice usable during a display change.
            pixels_per_point: if pixels_per_point > 0.0 {
                pixels_per_point
            } else {
                1.0
            },
        }
    }

    pub fn origin(&self) -> Pos2 {
        self.origin
    }

    pub fn cell(&self) -> Vec2 {
        self.cell
    }

    pub fn pixels_per_point(&self) -> f32 {
        self.pixels_per_point
    }

    /// The rectangle covering `cols` columns from (`row`, `col`).
    ///
    /// Built from the two boundaries rather than from a position and a size, so a span and
    /// the cells inside it cover exactly the same pixels: a background painted per run and
    /// a glyph painted per cell cannot disagree by a rounding error.
    pub fn span(&self, row: u16, col: u16, cols: u16) -> Rect {
        Rect::from_min_max(
            Pos2::new(self.column_edge(col), self.row_edge(row)),
            Pos2::new(
                self.column_edge(col.saturating_add(cols)),
                self.row_edge(row.saturating_add(1)),
            ),
        )
    }

    /// One cell's rectangle.
    pub fn cell_rect(&self, row: u16, col: u16) -> Rect {
        self.span(row, col, 1)
    }

    /// The left edge of a column, on the pixel grid.
    fn column_edge(&self, col: u16) -> f32 {
        (self.origin.x + col as f32 * self.cell.x).round_to_pixels(self.pixels_per_point)
    }

    /// The top edge of a row, on the pixel grid.
    fn row_edge(&self, row: u16) -> f32 {
        (self.origin.y + row as f32 * self.cell.y).round_to_pixels(self.pixels_per_point)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The awkward case on purpose: a fractional origin and the real measured advance of
    /// the bundled monospace face.
    fn awkward() -> CellGrid {
        CellGrid::new(Pos2::new(10.3, 7.6), Vec2::new(7.82666, 15.125), 2.0)
    }

    /// The property the report is about: a run of horizontals has to be one line. It is one
    /// line only if each cell's right edge is the next cell's left edge, exactly.
    #[test]
    fn adjacent_cells_share_an_edge_exactly_so_a_horizontal_run_has_no_seam() {
        let lattice = awkward();
        for col in 0..200u16 {
            let left = lattice.cell_rect(4, col);
            let right = lattice.cell_rect(4, col + 1);
            assert_eq!(
                left.max.x,
                right.min.x,
                "column {col} leaves a seam before column {}",
                col + 1
            );
            assert_eq!(
                left.y_range(),
                right.y_range(),
                "column {col} sits off its row"
            );
        }
    }

    /// And the same downwards, or a vertical border breaks between every pair of rows.
    #[test]
    fn stacked_cells_share_an_edge_exactly_so_a_vertical_run_has_no_seam() {
        let lattice = awkward();
        for row in 0..200u16 {
            let upper = lattice.cell_rect(row, 9);
            let lower = lattice.cell_rect(row + 1, 9);
            assert_eq!(upper.max.y, lower.min.y, "row {row} leaves a seam");
            assert_eq!(upper.x_range(), lower.x_range());
        }
    }

    #[test]
    fn every_cell_edge_is_a_whole_physical_pixel() {
        let lattice = awkward();
        let scale = lattice.pixels_per_point();
        for col in 0..80u16 {
            for row in 0..40u16 {
                let rect = lattice.cell_rect(row, col);
                for edge in [rect.min.x, rect.max.x, rect.min.y, rect.max.y] {
                    let pixels = edge * scale;
                    assert!(
                        (pixels - pixels.round()).abs() < 1e-3,
                        "the edge {edge} of cell ({row}, {col}) is {pixels} pixels"
                    );
                }
            }
        }
    }

    /// Rounding per cell rather than accumulating: column 79 must be where 79 cells put it,
    /// not half a column further along.
    #[test]
    fn a_far_column_does_not_drift_from_the_grid_it_belongs_to() {
        let lattice = awkward();
        let cell = lattice.cell().x;
        for col in [1u16, 40, 79, 200, 500] {
            let expected = lattice.origin().x + col as f32 * cell;
            let actual = lattice.cell_rect(0, col).min.x;
            assert!(
                (actual - expected).abs() <= 0.5 / lattice.pixels_per_point(),
                "column {col} drifted from {expected} to {actual}"
            );
        }
    }

    /// A run's background and the glyphs inside it are painted from different loops, so
    /// they have to agree to the pixel.
    #[test]
    fn a_span_covers_exactly_the_cells_inside_it() {
        let lattice = awkward();
        let span = lattice.span(3, 5, 7);
        assert_eq!(span.min, lattice.cell_rect(3, 5).min);
        assert_eq!(span.max.x, lattice.cell_rect(3, 11).max.x);
        assert_eq!(span.max.y, lattice.cell_rect(3, 5).max.y);
    }

    /// A wide glyph is one cell of grid state and two columns of screen, and the pane may
    /// ask for the pair as one rectangle.
    #[test]
    fn a_two_column_span_is_twice_a_cell_give_or_take_a_pixel() {
        let lattice = awkward();
        let pair = lattice.span(0, 6, 2);
        assert_eq!(pair.min.x, lattice.cell_rect(0, 6).min.x);
        assert_eq!(pair.max.x, lattice.cell_rect(0, 7).max.x);
        assert!((pair.width() - 2.0 * lattice.cell().x).abs() <= 1.0);
    }

    /// A scale factor of zero arrives for one frame when a display is reconfigured, and
    /// must not collapse every cell onto the origin.
    #[test]
    fn an_impossible_scale_factor_does_not_collapse_the_lattice() {
        let lattice = CellGrid::new(Pos2::ZERO, Vec2::new(8.0, 15.0), 0.0);
        assert_eq!(
            lattice.cell_rect(1, 1),
            Rect::from_min_max(Pos2::new(8.0, 15.0), Pos2::new(16.0, 30.0))
        );
    }
}
