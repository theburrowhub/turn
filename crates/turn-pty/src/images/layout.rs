//! How big a picture is, in cells.
//!
//! All three protocols let a program ask for a size, and they ask in different units:
//! iTerm2 in cells, pixels or a percentage of the pane; Kitty in cells; Sixel not at all.
//! Turning any of those into a rectangle of character cells is this module, and it is
//! separate from the protocols so the rule is written once and tested without a terminal.
//!
//! ## Why the daemon decides the box and the client decides the fit
//!
//! The daemon has to choose the **cell box**, because the box is what the markers occupy
//! and the markers are what scroll. But the daemon does not know how many pixels a cell
//! is: that is measured from the font by whoever is drawing, and two clients attached to
//! the same pane at different font sizes would measure differently.
//!
//! So the split is: the daemon picks a box in cells, and the client fits the picture inside
//! that box preserving its aspect ratio. A cell size the daemon guessed wrong by ten
//! percent changes how much of the pane a picture claims; it cannot make the picture come
//! out stretched, which is the failure that would actually be visible.
//!
//! [`NOMINAL_CELL_PIXELS`] is the guess used until a client says otherwise, and
//! `TerminalBuffer::set_cell_pixels` is how it says so.

use turn_proto::{MAX_IMAGE_CELL_COLS, MAX_IMAGE_CELL_ROWS};

/// The cell size assumed for converting a *pixel* size into cells, before any client has
/// reported the size it actually measured.
///
/// This is not a rendering constant and it never reaches the renderer: the client measures
/// its own cell from the font, and this value only decides how many cells a program's
/// `width=400px` claims. A guess is unavoidable — the request arrives at the daemon, which
/// has no font — and 8 by 17 is what the conventional terminal cell is at a common size, so
/// a program's pixel request lands roughly where its author expected.
pub const NOMINAL_CELL_PIXELS: (u16, u16) = (8, 17);

/// A size a program asked for, in whichever unit it used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SizeSpec {
    /// Not specified: the picture's own size decides.
    #[default]
    Auto,
    /// A number of character cells.
    Cells(u32),
    /// A number of pixels, converted with the cell size.
    Pixels(u32),
    /// A percentage of the pane in this direction.
    Percent(u32),
}

impl SizeSpec {
    pub fn is_auto(&self) -> bool {
        matches!(self, SizeSpec::Auto)
    }
}

/// What a program asked for, before it is turned into a rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoxRequest {
    pub width: SizeSpec,
    pub height: SizeSpec,
    /// Whether the picture must keep its shape inside the box. True unless the program
    /// explicitly said otherwise, which is the default in all three protocols.
    pub preserve_aspect: bool,
}

impl Default for BoxRequest {
    fn default() -> Self {
        Self {
            width: SizeSpec::Auto,
            height: SizeSpec::Auto,
            preserve_aspect: true,
        }
    }
}

/// The screen a picture is being placed on, as far as sizing is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Viewport {
    pub rows: u16,
    pub cols: u16,
    /// Pixels per cell, horizontally then vertically.
    pub cell: (u16, u16),
    /// Columns left between the cursor and the right margin.
    pub room_cols: u16,
}

impl Viewport {
    pub fn new(rows: u16, cols: u16, cell: (u16, u16), room_cols: u16) -> Self {
        Self {
            rows: rows.max(1),
            cols: cols.max(1),
            // A zero would divide by zero on the way to a cell count.
            cell: (cell.0.max(1), cell.1.max(1)),
            room_cols: room_cols.max(1),
        }
    }
}

/// A rectangle of cells a picture will occupy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellBox {
    pub rows: u16,
    pub cols: u16,
}

/// Works out the cell box for a picture.
///
/// `pixels` is the decoded picture's own size. The result is always at least one cell, at
/// most [`MAX_IMAGE_CELL_ROWS`] by [`MAX_IMAGE_CELL_COLS`], and never wider than the room
/// left on the line — a picture that ran past the right margin would have half its markers
/// wrapped onto the next row, which is a picture cut in half.
pub fn resolve(request: BoxRequest, pixels: (u32, u32), view: Viewport) -> CellBox {
    let (image_w, image_h) = (pixels.0.max(1), pixels.1.max(1));
    let max_cols = MAX_IMAGE_CELL_COLS.min(view.room_cols);
    let max_rows = MAX_IMAGE_CELL_ROWS.min(view.rows);

    let asked_cols = cells_for(request.width, view.cols, view.cell.0, image_w);
    let asked_rows = cells_for(request.height, view.rows, view.cell.1, image_h);

    let (cols, rows) = match (request.width.is_auto(), request.height.is_auto()) {
        // Neither given: the picture's natural size in cells.
        (true, true) => (asked_cols, asked_rows),
        // One given and the shape must be kept: the other follows from the aspect ratio,
        // which is the behaviour `width=40` is asking for.
        (false, true) if request.preserve_aspect => {
            let cols = asked_cols.clamp(1, max_cols);
            (cols, rows_for_cols(cols, image_w, image_h, view.cell))
        }
        (true, false) if request.preserve_aspect => {
            let rows = asked_rows.clamp(1, max_rows);
            (cols_for_rows(rows, image_w, image_h, view.cell), rows)
        }
        // One given without aspect preservation, or both given: each direction stands as
        // asked, and the client letterboxes the picture inside the box.
        _ => (asked_cols, asked_rows),
    };

    CellBox {
        rows: rows.clamp(1, max_rows),
        cols: cols.clamp(1, max_cols),
    }
}

/// One direction of a request, in cells.
fn cells_for(spec: SizeSpec, span_cells: u16, cell_pixels: u16, image_pixels: u32) -> u16 {
    let cells = match spec {
        // Rounded up: a picture 12 pixels wide in an 8-pixel cell needs two columns, and
        // rounding down would crop it.
        SizeSpec::Auto => image_pixels.div_ceil(cell_pixels as u32),
        SizeSpec::Cells(n) => n,
        SizeSpec::Pixels(p) => p.div_ceil(cell_pixels as u32),
        // Rounded down: `width=100%` must not ask for one column more than the pane has.
        SizeSpec::Percent(p) => (span_cells as u32 * p.min(100)) / 100,
    };
    cells.clamp(1, u16::MAX as u32) as u16
}

/// The rows that keep a picture's shape at a given width.
fn rows_for_cols(cols: u16, image_w: u32, image_h: u32, cell: (u16, u16)) -> u16 {
    let pixels_wide = cols as u64 * cell.0 as u64;
    let pixels_high = pixels_wide * image_h as u64 / image_w.max(1) as u64;
    (pixels_high.div_ceil(cell.1 as u64)).clamp(1, u16::MAX as u64) as u16
}

/// The columns that keep a picture's shape at a given height.
fn cols_for_rows(rows: u16, image_w: u32, image_h: u32, cell: (u16, u16)) -> u16 {
    let pixels_high = rows as u64 * cell.1 as u64;
    let pixels_wide = pixels_high * image_w as u64 / image_h.max(1) as u64;
    (pixels_wide.div_ceil(cell.0 as u64)).clamp(1, u16::MAX as u64) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view() -> Viewport {
        Viewport::new(40, 120, (8, 17), 120)
    }

    #[test]
    fn a_picture_with_no_size_asked_for_takes_the_cells_its_pixels_need() {
        // 80 by 51 pixels in an 8 by 17 cell is ten columns and three rows exactly.
        let placed = resolve(BoxRequest::default(), (80, 51), view());
        assert_eq!(placed, CellBox { rows: 3, cols: 10 });

        // And a picture that does not divide evenly gets the cell it spills into rather
        // than being cropped.
        let spilling = resolve(BoxRequest::default(), (81, 52), view());
        assert_eq!(spilling, CellBox { rows: 4, cols: 11 });
    }

    #[test]
    fn a_size_in_cells_is_taken_at_its_word() {
        let request = BoxRequest {
            width: SizeSpec::Cells(20),
            height: SizeSpec::Cells(6),
            preserve_aspect: true,
        };
        assert_eq!(
            resolve(request, (400, 400), view()),
            CellBox { rows: 6, cols: 20 },
            "both given means both honoured; the client letterboxes inside the box"
        );
    }

    #[test]
    fn a_size_in_pixels_is_converted_with_the_cell_size() {
        let request = BoxRequest {
            width: SizeSpec::Pixels(400),
            height: SizeSpec::Pixels(170),
            preserve_aspect: false,
        };
        assert_eq!(
            resolve(request, (1_000, 1_000), view()),
            CellBox { rows: 10, cols: 50 }
        );
    }

    #[test]
    fn a_percentage_is_of_the_pane_and_never_one_column_more_than_it_has() {
        let request = BoxRequest {
            width: SizeSpec::Percent(50),
            height: SizeSpec::Percent(25),
            preserve_aspect: false,
        };
        assert_eq!(
            resolve(request, (100, 100), view()),
            CellBox { rows: 10, cols: 60 }
        );

        let full = BoxRequest {
            width: SizeSpec::Percent(100),
            height: SizeSpec::Percent(100),
            preserve_aspect: false,
        };
        let placed = resolve(full, (100, 100), view());
        assert!(placed.cols <= 120 && placed.rows <= 40, "{placed:?}");

        // A percentage over a hundred is a program being wrong, not a licence to overflow.
        let absurd = BoxRequest {
            width: SizeSpec::Percent(4_000),
            ..full
        };
        assert!(resolve(absurd, (100, 100), view()).cols <= 120);
    }

    /// `width=40` on its own is asking for a picture forty columns wide *and still the
    /// right shape*. Deriving the height is what makes that true.
    #[test]
    fn one_dimension_given_derives_the_other_from_the_aspect_ratio() {
        let request = BoxRequest {
            width: SizeSpec::Cells(40),
            height: SizeSpec::Auto,
            preserve_aspect: true,
        };
        // A square picture 40 columns wide is 320 pixels wide, so 320 pixels tall, which
        // is 19 rows of a 17-pixel cell.
        assert_eq!(
            resolve(request, (500, 500), view()),
            CellBox { rows: 19, cols: 40 }
        );

        // Twice as wide as it is tall halves the rows.
        assert_eq!(
            resolve(request, (1_000, 500), view()),
            CellBox { rows: 10, cols: 40 }
        );

        // And the other way round.
        let by_height = BoxRequest {
            width: SizeSpec::Auto,
            height: SizeSpec::Cells(10),
            preserve_aspect: true,
        };
        assert_eq!(
            resolve(by_height, (1_000, 500), view()),
            CellBox { rows: 10, cols: 43 }
        );
    }

    #[test]
    fn one_dimension_given_without_aspect_preservation_leaves_the_other_natural() {
        let request = BoxRequest {
            width: SizeSpec::Cells(40),
            height: SizeSpec::Auto,
            preserve_aspect: false,
        };
        // 34 pixels tall is two rows, whatever the width became.
        assert_eq!(
            resolve(request, (500, 34), view()),
            CellBox { rows: 2, cols: 40 }
        );
    }

    /// A picture that ran past the right margin would have half its markers wrapped onto
    /// the next row, which is a picture cut in half rather than a picture that is too wide.
    #[test]
    fn a_box_never_runs_past_the_room_left_on_the_line() {
        let narrow = Viewport::new(40, 120, (8, 17), 10);
        let request = BoxRequest {
            width: SizeSpec::Cells(60),
            height: SizeSpec::Cells(4),
            preserve_aspect: false,
        };
        assert_eq!(resolve(request, (500, 500), narrow).cols, 10);
    }

    #[test]
    fn a_box_never_exceeds_what_a_marker_can_address() {
        let huge = Viewport::new(400, 400, (8, 17), 400);
        let request = BoxRequest {
            width: SizeSpec::Cells(10_000),
            height: SizeSpec::Cells(10_000),
            preserve_aspect: false,
        };
        let placed = resolve(request, (100, 100), huge);
        assert_eq!(placed.cols, MAX_IMAGE_CELL_COLS);
        assert_eq!(placed.rows, MAX_IMAGE_CELL_ROWS);
    }

    #[test]
    fn a_box_is_always_at_least_one_cell_so_a_tiny_picture_still_has_somewhere_to_go() {
        let request = BoxRequest {
            width: SizeSpec::Cells(0),
            height: SizeSpec::Percent(0),
            preserve_aspect: false,
        };
        assert_eq!(
            resolve(request, (1, 1), view()),
            CellBox { rows: 1, cols: 1 }
        );
    }

    /// A cell size of zero arrives from a client that measured a pane mid-layout, and must
    /// not divide by zero on the way to a cell count.
    #[test]
    fn an_impossible_cell_size_is_clamped_rather_than_dividing_by_zero() {
        let broken = Viewport::new(24, 80, (0, 0), 80);
        assert_eq!(broken.cell, (1, 1));
        let placed = resolve(BoxRequest::default(), (10, 10), broken);
        assert_eq!(placed, CellBox { rows: 10, cols: 10 });
    }

    #[test]
    fn a_picture_with_no_pixels_does_not_divide_by_zero_either() {
        let request = BoxRequest {
            width: SizeSpec::Cells(10),
            height: SizeSpec::Auto,
            preserve_aspect: true,
        };
        let placed = resolve(request, (0, 0), view());
        assert!(placed.rows >= 1 && placed.cols >= 1, "{placed:?}");
    }

    /// The reported cell size is what makes a pixel request land where its author meant.
    #[test]
    fn a_client_that_reported_its_measured_cell_changes_how_many_cells_a_pixel_size_claims() {
        let request = BoxRequest {
            width: SizeSpec::Pixels(320),
            height: SizeSpec::Auto,
            preserve_aspect: false,
        };
        let nominal = Viewport::new(40, 120, NOMINAL_CELL_PIXELS, 120);
        assert_eq!(resolve(request, (320, 100), nominal).cols, 40);

        // A retina cell measured at 16 pixels wide halves the columns for the same
        // request, which is what keeps the picture the same physical size.
        let retina = Viewport::new(40, 120, (16, 34), 120);
        assert_eq!(resolve(request, (320, 100), retina).cols, 20);
    }
}
