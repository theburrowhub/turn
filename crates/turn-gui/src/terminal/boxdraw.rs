//! Box-drawing and block characters, drawn by Turn instead of by the font.
//!
//! ## Why not just use the glyphs
//!
//! Because they do not meet. A font's `│` is a stroke inside a glyph box sized by the
//! face's ascent and descent, and a terminal row is the face's *line height* — a larger
//! number. Stack two of them and the strokes stop short of each other; that is the "loose
//! and doubled pipes" in the report, and it is visible in the bundled monospace face at
//! Turn's own size. The same goes the other way: `─` is drawn inside its advance, so a run
//! of them is a dashed line whenever the cell is wider than the ink. Every terminal that
//! renders TUIs properly — iTerm2, kitty, WezTerm, Ghostty — draws these characters itself
//! for exactly this reason.
//!
//! ## The rules that make a frame a frame
//!
//! * **Arms reach the cell edge.** A vertical spans the full cell height, a horizontal the
//!   full width, so the neighbour's arm starts where this one ends. With
//!   [`super::geometry::CellGrid`] guaranteeing adjacent cells share an edge, a run is one
//!   unbroken line.
//! * **An arm that turns crosses the middle.** `┌`'s downward arm starts at the *top* edge
//!   of the horizontal band rather than at the centre line, so the corner is solid instead
//!   of notched.
//! * **Whole pixels, no feathering.** Bars are snapped to physical pixels and painted as a
//!   mesh, not as anti-aliased rectangles: a 1-pixel line that egui feathers is a 2-pixel
//!   grey smear, which reads as exactly the softness this module exists to remove. Curves
//!   and diagonals do keep anti-aliasing — a hard-edged arc looks worse than a smooth one.
//!
//! ## What is not perfect
//!
//! Double lines (`╬` and friends) are drawn as two continuous parallel strokes each way,
//! so their crossings are solid where a typeface would leave the classic little hole. The
//! lines meet, which is what the grid needs; the hole is decoration, and inventing per-case
//! junction geometry for a character almost no TUI emits is not worth the code.

use egui::epaint::Mesh;
use egui::{Color32, Painter, Pos2, Rect, Shape, Stroke};

/// How heavy one arm of a junction is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Weight {
    /// No arm in this direction.
    Blank,
    Light,
    Heavy,
    /// Two parallel light strokes.
    Double,
}

/// Short names, so the table below reads as the shapes it describes.
const N: Weight = Weight::Blank;
const L: Weight = Weight::Light;
const H: Weight = Weight::Heavy;
const D: Weight = Weight::Double;

/// The four arms of a junction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Arms {
    up: Weight,
    down: Weight,
    left: Weight,
    right: Weight,
}

impl Arms {
    const fn new(up: Weight, down: Weight, left: Weight, right: Weight) -> Self {
        Self {
            up,
            down,
            left,
            right,
        }
    }
}

/// Which way a dashed line runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Axis {
    Horizontal,
    Vertical,
}

/// Which edge a partial block is measured from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Edge {
    Top,
    Bottom,
    Left,
    Right,
}

/// One corner of a cell, for the quadrant blocks.
struct Quadrant;

impl Quadrant {
    const UPPER_LEFT: u8 = 1 << 0;
    const UPPER_RIGHT: u8 = 1 << 1;
    const LOWER_LEFT: u8 = 1 << 2;
    const LOWER_RIGHT: u8 = 1 << 3;
}

/// What one character is made of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ink {
    /// Arms from the cell's edges towards its middle.
    Join(Arms),
    /// A broken line: `dashes` marks along the axis, with gaps between them.
    Dash {
        weight: Weight,
        dashes: u8,
        axis: Axis,
    },
    /// A quarter turn with a rounded corner. Exactly two arms, both light.
    Arc(Arms),
    /// Corner-to-corner strokes.
    Diagonal { falling: bool, rising: bool },
    /// `eighths` eighths of the cell, filled solid from one edge.
    Part { edge: Edge, eighths: u8 },
    /// The whole cell, at `eighths`/8 of the foreground's opacity.
    Shade(u8),
    /// A set of [`Quadrant`] bits, filled solid.
    Quadrants(u8),
}

/// The geometry Turn paints for one character, in points.
///
/// Returned rather than painted so the properties that matter — a horizontal run is one
/// unbroken bar, a corner is solid, a block fills its cell exactly — are testable without
/// a window or a recorded image.
#[derive(Debug, Clone, PartialEq)]
pub struct Drawing {
    /// Solid bars, already snapped to whole physical pixels.
    pub bars: Vec<Rect>,
    /// Polylines for the arcs and the diagonals, which are not axis-aligned.
    pub curves: Vec<Vec<Pos2>>,
    /// How wide a curve's stroke is, in points.
    pub curve_width: f32,
    /// How opaque the bars are. Below one only for the shade blocks, which are the
    /// foreground seen through a screen door.
    pub opacity: f32,
}

impl Drawing {
    fn bars(bars: Vec<Rect>) -> Self {
        Self {
            bars,
            curves: Vec::new(),
            curve_width: 0.0,
            opacity: 1.0,
        }
    }
}

/// Whether Turn draws this character itself.
pub fn is_drawn(c: char) -> bool {
    ink(c).is_some()
}

/// Paints `c` into `cell`, or reports that the font has to.
///
/// `false` means "not one of ours" — the caller falls back to a glyph, which is what
/// happens for every ordinary character.
pub fn paint(
    painter: &Painter,
    c: char,
    cell: Rect,
    colour: Color32,
    pixels_per_point: f32,
) -> bool {
    let Some(drawing) = drawing(c, cell, pixels_per_point) else {
        return false;
    };
    if !drawing.bars.is_empty() {
        let colour = if drawing.opacity < 1.0 {
            colour.gamma_multiply(drawing.opacity)
        } else {
            colour
        };
        // A mesh rather than `rect_filled`: egui feathers a rectangle's edges by a pixel,
        // which turns a one-pixel rule into a two-pixel smudge. A mesh is passed through
        // untouched, so the bar covers the pixels it says it covers.
        let mut mesh = Mesh::default();
        for bar in &drawing.bars {
            mesh.add_colored_rect(*bar, colour);
        }
        painter.add(Shape::mesh(mesh));
    }
    for curve in &drawing.curves {
        painter.add(Shape::line(
            curve.clone(),
            Stroke::new(drawing.curve_width, colour),
        ));
    }
    true
}

/// The geometry for `c` inside `cell`, or `None` when the font must draw it.
pub fn drawing(c: char, cell: Rect, pixels_per_point: f32) -> Option<Drawing> {
    let ink = ink(c)?;
    let grid = Pixels::new(cell, pixels_per_point);
    Some(match ink {
        Ink::Join(arms) => Drawing::bars(grid.join(arms)),
        Ink::Dash {
            weight,
            dashes,
            axis,
        } => Drawing::bars(grid.dash(weight, dashes, axis)),
        Ink::Arc(arms) => grid.arc(arms),
        Ink::Diagonal { falling, rising } => grid.diagonal(falling, rising),
        Ink::Part { edge, eighths } => Drawing::bars(vec![grid.part(edge, eighths)]),
        Ink::Shade(eighths) => Drawing {
            opacity: f32::from(eighths) / 8.0,
            ..Drawing::bars(vec![grid.whole()])
        },
        Ink::Quadrants(bits) => Drawing::bars(grid.quadrants(bits)),
    })
}

/// A cell in whole physical pixels.
///
/// Every coordinate in here is an integer number of pixels, so the arithmetic that decides
/// whether two bars meet is integer arithmetic. Points are recovered only when a rectangle
/// is handed back.
struct Pixels {
    left: f32,
    right: f32,
    top: f32,
    bottom: f32,
    scale: f32,
}

impl Pixels {
    fn new(cell: Rect, pixels_per_point: f32) -> Self {
        let scale = if pixels_per_point > 0.0 {
            pixels_per_point
        } else {
            1.0
        };
        Self {
            left: (cell.min.x * scale).round(),
            right: (cell.max.x * scale).round(),
            top: (cell.min.y * scale).round(),
            bottom: (cell.max.y * scale).round(),
            scale,
        }
    }

    fn width(&self) -> f32 {
        (self.right - self.left).max(0.0)
    }

    fn height(&self) -> f32 {
        (self.bottom - self.top).max(0.0)
    }

    /// A rectangle back in points.
    fn rect(&self, left: f32, top: f32, right: f32, bottom: f32) -> Rect {
        Rect::from_min_max(
            Pos2::new(left / self.scale, top / self.scale),
            Pos2::new(right / self.scale, bottom / self.scale),
        )
    }

    fn whole(&self) -> Rect {
        self.rect(self.left, self.top, self.right, self.bottom)
    }

    /// How thick a light stroke is, in pixels.
    ///
    /// A twelfth of the line height, floored, and never less than one pixel: at Turn's
    /// default that is the single pixel a terminal rule should be, and it thickens with the
    /// font instead of staying hairline on a large display.
    fn light(&self) -> f32 {
        (self.height() / 12.0).floor().max(1.0)
    }

    fn thickness(&self, weight: Weight) -> f32 {
        match weight {
            Weight::Blank => 0.0,
            Weight::Light => self.light(),
            Weight::Heavy => self.light() * 2.0,
            // Two light strokes and a gap of the same size between them.
            Weight::Double => self.light() * 3.0,
        }
    }

    /// The bands a weight occupies across `extent`, centred on `centre`.
    ///
    /// One band for a light or heavy stroke, two for a double. Each band is a pair of whole
    /// pixels, so bands from different cells that share a centre are identical.
    fn bands(&self, weight: Weight, centre: f32) -> Vec<(f32, f32)> {
        let unit = self.light();
        match weight {
            Weight::Blank => Vec::new(),
            Weight::Light | Weight::Heavy => {
                let thickness = self.thickness(weight);
                let start = (centre - thickness / 2.0).round();
                vec![(start, start + thickness)]
            }
            Weight::Double => {
                let start = (centre - unit * 1.5).round();
                vec![
                    (start, start + unit),
                    (start + unit * 2.0, start + unit * 3.0),
                ]
            }
        }
    }

    fn centre_x(&self) -> f32 {
        (self.left + self.right) / 2.0
    }

    fn centre_y(&self) -> f32 {
        (self.top + self.bottom) / 2.0
    }

    /// The bars of a junction.
    fn join(&self, arms: Arms) -> Vec<Rect> {
        let mut bars = Vec::new();
        let vertical: Vec<(f32, f32)> = self
            .bands(arms.up, self.centre_x())
            .into_iter()
            .chain(self.bands(arms.down, self.centre_x()))
            .collect();
        let horizontal: Vec<(f32, f32)> = self
            .bands(arms.left, self.centre_y())
            .into_iter()
            .chain(self.bands(arms.right, self.centre_y()))
            .collect();
        let across_vertical = union(&vertical);
        let across_horizontal = union(&horizontal);
        let middle_x = self.centre_x().round();
        let middle_y = self.centre_y().round();

        // A line that continues out the far side at the same weight is *one* bar, so a
        // vertical is a vertical rather than two halves that could disagree by a rounding.
        // Arms of different weights meet at the middle instead, which is what `╼` is.
        if arms.up != Weight::Blank && arms.up == arms.down {
            for (start, end) in self.bands(arms.up, self.centre_x()) {
                bars.push(self.rect(start, self.top, end, self.bottom));
            }
        } else {
            if arms.up != Weight::Blank {
                // Past the far side of the perpendicular bands, so a turn is solid rather
                // than notched; to the middle when there is nothing to meet.
                let stop = if arms.down != Weight::Blank {
                    middle_y
                } else {
                    across_horizontal.map_or(middle_y, |(_, bottom)| bottom)
                };
                for (start, end) in self.bands(arms.up, self.centre_x()) {
                    bars.push(self.rect(start, self.top, end, stop));
                }
            }
            if arms.down != Weight::Blank {
                let stop = if arms.up != Weight::Blank {
                    middle_y
                } else {
                    across_horizontal.map_or(middle_y, |(top, _)| top)
                };
                for (start, end) in self.bands(arms.down, self.centre_x()) {
                    bars.push(self.rect(start, stop, end, self.bottom));
                }
            }
        }
        if arms.left != Weight::Blank && arms.left == arms.right {
            for (start, end) in self.bands(arms.left, self.centre_y()) {
                bars.push(self.rect(self.left, start, self.right, end));
            }
        } else {
            if arms.left != Weight::Blank {
                let stop = if arms.right != Weight::Blank {
                    middle_x
                } else {
                    across_vertical.map_or(middle_x, |(_, right)| right)
                };
                for (start, end) in self.bands(arms.left, self.centre_y()) {
                    bars.push(self.rect(self.left, start, stop, end));
                }
            }
            if arms.right != Weight::Blank {
                let stop = if arms.left != Weight::Blank {
                    middle_x
                } else {
                    across_vertical.map_or(middle_x, |(left, _)| left)
                };
                for (start, end) in self.bands(arms.right, self.centre_y()) {
                    bars.push(self.rect(stop, start, self.right, end));
                }
            }
        }
        bars
    }

    /// The bars of a dashed line: `dashes` marks with a gap between each pair.
    fn dash(&self, weight: Weight, dashes: u8, axis: Axis) -> Vec<Rect> {
        let dashes = dashes.max(1) as f32;
        // Marks and gaps alternate and the run starts and ends with a mark, so there is one
        // fewer gap than mark.
        let steps = dashes * 2.0 - 1.0;
        let (from, to, extent) = match axis {
            Axis::Horizontal => (self.left, self.right, self.width()),
            Axis::Vertical => (self.top, self.bottom, self.height()),
        };
        let bands = match axis {
            Axis::Horizontal => self.bands(weight, self.centre_y()),
            Axis::Vertical => self.bands(weight, self.centre_x()),
        };
        let mut bars = Vec::new();
        for mark in 0..dashes as u32 {
            let start = (from + extent * (mark as f32 * 2.0) / steps).round();
            let end = (from + extent * (mark as f32 * 2.0 + 1.0) / steps)
                .round()
                .min(to);
            if end <= start {
                continue;
            }
            for (band_start, band_end) in &bands {
                bars.push(match axis {
                    Axis::Horizontal => self.rect(start, *band_start, end, *band_end),
                    Axis::Vertical => self.rect(*band_start, start, *band_end, end),
                });
            }
        }
        bars
    }

    /// A quarter turn.
    ///
    /// A quarter ellipse whose centre of curvature is the cell corner *between* the two
    /// arms: that is the one curve which leaves the cell vertically where a `│` would and
    /// horizontally where a `─` would, so a rounded frame still meets its own edges. Both
    /// ends land on the band centres a straight arm would use.
    fn arc(&self, arms: Arms) -> Drawing {
        let centre_x = middle(&self.bands(Weight::Light, self.centre_x()));
        let centre_y = middle(&self.bands(Weight::Light, self.centre_y()));
        let corner_x = if arms.right != Weight::Blank {
            self.right
        } else {
            self.left
        };
        let corner_y = if arms.down != Weight::Blank {
            self.bottom
        } else {
            self.top
        };
        // Enough segments that the curve reads as a curve at any cell size, few enough that
        // a screen of them is still cheap.
        const SEGMENTS: usize = 8;
        let mut curve = Vec::with_capacity(SEGMENTS + 1);
        for step in 0..=SEGMENTS {
            let angle = std::f32::consts::FRAC_PI_2 * step as f32 / SEGMENTS as f32;
            let x = corner_x + (centre_x - corner_x) * angle.cos();
            let y = corner_y + (centre_y - corner_y) * angle.sin();
            curve.push(Pos2::new(x / self.scale, y / self.scale));
        }
        Drawing {
            bars: Vec::new(),
            curves: vec![curve],
            curve_width: self.light() / self.scale,
            opacity: 1.0,
        }
    }

    fn diagonal(&self, falling: bool, rising: bool) -> Drawing {
        let mut curves = Vec::new();
        if falling {
            curves.push(vec![
                Pos2::new(self.left / self.scale, self.top / self.scale),
                Pos2::new(self.right / self.scale, self.bottom / self.scale),
            ]);
        }
        if rising {
            curves.push(vec![
                Pos2::new(self.right / self.scale, self.top / self.scale),
                Pos2::new(self.left / self.scale, self.bottom / self.scale),
            ]);
        }
        Drawing {
            bars: Vec::new(),
            curves,
            curve_width: self.light() / self.scale,
            opacity: 1.0,
        }
    }

    /// A fraction of the cell, filled from one edge.
    ///
    /// The dividing line is always measured from the top or the left, never from the edge
    /// the block grows out of. Rounding from opposite ends would put `▀`'s boundary a pixel
    /// away from `▄`'s in a cell of odd height, and the pair would show a seam where a
    /// terminal shows none.
    fn part(&self, edge: Edge, eighths: u8) -> Rect {
        let fraction = f32::from(eighths.min(8)) / 8.0;
        match edge {
            Edge::Top => {
                let split = self.top + (self.height() * fraction).round();
                self.rect(self.left, self.top, self.right, split)
            }
            Edge::Bottom => {
                let split = self.top + (self.height() * (1.0 - fraction)).round();
                self.rect(self.left, split, self.right, self.bottom)
            }
            Edge::Left => {
                let split = self.left + (self.width() * fraction).round();
                self.rect(self.left, self.top, split, self.bottom)
            }
            Edge::Right => {
                let split = self.left + (self.width() * (1.0 - fraction)).round();
                self.rect(split, self.top, self.right, self.bottom)
            }
        }
    }

    fn quadrants(&self, bits: u8) -> Vec<Rect> {
        let middle_x = self.centre_x().round();
        let middle_y = self.centre_y().round();
        let mut bars = Vec::new();
        if bits & Quadrant::UPPER_LEFT != 0 {
            bars.push(self.rect(self.left, self.top, middle_x, middle_y));
        }
        if bits & Quadrant::UPPER_RIGHT != 0 {
            bars.push(self.rect(middle_x, self.top, self.right, middle_y));
        }
        if bits & Quadrant::LOWER_LEFT != 0 {
            bars.push(self.rect(self.left, middle_y, middle_x, self.bottom));
        }
        if bits & Quadrant::LOWER_RIGHT != 0 {
            bars.push(self.rect(middle_x, middle_y, self.right, self.bottom));
        }
        bars
    }
}

/// The outer extent of a set of bands, low edge first: how far a perpendicular arm has to
/// reach to cross all of them.
fn union(bands: &[(f32, f32)]) -> Option<(f32, f32)> {
    let mut low = f32::MAX;
    let mut high = f32::MIN;
    for (start, end) in bands {
        low = low.min(*start);
        high = high.max(*end);
    }
    if bands.is_empty() {
        None
    } else {
        Some((low, high))
    }
}

/// The centre of the first band, which is where a stroke's line should sit.
fn middle(bands: &[(f32, f32)]) -> f32 {
    match bands.first() {
        Some((start, end)) => (start + end) / 2.0,
        None => 0.0,
    }
}

/// What each character is made of, in code-point order.
///
/// Derived from the Unicode names — "BOX DRAWINGS DOWN LIGHT AND RIGHT HEAVY" says exactly
/// which arm has which weight — rather than transcribed by eye, because a table this size
/// read off a chart is a table with three mistakes in it.
///
/// Left aligned by hand: one line per character, with the name it came from beside it, is
/// the only form in which a reviewer can check an entry against the standard. rustfmt would
/// spread the dashed entries over five lines each and take the table to four hundred.
#[rustfmt::skip]
const INK: &[(char, Ink)] = &[
    ('─', Ink::Join(Arms::new(N, N, L, L))),                                                     // Box Drawings Light Horizontal
    ('━', Ink::Join(Arms::new(N, N, H, H))),                                                     // Box Drawings Heavy Horizontal
    ('│', Ink::Join(Arms::new(L, L, N, N))),                                                     // Box Drawings Light Vertical
    ('┃', Ink::Join(Arms::new(H, H, N, N))),                                                     // Box Drawings Heavy Vertical
    ('┄', Ink::Dash { weight: L, dashes: 3, axis: Axis::Horizontal }),                           // Box Drawings Light Triple Dash Horizontal
    ('┅', Ink::Dash { weight: H, dashes: 3, axis: Axis::Horizontal }),                           // Box Drawings Heavy Triple Dash Horizontal
    ('┆', Ink::Dash { weight: L, dashes: 3, axis: Axis::Vertical }),                             // Box Drawings Light Triple Dash Vertical
    ('┇', Ink::Dash { weight: H, dashes: 3, axis: Axis::Vertical }),                             // Box Drawings Heavy Triple Dash Vertical
    ('┈', Ink::Dash { weight: L, dashes: 4, axis: Axis::Horizontal }),                           // Box Drawings Light Quadruple Dash Horizontal
    ('┉', Ink::Dash { weight: H, dashes: 4, axis: Axis::Horizontal }),                           // Box Drawings Heavy Quadruple Dash Horizontal
    ('┊', Ink::Dash { weight: L, dashes: 4, axis: Axis::Vertical }),                             // Box Drawings Light Quadruple Dash Vertical
    ('┋', Ink::Dash { weight: H, dashes: 4, axis: Axis::Vertical }),                             // Box Drawings Heavy Quadruple Dash Vertical
    ('┌', Ink::Join(Arms::new(N, L, N, L))),                                                     // Box Drawings Light Down And Right
    ('┍', Ink::Join(Arms::new(N, L, N, H))),                                                     // Box Drawings Down Light And Right Heavy
    ('┎', Ink::Join(Arms::new(N, H, N, L))),                                                     // Box Drawings Down Heavy And Right Light
    ('┏', Ink::Join(Arms::new(N, H, N, H))),                                                     // Box Drawings Heavy Down And Right
    ('┐', Ink::Join(Arms::new(N, L, L, N))),                                                     // Box Drawings Light Down And Left
    ('┑', Ink::Join(Arms::new(N, L, H, N))),                                                     // Box Drawings Down Light And Left Heavy
    ('┒', Ink::Join(Arms::new(N, H, L, N))),                                                     // Box Drawings Down Heavy And Left Light
    ('┓', Ink::Join(Arms::new(N, H, H, N))),                                                     // Box Drawings Heavy Down And Left
    ('└', Ink::Join(Arms::new(L, N, N, L))),                                                     // Box Drawings Light Up And Right
    ('┕', Ink::Join(Arms::new(L, N, N, H))),                                                     // Box Drawings Up Light And Right Heavy
    ('┖', Ink::Join(Arms::new(H, N, N, L))),                                                     // Box Drawings Up Heavy And Right Light
    ('┗', Ink::Join(Arms::new(H, N, N, H))),                                                     // Box Drawings Heavy Up And Right
    ('┘', Ink::Join(Arms::new(L, N, L, N))),                                                     // Box Drawings Light Up And Left
    ('┙', Ink::Join(Arms::new(L, N, H, N))),                                                     // Box Drawings Up Light And Left Heavy
    ('┚', Ink::Join(Arms::new(H, N, L, N))),                                                     // Box Drawings Up Heavy And Left Light
    ('┛', Ink::Join(Arms::new(H, N, H, N))),                                                     // Box Drawings Heavy Up And Left
    ('├', Ink::Join(Arms::new(L, L, N, L))),                                                     // Box Drawings Light Vertical And Right
    ('┝', Ink::Join(Arms::new(L, L, N, H))),                                                     // Box Drawings Vertical Light And Right Heavy
    ('┞', Ink::Join(Arms::new(H, L, N, L))),                                                     // Box Drawings Up Heavy And Right Down Light
    ('┟', Ink::Join(Arms::new(L, H, N, L))),                                                     // Box Drawings Down Heavy And Right Up Light
    ('┠', Ink::Join(Arms::new(H, H, N, L))),                                                     // Box Drawings Vertical Heavy And Right Light
    ('┡', Ink::Join(Arms::new(H, L, N, H))),                                                     // Box Drawings Down Light And Right Up Heavy
    ('┢', Ink::Join(Arms::new(L, H, N, H))),                                                     // Box Drawings Up Light And Right Down Heavy
    ('┣', Ink::Join(Arms::new(H, H, N, H))),                                                     // Box Drawings Heavy Vertical And Right
    ('┤', Ink::Join(Arms::new(L, L, L, N))),                                                     // Box Drawings Light Vertical And Left
    ('┥', Ink::Join(Arms::new(L, L, H, N))),                                                     // Box Drawings Vertical Light And Left Heavy
    ('┦', Ink::Join(Arms::new(H, L, L, N))),                                                     // Box Drawings Up Heavy And Left Down Light
    ('┧', Ink::Join(Arms::new(L, H, L, N))),                                                     // Box Drawings Down Heavy And Left Up Light
    ('┨', Ink::Join(Arms::new(H, H, L, N))),                                                     // Box Drawings Vertical Heavy And Left Light
    ('┩', Ink::Join(Arms::new(H, L, H, N))),                                                     // Box Drawings Down Light And Left Up Heavy
    ('┪', Ink::Join(Arms::new(L, H, H, N))),                                                     // Box Drawings Up Light And Left Down Heavy
    ('┫', Ink::Join(Arms::new(H, H, H, N))),                                                     // Box Drawings Heavy Vertical And Left
    ('┬', Ink::Join(Arms::new(N, L, L, L))),                                                     // Box Drawings Light Down And Horizontal
    ('┭', Ink::Join(Arms::new(N, L, H, L))),                                                     // Box Drawings Left Heavy And Right Down Light
    ('┮', Ink::Join(Arms::new(N, L, L, H))),                                                     // Box Drawings Right Heavy And Left Down Light
    ('┯', Ink::Join(Arms::new(N, L, H, H))),                                                     // Box Drawings Down Light And Horizontal Heavy
    ('┰', Ink::Join(Arms::new(N, H, L, L))),                                                     // Box Drawings Down Heavy And Horizontal Light
    ('┱', Ink::Join(Arms::new(N, H, H, L))),                                                     // Box Drawings Right Light And Left Down Heavy
    ('┲', Ink::Join(Arms::new(N, H, L, H))),                                                     // Box Drawings Left Light And Right Down Heavy
    ('┳', Ink::Join(Arms::new(N, H, H, H))),                                                     // Box Drawings Heavy Down And Horizontal
    ('┴', Ink::Join(Arms::new(L, N, L, L))),                                                     // Box Drawings Light Up And Horizontal
    ('┵', Ink::Join(Arms::new(L, N, H, L))),                                                     // Box Drawings Left Heavy And Right Up Light
    ('┶', Ink::Join(Arms::new(L, N, L, H))),                                                     // Box Drawings Right Heavy And Left Up Light
    ('┷', Ink::Join(Arms::new(L, N, H, H))),                                                     // Box Drawings Up Light And Horizontal Heavy
    ('┸', Ink::Join(Arms::new(H, N, L, L))),                                                     // Box Drawings Up Heavy And Horizontal Light
    ('┹', Ink::Join(Arms::new(H, N, H, L))),                                                     // Box Drawings Right Light And Left Up Heavy
    ('┺', Ink::Join(Arms::new(H, N, L, H))),                                                     // Box Drawings Left Light And Right Up Heavy
    ('┻', Ink::Join(Arms::new(H, N, H, H))),                                                     // Box Drawings Heavy Up And Horizontal
    ('┼', Ink::Join(Arms::new(L, L, L, L))),                                                     // Box Drawings Light Vertical And Horizontal
    ('┽', Ink::Join(Arms::new(L, L, H, L))),                                                     // Box Drawings Left Heavy And Right Vertical Light
    ('┾', Ink::Join(Arms::new(L, L, L, H))),                                                     // Box Drawings Right Heavy And Left Vertical Light
    ('┿', Ink::Join(Arms::new(L, L, H, H))),                                                     // Box Drawings Vertical Light And Horizontal Heavy
    ('╀', Ink::Join(Arms::new(H, L, L, L))),                                                     // Box Drawings Up Heavy And Down Horizontal Light
    ('╁', Ink::Join(Arms::new(L, H, L, L))),                                                     // Box Drawings Down Heavy And Up Horizontal Light
    ('╂', Ink::Join(Arms::new(H, H, L, L))),                                                     // Box Drawings Vertical Heavy And Horizontal Light
    ('╃', Ink::Join(Arms::new(H, L, H, L))),                                                     // Box Drawings Left Up Heavy And Right Down Light
    ('╄', Ink::Join(Arms::new(H, L, L, H))),                                                     // Box Drawings Right Up Heavy And Left Down Light
    ('╅', Ink::Join(Arms::new(L, H, H, L))),                                                     // Box Drawings Left Down Heavy And Right Up Light
    ('╆', Ink::Join(Arms::new(L, H, L, H))),                                                     // Box Drawings Right Down Heavy And Left Up Light
    ('╇', Ink::Join(Arms::new(H, L, H, H))),                                                     // Box Drawings Down Light And Up Horizontal Heavy
    ('╈', Ink::Join(Arms::new(L, H, H, H))),                                                     // Box Drawings Up Light And Down Horizontal Heavy
    ('╉', Ink::Join(Arms::new(H, H, H, L))),                                                     // Box Drawings Right Light And Left Vertical Heavy
    ('╊', Ink::Join(Arms::new(H, H, L, H))),                                                     // Box Drawings Left Light And Right Vertical Heavy
    ('╋', Ink::Join(Arms::new(H, H, H, H))),                                                     // Box Drawings Heavy Vertical And Horizontal
    ('╌', Ink::Dash { weight: L, dashes: 2, axis: Axis::Horizontal }),                           // Box Drawings Light Double Dash Horizontal
    ('╍', Ink::Dash { weight: H, dashes: 2, axis: Axis::Horizontal }),                           // Box Drawings Heavy Double Dash Horizontal
    ('╎', Ink::Dash { weight: L, dashes: 2, axis: Axis::Vertical }),                             // Box Drawings Light Double Dash Vertical
    ('╏', Ink::Dash { weight: H, dashes: 2, axis: Axis::Vertical }),                             // Box Drawings Heavy Double Dash Vertical
    ('═', Ink::Join(Arms::new(N, N, D, D))),                                                     // Box Drawings Double Horizontal
    ('║', Ink::Join(Arms::new(D, D, N, N))),                                                     // Box Drawings Double Vertical
    ('╒', Ink::Join(Arms::new(N, L, N, D))),                                                     // Box Drawings Down Single And Right Double
    ('╓', Ink::Join(Arms::new(N, D, N, L))),                                                     // Box Drawings Down Double And Right Single
    ('╔', Ink::Join(Arms::new(N, D, N, D))),                                                     // Box Drawings Double Down And Right
    ('╕', Ink::Join(Arms::new(N, L, D, N))),                                                     // Box Drawings Down Single And Left Double
    ('╖', Ink::Join(Arms::new(N, D, L, N))),                                                     // Box Drawings Down Double And Left Single
    ('╗', Ink::Join(Arms::new(N, D, D, N))),                                                     // Box Drawings Double Down And Left
    ('╘', Ink::Join(Arms::new(L, N, N, D))),                                                     // Box Drawings Up Single And Right Double
    ('╙', Ink::Join(Arms::new(D, N, N, L))),                                                     // Box Drawings Up Double And Right Single
    ('╚', Ink::Join(Arms::new(D, N, N, D))),                                                     // Box Drawings Double Up And Right
    ('╛', Ink::Join(Arms::new(L, N, D, N))),                                                     // Box Drawings Up Single And Left Double
    ('╜', Ink::Join(Arms::new(D, N, L, N))),                                                     // Box Drawings Up Double And Left Single
    ('╝', Ink::Join(Arms::new(D, N, D, N))),                                                     // Box Drawings Double Up And Left
    ('╞', Ink::Join(Arms::new(L, L, N, D))),                                                     // Box Drawings Vertical Single And Right Double
    ('╟', Ink::Join(Arms::new(D, D, N, L))),                                                     // Box Drawings Vertical Double And Right Single
    ('╠', Ink::Join(Arms::new(D, D, N, D))),                                                     // Box Drawings Double Vertical And Right
    ('╡', Ink::Join(Arms::new(L, L, D, N))),                                                     // Box Drawings Vertical Single And Left Double
    ('╢', Ink::Join(Arms::new(D, D, L, N))),                                                     // Box Drawings Vertical Double And Left Single
    ('╣', Ink::Join(Arms::new(D, D, D, N))),                                                     // Box Drawings Double Vertical And Left
    ('╤', Ink::Join(Arms::new(N, L, D, D))),                                                     // Box Drawings Down Single And Horizontal Double
    ('╥', Ink::Join(Arms::new(N, D, L, L))),                                                     // Box Drawings Down Double And Horizontal Single
    ('╦', Ink::Join(Arms::new(N, D, D, D))),                                                     // Box Drawings Double Down And Horizontal
    ('╧', Ink::Join(Arms::new(L, N, D, D))),                                                     // Box Drawings Up Single And Horizontal Double
    ('╨', Ink::Join(Arms::new(D, N, L, L))),                                                     // Box Drawings Up Double And Horizontal Single
    ('╩', Ink::Join(Arms::new(D, N, D, D))),                                                     // Box Drawings Double Up And Horizontal
    ('╪', Ink::Join(Arms::new(L, L, D, D))),                                                     // Box Drawings Vertical Single And Horizontal Double
    ('╫', Ink::Join(Arms::new(D, D, L, L))),                                                     // Box Drawings Vertical Double And Horizontal Single
    ('╬', Ink::Join(Arms::new(D, D, D, D))),                                                     // Box Drawings Double Vertical And Horizontal
    ('╭', Ink::Arc(Arms::new(N, L, N, L))),                                                      // Box Drawings Light Arc Down And Right
    ('╮', Ink::Arc(Arms::new(N, L, L, N))),                                                      // Box Drawings Light Arc Down And Left
    ('╯', Ink::Arc(Arms::new(L, N, L, N))),                                                      // Box Drawings Light Arc Up And Left
    ('╰', Ink::Arc(Arms::new(L, N, N, L))),                                                      // Box Drawings Light Arc Up And Right
    ('╱', Ink::Diagonal { falling: false, rising: true }),                                       // Box Drawings Light Diagonal Upper Right To Lower Left
    ('╲', Ink::Diagonal { falling: true, rising: false }),                                       // Box Drawings Light Diagonal Upper Left To Lower Right
    ('╳', Ink::Diagonal { falling: true, rising: true }),                                        // Box Drawings Light Diagonal Cross
    ('╴', Ink::Join(Arms::new(N, N, L, N))),                                                     // Box Drawings Light Left
    ('╵', Ink::Join(Arms::new(L, N, N, N))),                                                     // Box Drawings Light Up
    ('╶', Ink::Join(Arms::new(N, N, N, L))),                                                     // Box Drawings Light Right
    ('╷', Ink::Join(Arms::new(N, L, N, N))),                                                     // Box Drawings Light Down
    ('╸', Ink::Join(Arms::new(N, N, H, N))),                                                     // Box Drawings Heavy Left
    ('╹', Ink::Join(Arms::new(H, N, N, N))),                                                     // Box Drawings Heavy Up
    ('╺', Ink::Join(Arms::new(N, N, N, H))),                                                     // Box Drawings Heavy Right
    ('╻', Ink::Join(Arms::new(N, H, N, N))),                                                     // Box Drawings Heavy Down
    ('╼', Ink::Join(Arms::new(N, N, L, H))),                                                     // Box Drawings Light Left And Heavy Right
    ('╽', Ink::Join(Arms::new(L, H, N, N))),                                                     // Box Drawings Light Up And Heavy Down
    ('╾', Ink::Join(Arms::new(N, N, H, L))),                                                     // Box Drawings Heavy Left And Light Right
    ('╿', Ink::Join(Arms::new(H, L, N, N))),                                                     // Box Drawings Heavy Up And Light Down
    ('▀', Ink::Part { edge: Edge::Top, eighths: 4 }),                                            // Upper Half Block
    ('▁', Ink::Part { edge: Edge::Bottom, eighths: 1 }),                                         // Lower One Eighth Block
    ('▂', Ink::Part { edge: Edge::Bottom, eighths: 2 }),                                         // Lower One Quarter Block
    ('▃', Ink::Part { edge: Edge::Bottom, eighths: 3 }),                                         // Lower Three Eighths Block
    ('▄', Ink::Part { edge: Edge::Bottom, eighths: 4 }),                                         // Lower Half Block
    ('▅', Ink::Part { edge: Edge::Bottom, eighths: 5 }),                                         // Lower Five Eighths Block
    ('▆', Ink::Part { edge: Edge::Bottom, eighths: 6 }),                                         // Lower Three Quarters Block
    ('▇', Ink::Part { edge: Edge::Bottom, eighths: 7 }),                                         // Lower Seven Eighths Block
    ('█', Ink::Part { edge: Edge::Bottom, eighths: 8 }),                                         // Full Block
    ('▉', Ink::Part { edge: Edge::Left, eighths: 7 }),                                           // Left Seven Eighths Block
    ('▊', Ink::Part { edge: Edge::Left, eighths: 6 }),                                           // Left Three Quarters Block
    ('▋', Ink::Part { edge: Edge::Left, eighths: 5 }),                                           // Left Five Eighths Block
    ('▌', Ink::Part { edge: Edge::Left, eighths: 4 }),                                           // Left Half Block
    ('▍', Ink::Part { edge: Edge::Left, eighths: 3 }),                                           // Left Three Eighths Block
    ('▎', Ink::Part { edge: Edge::Left, eighths: 2 }),                                           // Left One Quarter Block
    ('▏', Ink::Part { edge: Edge::Left, eighths: 1 }),                                           // Left One Eighth Block
    ('▐', Ink::Part { edge: Edge::Right, eighths: 4 }),                                          // Right Half Block
    ('░', Ink::Shade(2)),                                                                        // Light Shade
    ('▒', Ink::Shade(4)),                                                                        // Medium Shade
    ('▓', Ink::Shade(6)),                                                                        // Dark Shade
    ('▔', Ink::Part { edge: Edge::Top, eighths: 1 }),                                            // Upper One Eighth Block
    ('▕', Ink::Part { edge: Edge::Right, eighths: 1 }),                                          // Right One Eighth Block
    ('▖', Ink::Quadrants(Quadrant::LOWER_LEFT)),                                                 // Quadrant Lower Left
    ('▗', Ink::Quadrants(Quadrant::LOWER_RIGHT)),                                                // Quadrant Lower Right
    ('▘', Ink::Quadrants(Quadrant::UPPER_LEFT)),                                                 // Quadrant Upper Left
    ('▙', Ink::Quadrants(Quadrant::UPPER_LEFT | Quadrant::LOWER_LEFT | Quadrant::LOWER_RIGHT)),  // Quadrant Upper Left And Lower Left And Lower Right
    ('▚', Ink::Quadrants(Quadrant::UPPER_LEFT | Quadrant::LOWER_RIGHT)),                         // Quadrant Upper Left And Lower Right
    ('▛', Ink::Quadrants(Quadrant::UPPER_LEFT | Quadrant::UPPER_RIGHT | Quadrant::LOWER_LEFT)),  // Quadrant Upper Left And Upper Right And Lower Left
    ('▜', Ink::Quadrants(Quadrant::UPPER_LEFT | Quadrant::UPPER_RIGHT | Quadrant::LOWER_RIGHT)), // Quadrant Upper Left And Upper Right And Lower Right
    ('▝', Ink::Quadrants(Quadrant::UPPER_RIGHT)),                                                // Quadrant Upper Right
    ('▞', Ink::Quadrants(Quadrant::UPPER_RIGHT | Quadrant::LOWER_LEFT)),                         // Quadrant Upper Right And Lower Left
    ('▟', Ink::Quadrants(Quadrant::UPPER_RIGHT | Quadrant::LOWER_LEFT | Quadrant::LOWER_RIGHT)), // Quadrant Upper Right And Lower Left And Lower Right
];

/// What `c` is made of, if Turn draws it.
///
/// A binary search over a table in code-point order: this runs once per box-drawing cell
/// on screen, which for a full-screen TUI is a few thousand times a frame.
fn ink(c: char) -> Option<Ink> {
    INK.binary_search_by_key(&c, |(candidate, _)| *candidate)
        .ok()
        .map(|found| INK[found].1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A cell of the real measured size, at 1x, where a light stroke is one pixel.
    fn cell(row: u16, col: u16) -> Rect {
        Rect::from_min_max(
            Pos2::new(col as f32 * 8.0, row as f32 * 15.0),
            Pos2::new((col + 1) as f32 * 8.0, (row + 1) as f32 * 15.0),
        )
    }

    fn bars(c: char, row: u16, col: u16) -> Vec<Rect> {
        drawing(c, cell(row, col), 1.0)
            .unwrap_or_else(|| panic!("{c} is not drawn"))
            .bars
    }

    /// The table has to stay sorted or the binary search silently stops finding characters.
    #[test]
    fn the_table_is_in_code_point_order_and_has_no_duplicates() {
        for pair in INK.windows(2) {
            assert!(
                pair[0].0 < pair[1].0,
                "{:?} is out of order before {:?}",
                pair[0].0,
                pair[1].0
            );
        }
    }

    /// Everything the report is about, plus every junction and block, must be ours to draw:
    /// leaving any of them to the font is a border that does not meet.
    #[test]
    fn every_box_drawing_and_block_character_is_drawn_by_turn() {
        for c in "─│┌┐└┘├┤┬┴┼█▀▄".chars() {
            assert!(is_drawn(c), "{c} would be left to the font");
        }
        for code in 0x2500..=0x259F_u32 {
            let c = char::from_u32(code).expect("a box-drawing code point");
            assert!(is_drawn(c), "U+{code:04X} {c} would be left to the font");
        }
        // And nothing else: an ordinary letter must still come from the font.
        for c in "Mi ~$aZ→✔".chars() {
            assert!(!is_drawn(c), "{c} must be drawn by the font");
        }
    }

    /// The owner's test: a horizontal run has to look like one line.
    #[test]
    fn a_run_of_horizontals_is_one_unbroken_bar() {
        let mut previous: Option<Rect> = None;
        for col in 0..20u16 {
            let bars = bars('─', 2, col);
            assert_eq!(bars.len(), 1, "a light horizontal is one bar");
            let bar = bars[0];
            assert_eq!(
                bar.min.x,
                cell(2, col).min.x,
                "the bar starts at the cell edge"
            );
            assert_eq!(bar.max.x, cell(2, col).max.x, "and reaches the next cell");
            if let Some(previous) = previous {
                assert_eq!(previous.max.x, bar.min.x, "seam before column {col}");
                assert_eq!(
                    previous.y_range(),
                    bar.y_range(),
                    "column {col} draws its line at a different height"
                );
            }
            previous = Some(bar);
        }
    }

    /// And a vertical run down a border, which is the half the report saw as doubled pipes.
    #[test]
    fn a_column_of_verticals_is_one_unbroken_bar() {
        let mut previous: Option<Rect> = None;
        for row in 0..20u16 {
            let bars = bars('│', row, 3);
            assert_eq!(bars.len(), 1);
            let bar = bars[0];
            assert_eq!(bar.min.y, cell(row, 3).min.y);
            assert_eq!(bar.max.y, cell(row, 3).max.y);
            if let Some(previous) = previous {
                assert_eq!(previous.max.y, bar.min.y, "seam above row {row}");
                assert_eq!(previous.x_range(), bar.x_range());
            }
            previous = Some(bar);
        }
    }

    /// A corner has to be solid: the arms must overlap in the middle rather than stop at the
    /// centre line and leave a notch.
    #[test]
    fn a_corner_joins_its_two_arms_without_a_notch() {
        let frame = cell(0, 0);
        let corner = bars('┌', 0, 0);
        assert_eq!(corner.len(), 2, "one arm down, one arm right");
        let down = corner
            .iter()
            .find(|bar| bar.max.y == frame.max.y)
            .expect("an arm reaching the bottom edge");
        let right = corner
            .iter()
            .find(|bar| bar.max.x == frame.max.x)
            .expect("an arm reaching the right edge");
        assert!(
            down.intersects(*right),
            "the arms of a corner must overlap: {down:?} and {right:?}"
        );
        assert!(
            down.min.y <= right.min.y,
            "the downward arm must start at the top of the horizontal band, not below it"
        );
        // The corner opens the way it should: nothing reaches the top or the left.
        assert!(corner
            .iter()
            .all(|bar| bar.min.y > frame.min.y || bar.max.x == frame.max.x));
    }

    /// The four corners of a frame meet the arms of the cells next to them, which is what
    /// makes a drawn box a box.
    #[test]
    fn the_corners_of_a_frame_meet_the_edges_that_run_into_them() {
        let top_left = bars('┌', 0, 0);
        let top = bars('─', 0, 1);
        let left = bars('│', 1, 0);
        let horizontal = top[0];
        let vertical = left[0];
        let arm_right = top_left
            .iter()
            .find(|bar| bar.max.x == cell(0, 0).max.x)
            .expect("a right arm");
        let arm_down = top_left
            .iter()
            .find(|bar| bar.max.y == cell(0, 0).max.y)
            .expect("a down arm");
        assert_eq!(arm_right.max.x, horizontal.min.x, "corner to horizontal");
        assert_eq!(
            arm_right.y_range(),
            horizontal.y_range(),
            "the corner's arm and the line it meets must be the same rule"
        );
        assert_eq!(arm_down.max.y, vertical.min.y, "corner to vertical");
        assert_eq!(arm_down.x_range(), vertical.x_range());
    }

    /// A cross is a plus, not four stubs.
    #[test]
    fn a_cross_spans_the_whole_cell_both_ways() {
        let frame = cell(5, 5);
        let cross = bars('┼', 5, 5);
        assert!(
            cross
                .iter()
                .any(|bar| bar.min.y == frame.min.y && bar.max.y == frame.max.y),
            "the vertical of a cross must span the cell"
        );
        assert!(
            cross
                .iter()
                .any(|bar| bar.min.x == frame.min.x && bar.max.x == frame.max.x),
            "and so must the horizontal"
        );
    }

    /// A tee's through-line stays unbroken, so a table's top edge does not have a gap at
    /// every column boundary.
    #[test]
    fn a_tees_through_line_is_not_broken_by_its_stem() {
        let frame = cell(0, 4);
        let tee = bars('┬', 0, 4);
        assert!(
            tee.iter()
                .any(|bar| bar.min.x == frame.min.x && bar.max.x == frame.max.x),
            "the horizontal of ┬ must cross the whole cell: {tee:?}"
        );
        assert!(
            tee.iter().any(|bar| bar.max.y == frame.max.y),
            "and the stem must reach the row below"
        );
        assert!(
            tee.iter().all(|bar| bar.min.y >= frame.min.y),
            "nothing may hang above the cell"
        );
    }

    /// Heavy is heavier than light, and both are centred on the same line so a heavy border
    /// still meets a light one.
    #[test]
    fn a_heavy_line_is_thicker_than_a_light_one_and_shares_its_centre() {
        let light = bars('─', 1, 1)[0];
        let heavy = bars('━', 1, 1)[0];
        assert!(
            heavy.height() > light.height(),
            "heavy {} is not thicker than light {}",
            heavy.height(),
            light.height()
        );
        assert!((heavy.center().y - light.center().y).abs() <= 1.0);
    }

    /// A double line is two strokes with daylight between them, not one thick one.
    #[test]
    fn a_double_line_is_two_parallel_strokes() {
        let bars = bars('═', 1, 1);
        assert_eq!(bars.len(), 2, "two strokes: {bars:?}");
        let (first, second) = (bars[0], bars[1]);
        assert!(
            first.max.y < second.min.y || second.max.y < first.min.y,
            "the strokes of a double line must not touch: {first:?} {second:?}"
        );
        for bar in [first, second] {
            assert_eq!(bar.min.x, cell(1, 1).min.x);
            assert_eq!(bar.max.x, cell(1, 1).max.x);
        }
    }

    /// A block has to fill its cell exactly, or a row of them is striped.
    #[test]
    fn a_full_block_fills_its_cell_and_tiles_with_its_neighbour() {
        let filled = bars('█', 3, 3);
        assert_eq!(filled, vec![cell(3, 3)]);
        assert_eq!(bars('█', 3, 4)[0].min.x, filled[0].max.x);
    }

    /// Half and eighth blocks are what a TUI draws a bar chart with: the same fraction must
    /// come out the same height in every cell.
    #[test]
    fn the_partial_blocks_take_the_fraction_of_the_cell_they_name() {
        let frame = cell(0, 0);
        let lower_half = bars('▄', 0, 0)[0];
        assert_eq!(lower_half.max.y, frame.max.y);
        assert!((lower_half.height() - frame.height() / 2.0).abs() <= 0.5);
        assert_eq!(lower_half.x_range(), frame.x_range());

        let upper_half = bars('▀', 0, 0)[0];
        assert_eq!(upper_half.min.y, frame.min.y);
        assert!((upper_half.height() - frame.height() / 2.0).abs() <= 0.5);
        // The two halves together are the whole cell: no seam across the middle.
        assert_eq!(upper_half.max.y, lower_half.min.y);

        let left_half = bars('▌', 0, 0)[0];
        let right_half = bars('▐', 0, 0)[0];
        assert_eq!(left_half.max.x, right_half.min.x);
        assert_eq!(left_half.min.x, frame.min.x);
        assert_eq!(right_half.max.x, frame.max.x);

        let eighth = bars('▁', 0, 0)[0];
        assert!(eighth.height() >= 1.0, "an eighth must still be visible");
        assert!(eighth.height() < lower_half.height());
    }

    /// The shades are the foreground seen through a screen: same rectangle, less opacity, so
    /// they tile instead of showing a dot pattern that lines up differently per cell.
    #[test]
    fn the_shades_fill_the_cell_at_a_fraction_of_the_foreground() {
        let mut previous = 0.0;
        for shade in ['░', '▒', '▓'] {
            let drawn = drawing(shade, cell(0, 0), 1.0).expect("a shade");
            assert_eq!(drawn.bars, vec![cell(0, 0)]);
            assert!(
                drawn.opacity > previous && drawn.opacity < 1.0,
                "{shade} must be darker than the last and lighter than solid"
            );
            previous = drawn.opacity;
        }
    }

    /// The quadrants have to divide the cell without overlapping or leaving a gap.
    #[test]
    fn the_quadrant_blocks_divide_the_cell_between_them() {
        let frame = cell(0, 0);
        let all: Vec<Rect> = ['▘', '▝', '▖', '▗']
            .iter()
            .map(|c| bars(*c, 0, 0)[0])
            .collect();
        let area: f32 = all.iter().map(|bar| bar.width() * bar.height()).sum();
        assert!(
            (area - frame.width() * frame.height()).abs() <= 1.0,
            "the four quadrants must add up to the cell: {area}"
        );
        assert_eq!(all[0].max, all[3].min, "the diagonal quadrants must touch");
    }

    /// A dashed line is drawn as marks with gaps, and still lands inside its own cell.
    #[test]
    fn a_dashed_line_is_marks_with_gaps_inside_one_cell() {
        let frame = cell(0, 0);
        let marks = bars('┄', 0, 0);
        assert!(marks.len() >= 2, "a triple dash needs more than one mark");
        for mark in &marks {
            assert!(mark.min.x >= frame.min.x && mark.max.x <= frame.max.x);
            assert!(mark.width() >= 1.0, "a mark has to be visible");
        }
        for pair in marks.windows(2) {
            assert!(
                pair[0].max.x < pair[1].min.x,
                "there must be a gap between marks: {pair:?}"
            );
        }
    }

    /// A rounded corner still has to reach the cell's edges, or a rounded frame comes apart
    /// at every corner.
    #[test]
    fn a_rounded_corner_reaches_the_edges_its_arms_come_from() {
        let frame = cell(0, 0);
        let arc = drawing('╭', frame, 1.0).expect("an arc");
        assert!(arc.bars.is_empty(), "an arc is a curve, not a bar");
        let curve = &arc.curves[0];
        assert!(arc.curve_width >= 1.0);
        let starts_at_bottom = curve
            .iter()
            .any(|point| (point.y - frame.max.y).abs() < f32::EPSILON);
        let ends_at_right = curve
            .iter()
            .any(|point| (point.x - frame.max.x).abs() < f32::EPSILON);
        assert!(starts_at_bottom && ends_at_right, "{curve:?}");
        // It curves away from the corner it turns around: nothing touches the top-left.
        assert!(curve
            .iter()
            .all(|point| point.x > frame.min.x && point.y > frame.min.y));
        // And its ends sit on the same centres a straight arm would use, so it meets them.
        let vertical = bars('│', 1, 0)[0];
        let horizontal = bars('─', 0, 1)[0];
        let tail = curve[0];
        let head = curve[curve.len() - 1];
        assert!(
            (tail.x - vertical.center().x).abs() <= 0.5,
            "the arc's tail {tail:?} misses the vertical at {}",
            vertical.center().x
        );
        assert!(
            (head.y - horizontal.center().y).abs() <= 0.5,
            "the arc's head {head:?} misses the horizontal at {}",
            horizontal.center().y
        );
    }

    /// The diagonals are the one case where a straight bar cannot say it.
    #[test]
    fn the_diagonals_run_corner_to_corner() {
        let frame = cell(0, 0);
        let cross = drawing('╳', frame, 1.0).expect("a diagonal cross");
        assert_eq!(cross.curves.len(), 2);
        for curve in &cross.curves {
            assert_eq!(curve.len(), 2);
            assert!(curve.iter().all(|point| frame.contains(*point)));
        }
    }

    /// The same character at a different scale still lands on whole pixels, or a retina
    /// display gets the softness a 1x display was just spared.
    #[test]
    fn bars_land_on_whole_physical_pixels_at_any_scale() {
        for scale in [1.0_f32, 1.25, 1.5, 2.0, 3.0] {
            let cell = Rect::from_min_max(
                Pos2::new(10.5 / scale, 3.5 / scale),
                Pos2::new(10.5 / scale + 7.9, 3.5 / scale + 15.1),
            );
            for c in ['─', '│', '┼', '█', '▄', '┬'] {
                for bar in drawing(c, cell, scale).expect("a drawn character").bars {
                    for edge in [bar.min.x, bar.max.x, bar.min.y, bar.max.y] {
                        let pixels = edge * scale;
                        assert!(
                            (pixels - pixels.round()).abs() < 1e-3,
                            "{c} at {scale}x has an edge at {pixels} pixels"
                        );
                    }
                }
            }
        }
    }

    /// A cell too small to hold a stroke must still draw one rather than nothing: a pane
    /// mid-resize is briefly a few pixels wide.
    #[test]
    fn a_degenerate_cell_still_draws_something_visible() {
        let tiny = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
        for c in ['─', '│', '█', '┼'] {
            let drawn = drawing(c, tiny, 1.0).expect("a drawn character");
            assert!(
                drawn
                    .bars
                    .iter()
                    .all(|bar| bar.width() > 0.0 && bar.height() > 0.0),
                "{c} produced an empty bar in a one-pixel cell"
            );
        }
    }
}
