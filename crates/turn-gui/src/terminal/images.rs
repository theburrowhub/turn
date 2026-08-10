//! Drawing the pictures a pane's cells refer to.
//!
//! The daemon decodes an image and anchors it to cells; what arrives here is a grid whose
//! cells carry markers and a small table saying which payload each marker's slot refers to
//! ([`turn_proto::images`]). Two things are left, and both belong on this side of the
//! socket because both need something only the window knows.
//!
//! ## Fitting, because only the window knows how big a cell is
//!
//! The daemon chose a **box** in cells. It could not choose more than that: the pixel size
//! of a cell is measured from the font, and two windows attached to the same pane at
//! different font sizes measure differently. So the window fits the picture inside the box
//! it was given, preserving the aspect ratio, and centres it — which is what makes a
//! photograph come out the right shape however wrong the daemon's assumption about cell
//! size was.
//!
//! ## Tiles, because the lattice is not negotiable
//!
//! A picture is drawn **one cell at a time**, or rather one run of cells at a time, and
//! every run is clipped to the pixel-snapped rectangle that
//! [`super::geometry::CellGrid`] gives for exactly those columns. Nothing is ever drawn
//! outside a cell's own rectangle. That is what keeps a picture from disturbing the grid
//! the box-drawing work depends on, and it is also what makes a picture scrolled half off
//! the top of a pane come out right with no special case: the rows that are still there
//! carry the tiles they always did, and each one knows which part of the image it is.
//!
//! ## The cache, and what happens before it fills
//!
//! Pixels are fetched by id, once, and held here. A cell whose picture has not arrived yet
//! — or whose picture the pane no longer holds — is drawn as a framed placeholder, and its
//! id is recorded in [`ImageCache::wanted`] for the window to ask for. A placeholder is
//! deliberately visible: a picture that silently did not appear is indistinguishable from a
//! bug.

use std::collections::{BTreeMap, BTreeSet};

use egui::{Color32, Rect, Stroke, TextureHandle, TextureOptions, Vec2};
use turn_proto::cells::Grid;
use turn_proto::images::{GridImage, ImageCell, ImageId, ImagePayload};

use super::geometry::CellGrid;
use crate::theme::Theme;

/// How many pictures one pane keeps uploaded.
///
/// Matches the number a screen can place, plus room for the ones just above it in the
/// window's own scrollback — so scrolling back a screen to look at a plot shows the plot
/// rather than a placeholder.
pub const MAX_CACHED_IMAGES: usize = 12;

/// How many bytes of pixels one pane keeps, 12 MiB.
///
/// A bound on the window's side as well as the daemon's, because the two caches are filled
/// by different things: the daemon's by what a process sent, this one by what the user has
/// scrolled past.
pub const MAX_CACHE_BYTES: usize = 12 * 1024 * 1024;

/// How many ids a pane will ask for at once.
///
/// A pane that scrolled through a hundred pictures must not put a hundred requests on the
/// socket in one frame.
pub const MAX_WANTED: usize = 8;

/// One uploaded picture.
///
/// `Clone` because [`super::PaneInteraction`] is, and cloning a [`TextureHandle`] shares the
/// texture rather than uploading it again — so a clone of a pane's state costs a refcount
/// bump per picture and not a megabyte per picture.
#[derive(Clone)]
struct Cached {
    texture: TextureHandle,
    bytes: usize,
    /// Bumped whenever the picture is drawn, so the least recently used one is the one
    /// dropped when the cache is full.
    used: u64,
}

/// A pane's uploaded pictures, and the ids it still needs.
#[derive(Clone, Default)]
pub struct ImageCache {
    images: BTreeMap<ImageId, Cached>,
    wanted: BTreeSet<ImageId>,
    bytes: usize,
    clock: u64,
    /// Ids already asked for, so a picture the daemon no longer holds is not requested once
    /// per frame for the rest of the session.
    asked: BTreeSet<ImageId>,
}

impl std::fmt::Debug for ImageCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "ImageCache({} images, {} bytes, {} wanted)",
            self.images.len(),
            self.bytes,
            self.wanted.len()
        )
    }
}

impl ImageCache {
    /// Uploads a payload and remembers it by id.
    ///
    /// Ids are content-derived, so re-inserting the same picture is free and cannot replace
    /// one picture's pixels with another's.
    pub fn insert(&mut self, ctx: &egui::Context, payload: &ImagePayload) {
        self.wanted.remove(&payload.id);
        if self.images.contains_key(&payload.id) {
            return;
        }
        let size = [payload.width as usize, payload.height as usize];
        let image = egui::ColorImage::from_rgba_unmultiplied(size, payload.pixels());
        // Linear filtering because a picture is nearly always being *reduced* into its cell
        // box, and nearest-neighbour reduction turns a plot's gridlines into aliasing.
        let texture = ctx.load_texture(
            format!("turn-pane-image-{}", payload.id),
            image,
            TextureOptions::LINEAR,
        );
        self.clock += 1;
        self.bytes += payload.byte_len();
        self.images.insert(
            payload.id,
            Cached {
                texture,
                bytes: payload.byte_len(),
                used: self.clock,
            },
        );
        self.evict();
    }

    /// Drops the least recently drawn pictures until both bounds hold.
    fn evict(&mut self) {
        while self.images.len() > MAX_CACHED_IMAGES || self.bytes > MAX_CACHE_BYTES {
            let Some(victim) = self
                .images
                .iter()
                .min_by_key(|(_, cached)| cached.used)
                .map(|(id, _)| *id)
            else {
                break;
            };
            if let Some(dropped) = self.images.remove(&victim) {
                self.bytes -= dropped.bytes;
                // Forgotten as asked-for too: if it comes back on screen it is worth
                // fetching again.
                self.asked.remove(&victim);
            }
        }
    }

    /// The ids this pane has cells for and no pixels, bounded.
    ///
    /// Each id is offered once. A picture the daemon can no longer supply would otherwise be
    /// asked for on every frame for the rest of the session.
    pub fn wanted(&self) -> Vec<ImageId> {
        self.wanted.iter().copied().take(MAX_WANTED).collect()
    }

    /// The ids to ask for, marking them asked so they are not offered again.
    pub fn take_wanted(&mut self) -> Vec<ImageId> {
        let ids = self.wanted();
        for id in &ids {
            self.wanted.remove(id);
            self.asked.insert(*id);
        }
        ids
    }

    /// Records that a picture is on screen and its pixels are not here.
    fn note_missing(&mut self, id: ImageId) {
        if !self.asked.contains(&id) {
            self.wanted.insert(id);
        }
    }

    pub fn len(&self) -> usize {
        self.images.len()
    }

    pub fn is_empty(&self) -> bool {
        self.images.is_empty()
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Whether a picture's pixels are here.
    pub fn holds(&self, id: ImageId) -> bool {
        self.images.contains_key(&id)
    }

    /// The texture for a picture, marking it as used.
    fn use_texture(&mut self, id: ImageId) -> Option<egui::TextureId> {
        self.clock += 1;
        let clock = self.clock;
        let cached = self.images.get_mut(&id)?;
        cached.used = clock;
        Some(cached.texture.id())
    }
}

/// The rectangle a picture actually occupies inside the cell box it was given.
///
/// Centred, and never larger than the box. With `preserve_aspect` clear the picture fills
/// the box, which is what a program that asked for both dimensions and turned aspect
/// preservation off is asking for.
///
/// A pure function of two rectangles and a flag, so the arithmetic is tested rather than
/// eyeballed — and it is the only place the picture's own pixel dimensions matter, which is
/// why the daemon can be wrong about cell size without a picture coming out stretched.
pub fn fit(box_rect: Rect, image: &GridImage) -> Rect {
    if !image.preserve_aspect || image.width == 0 || image.height == 0 {
        return box_rect;
    }
    let (bw, bh) = (box_rect.width(), box_rect.height());
    if bw <= 0.0 || bh <= 0.0 {
        return box_rect;
    }
    let scale = (bw / image.width as f32).min(bh / image.height as f32);
    let size = Vec2::new(image.width as f32 * scale, image.height as f32 * scale);
    Rect::from_center_size(box_rect.center(), size)
}

/// Where the whole cell box of a picture sits, worked out from one of its tiles.
///
/// The box's own top-left cell may be off screen — a picture scrolled halfway off the top
/// has no row zero to measure from — so it is derived from a tile that *is* on screen and
/// the tile's own coordinates inside the picture. That is the whole reason a marker carries
/// `dy` and `dx` rather than only a slot.
pub fn box_rect(
    lattice: &CellGrid,
    row: u16,
    col: u16,
    tile: ImageCell,
    image: &GridImage,
) -> Rect {
    let cell = lattice.cell();
    let anchor = lattice.cell_rect(row, col).min
        - Vec2::new(tile.dx as f32 * cell.x, tile.dy as f32 * cell.y);
    Rect::from_min_size(
        anchor,
        Vec2::new(image.cols as f32 * cell.x, image.rows as f32 * cell.y),
    )
}

/// The part of a texture that shows through a rectangle of the fitted picture.
///
/// Normalised to the fitted rectangle rather than to the cell box, so the letterboxing a
/// non-matching aspect ratio produces falls outside every tile rather than being sampled as
/// stretched pixels.
pub fn uv_for(fitted: Rect, part: Rect) -> Rect {
    let (w, h) = (fitted.width(), fitted.height());
    if w <= 0.0 || h <= 0.0 {
        return Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
    }
    Rect::from_min_max(
        egui::pos2(
            (part.min.x - fitted.min.x) / w,
            (part.min.y - fitted.min.y) / h,
        ),
        egui::pos2(
            (part.max.x - fitted.min.x) / w,
            (part.max.y - fitted.min.y) / h,
        ),
    )
}

/// One horizontal run of tiles of the same picture, on one row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TileRun {
    pub slot: u8,
    /// Tile row inside the picture, shared by every cell of the run.
    pub dy: u16,
    /// First column on screen.
    pub from: u16,
    /// One past the last column on screen.
    pub to: u16,
    /// Tile column of the cell at `from`.
    pub dx: u16,
}

impl TileRun {
    pub fn cols(&self) -> u16 {
        self.to.saturating_sub(self.from)
    }
}

/// The runs of image tiles on one row.
///
/// Runs rather than cells so a full-width picture is a handful of draw calls, and a run
/// breaks wherever the picture, its tile row, or the sequence of tile columns does — which
/// is what makes two pictures side by side, and a picture with a hole punched in it, come
/// out right.
pub fn tile_runs(grid: &Grid, row: u16) -> Vec<TileRun> {
    let mut runs: Vec<TileRun> = Vec::new();
    for col in 0..grid.cols {
        let Some(tile) = grid
            .cell(row, col)
            .and_then(turn_proto::cells::Cell::image_tile)
        else {
            continue;
        };
        let extends = runs.last().is_some_and(|last| {
            last.to == col
                && last.slot == tile.slot
                && last.dy == tile.dy
                && last.dx + last.cols() == tile.dx
        });
        if extends {
            if let Some(last) = runs.last_mut() {
                last.to = col + 1;
            }
            continue;
        }
        runs.push(TileRun {
            slot: tile.slot,
            dy: tile.dy,
            from: col,
            to: col + 1,
            dx: tile.dx,
        });
    }
    runs
}

/// Paints one row's pictures.
///
/// `selected` answers whether a column is inside the selection, so a picture under a
/// selection is tinted the same way text is highlighted rather than being the one thing on
/// screen a selection does not touch.
pub fn paint_row(
    painter: &egui::Painter,
    theme: &Theme,
    lattice: &CellGrid,
    grid: &Grid,
    row: u16,
    cache: &mut ImageCache,
    selected: &dyn Fn(u16, u16) -> bool,
) {
    for run in tile_runs(grid, row) {
        let Some(image) = grid.image_in_slot(run.slot) else {
            // A marker whose slot the screen's table does not fill: the picture has been
            // forgotten. Drawn as a placeholder rather than as nothing, because a hole in
            // the output with no explanation is worse than a frame.
            paint_placeholder(painter, theme, lattice, row, &run, None);
            continue;
        };
        let run_rect = lattice.span(row, run.from, run.cols());
        let fitted = fit(
            box_rect(lattice, row, run.from, tile_at(&run), image),
            image,
        );
        let visible = fitted.intersect(run_rect);

        match cache.use_texture(image.id) {
            Some(texture) if visible.is_positive() => {
                painter.image(texture, visible, uv_for(fitted, visible), Color32::WHITE);
            }
            Some(_) => {}
            None => {
                cache.note_missing(image.id);
                paint_placeholder(painter, theme, lattice, row, &run, Some(image));
                continue;
            }
        }

        // The selection highlight goes over the picture, per column, because a selection may
        // cover part of a run.
        for col in run.from..run.to {
            if selected(row, col) {
                painter.rect_filled(
                    lattice.cell_rect(row, col),
                    0.0,
                    theme.selection.gamma_multiply(0.5),
                );
            }
        }
    }
}

/// The tile coordinates of a run's first cell.
fn tile_at(run: &TileRun) -> ImageCell {
    ImageCell::new(run.slot, run.dy, run.dx)
}

/// A frame where a picture will be, or would have been.
///
/// Only the edges of the *picture* are stroked, so a picture spread over several rows shows
/// as one rectangle rather than as a grid of boxes — and a picture scrolled half off the top
/// shows as a rectangle with no top edge, which is exactly what it is.
fn paint_placeholder(
    painter: &egui::Painter,
    theme: &Theme,
    lattice: &CellGrid,
    row: u16,
    run: &TileRun,
    image: Option<&GridImage>,
) {
    let rect = lattice.span(row, run.from, run.cols());
    painter.rect_filled(rect, 0.0, theme.raised);
    let stroke = Stroke::new(1.0, theme.border);
    if run.dy == 0 {
        painter.hline(rect.x_range(), rect.min.y + 0.5, stroke);
    }
    if let Some(image) = image {
        if run.dy + 1 == image.rows {
            painter.hline(rect.x_range(), rect.max.y - 0.5, stroke);
        }
        if run.dx == 0 {
            painter.vline(rect.min.x + 0.5, rect.y_range(), stroke);
        }
        if run.dx + run.cols() == image.cols {
            painter.vline(rect.max.x - 0.5, rect.y_range(), stroke);
        }
    } else {
        // No placement at all: frame the run itself, since nothing says how big the picture
        // was meant to be.
        painter.rect_stroke(rect, 0.0, stroke, egui::StrokeKind::Inside);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use turn_proto::cells::Cell;
    use turn_proto::images::{GridImage, ImageId};

    fn lattice() -> CellGrid {
        CellGrid::new(egui::pos2(10.3, 7.6), Vec2::new(8.0, 16.0), 2.0)
    }

    /// A grid with one picture of `rows` by `cols` cells anchored at `(top, left)`.
    fn with_image(
        grid: &mut Grid,
        slot: u8,
        top: u16,
        left: u16,
        rows: u16,
        cols: u16,
        pixels: (u32, u32),
    ) -> GridImage {
        for dy in 0..rows {
            for dx in 0..cols {
                if let Some(cell) = grid.cell_mut(top + dy, left + dx) {
                    *cell = Cell::image(ImageCell::new(slot, dy, dx)).expect("an addressable tile");
                }
            }
        }
        let placed = GridImage::new(slot, ImageId(0x77), rows, cols, pixels.0, pixels.1);
        grid.images.push(placed);
        placed
    }

    /// The property that makes the daemon's guess about cell size harmless: a picture keeps
    /// its shape whatever box it is given.
    #[test]
    fn a_picture_keeps_its_aspect_ratio_inside_a_box_of_the_wrong_shape() {
        let box_rect = Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(400.0, 100.0));
        // A square picture in a wide box: as tall as the box, centred horizontally.
        let square = GridImage::new(0, ImageId(1), 1, 1, 500, 500);
        let fitted = fit(box_rect, &square);
        assert!((fitted.width() - 100.0).abs() < 0.01, "{fitted:?}");
        assert!((fitted.height() - 100.0).abs() < 0.01);
        assert_eq!(fitted.center(), box_rect.center());
        assert!(box_rect.contains_rect(fitted));

        // A wide picture in a square box: as wide as the box, centred vertically.
        let square_box = Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(200.0, 200.0));
        let wide = GridImage::new(0, ImageId(1), 1, 1, 400, 100);
        let fitted = fit(square_box, &wide);
        assert!((fitted.width() - 200.0).abs() < 0.01, "{fitted:?}");
        assert!((fitted.height() - 50.0).abs() < 0.01);
        assert_eq!(fitted.center(), square_box.center());
    }

    #[test]
    fn a_picture_told_not_to_keep_its_shape_fills_the_box_it_was_given() {
        let box_rect = Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(400.0, 100.0));
        let stretched = GridImage {
            preserve_aspect: false,
            ..GridImage::new(0, ImageId(1), 1, 1, 500, 500)
        };
        assert_eq!(fit(box_rect, &stretched), box_rect);
    }

    #[test]
    fn a_degenerate_box_or_picture_does_not_divide_by_zero() {
        let empty = Rect::from_min_size(egui::Pos2::ZERO, Vec2::ZERO);
        let image = GridImage::new(0, ImageId(1), 1, 1, 10, 10);
        assert_eq!(fit(empty, &image), empty);

        let no_pixels = GridImage::new(0, ImageId(1), 1, 1, 0, 0);
        let box_rect = Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(10.0, 10.0));
        assert_eq!(fit(box_rect, &no_pixels), box_rect);

        assert_eq!(
            uv_for(empty, empty),
            Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0))
        );
    }

    /// The box has to be derivable from any tile, because the picture's own first row may
    /// have scrolled off the top of the pane.
    #[test]
    fn the_box_of_a_picture_is_found_from_any_of_its_tiles_including_a_scrolled_one() {
        let lattice = lattice();
        let image = GridImage::new(0, ImageId(1), 4, 6, 100, 100);

        // From its own corner.
        let from_corner = box_rect(&lattice, 3, 5, ImageCell::new(0, 0, 0), &image);
        assert_eq!(from_corner.min, lattice.cell_rect(3, 5).min);

        // From a tile three rows into the picture, which puts the box above this row.
        let from_middle = box_rect(&lattice, 3, 5, ImageCell::new(0, 3, 0), &image);
        assert!(
            (from_middle.min.y - (from_corner.min.y - 3.0 * 16.0)).abs() < 0.6,
            "{from_middle:?} against {from_corner:?}"
        );
        assert_eq!(from_middle.min.x, from_corner.min.x);

        // A picture whose top row is off the pane entirely: the box begins above zero, which
        // is what makes the visible rows show their own part of it.
        let scrolled = box_rect(&lattice, 0, 0, ImageCell::new(0, 3, 0), &image);
        assert!(scrolled.min.y < 0.0, "{scrolled:?}");
        assert!((scrolled.height() - 4.0 * 16.0).abs() < 0.01);
        assert!((scrolled.width() - 6.0 * 8.0).abs() < 0.01);
    }

    /// The tiles of a row are drawn as runs, and a run stops wherever the picture does.
    #[test]
    fn a_row_of_tiles_is_one_run_and_two_pictures_side_by_side_are_two() {
        let mut grid = Grid::blank(4, 20);
        with_image(&mut grid, 0, 0, 0, 1, 4, (40, 20));
        with_image(&mut grid, 1, 0, 4, 1, 3, (30, 20));

        let runs = tile_runs(&grid, 0);
        assert_eq!(runs.len(), 2, "{runs:?}");
        assert_eq!(
            runs[0],
            TileRun {
                slot: 0,
                dy: 0,
                from: 0,
                to: 4,
                dx: 0
            }
        );
        assert_eq!(
            runs[1],
            TileRun {
                slot: 1,
                dy: 0,
                from: 4,
                to: 7,
                dx: 0
            }
        );
        assert!(
            tile_runs(&grid, 1).is_empty(),
            "a row with no tiles has no runs"
        );
    }

    /// A program printing over the middle of a picture punches a hole in it, and the two
    /// halves have to be drawn as two runs with their own tile columns — or the right-hand
    /// half would be drawn with the left-hand half's pixels.
    #[test]
    fn a_picture_with_a_hole_punched_in_it_is_drawn_as_the_pieces_that_survive() {
        let mut grid = Grid::blank(2, 12);
        with_image(&mut grid, 0, 0, 0, 1, 8, (80, 20));
        // Two characters printed over columns three and four.
        for col in 3..5u16 {
            if let Some(cell) = grid.cell_mut(0, col) {
                *cell = Cell::plain("x");
            }
        }

        let runs = tile_runs(&grid, 0);
        assert_eq!(runs.len(), 2, "{runs:?}");
        assert_eq!((runs[0].from, runs[0].to, runs[0].dx), (0, 3, 0));
        assert_eq!(
            (runs[1].from, runs[1].to, runs[1].dx),
            (5, 8, 5),
            "the surviving right-hand piece must still know it is columns five to eight"
        );
    }

    /// The same picture drawn in two places is two runs of the same slot, and each one shows
    /// its own tiles rather than being merged into a run that spans the gap.
    #[test]
    fn two_runs_of_the_same_picture_on_one_row_are_not_merged_across_the_gap() {
        let mut grid = Grid::blank(2, 12);
        with_image(&mut grid, 0, 0, 0, 1, 3, (30, 20));
        // The same slot again further along, starting from tile zero.
        for (offset, dx) in (0..3u16).enumerate() {
            if let Some(cell) = grid.cell_mut(0, 6 + offset as u16) {
                *cell = Cell::image(ImageCell::new(0, 0, dx)).expect("a tile");
            }
        }
        let runs = tile_runs(&grid, 0);
        assert_eq!(runs.len(), 2, "{runs:?}");
        assert_eq!(runs[1].from, 6);
        assert_eq!(runs[1].dx, 0);
    }

    /// The rule the lattice depends on: nothing is drawn outside the cells the run covers.
    #[test]
    fn a_run_is_never_drawn_outside_the_cells_it_covers() {
        let lattice = lattice();
        let image = GridImage::new(0, ImageId(1), 2, 6, 600, 100);
        // A wide picture in a box of a different shape: the fit is letterboxed vertically.
        let run = TileRun {
            slot: 0,
            dy: 0,
            from: 2,
            to: 8,
            dx: 0,
        };
        let run_rect = lattice.span(0, run.from, run.cols());
        let fitted = fit(
            box_rect(&lattice, 0, run.from, tile_at(&run), &image),
            &image,
        );
        let drawn = fitted.intersect(run_rect);
        assert!(
            run_rect.contains_rect(drawn),
            "{drawn:?} escaped the cells {run_rect:?}"
        );

        // And the UVs of that part are inside the texture.
        let uv = uv_for(fitted, drawn);
        assert!(uv.min.x >= -0.001 && uv.max.x <= 1.001, "{uv:?}");
        assert!(uv.min.y >= -0.001 && uv.max.y <= 1.001, "{uv:?}");
    }

    /// Each tile shows its own part of the picture, in order, with no overlap and no gap.
    #[test]
    fn consecutive_tiles_show_consecutive_parts_of_the_picture() {
        let lattice = lattice();
        // A picture whose shape matches its box exactly, so there is no letterboxing to
        // complicate the arithmetic.
        let image = GridImage::new(0, ImageId(1), 1, 4, 32, 16);
        let fitted = fit(
            box_rect(&lattice, 0, 0, ImageCell::new(0, 0, 0), &image),
            &image,
        );
        let mut previous: Option<Rect> = None;
        for dx in 0..4u16 {
            let cell = lattice.cell_rect(0, dx);
            let uv = uv_for(fitted, cell.intersect(fitted));
            if let Some(previous) = previous {
                assert!(
                    (uv.min.x - previous.max.x).abs() < 0.01,
                    "tile {dx} starts at {} where the last ended at {}",
                    uv.min.x,
                    previous.max.x
                );
            }
            assert!(uv.min.x < uv.max.x, "tile {dx} samples nothing: {uv:?}");
            previous = Some(uv);
        }
        let last = previous.expect("four tiles");
        assert!(
            (last.max.x - 1.0).abs() < 0.02,
            "the last tile must reach the right edge: {last:?}"
        );
    }

    #[test]
    fn a_cache_holds_a_picture_by_its_id_and_says_when_it_does_not() {
        let ctx = egui::Context::default();
        let mut cache = ImageCache::default();
        let payload = ImagePayload::new(2, 2, vec![9; 16]).expect("a 2x2 picture");
        assert!(!cache.holds(payload.id));

        cache.insert(&ctx, &payload);
        assert!(cache.holds(payload.id));
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.bytes(), 16);
        assert!(!cache.is_empty());

        // Inserting the same picture again is free and does not replace it.
        cache.insert(&ctx, &payload);
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.bytes(), 16);
    }

    #[test]
    fn a_picture_on_screen_with_no_pixels_is_asked_for_once_and_not_every_frame() {
        let mut cache = ImageCache::default();
        cache.note_missing(ImageId(1));
        cache.note_missing(ImageId(1));
        assert_eq!(cache.wanted(), vec![ImageId(1)]);

        assert_eq!(cache.take_wanted(), vec![ImageId(1)]);
        assert!(cache.wanted().is_empty(), "and it is not offered twice");
        cache.note_missing(ImageId(1));
        assert!(
            cache.wanted().is_empty(),
            "a picture the daemon could not supply must not be asked for for ever"
        );
    }

    #[test]
    fn a_pane_never_asks_for_more_pictures_at_once_than_it_will_draw() {
        let mut cache = ImageCache::default();
        for id in 0..100u64 {
            cache.note_missing(ImageId(id));
        }
        assert_eq!(cache.wanted().len(), MAX_WANTED);
        assert_eq!(cache.take_wanted().len(), MAX_WANTED);
        assert_eq!(
            cache.wanted().len(),
            MAX_WANTED,
            "the rest are still pending"
        );
    }

    /// The window's own bound. A pane that scrolls past a hundred pictures must not keep a
    /// hundred textures uploaded.
    #[test]
    fn the_cache_stays_inside_both_of_its_bounds() {
        let ctx = egui::Context::default();
        let mut cache = ImageCache::default();
        for index in 0..40u8 {
            // A different picture each time, so nothing deduplicates.
            let payload =
                ImagePayload::new(256, 256, vec![index; 256 * 256 * 4]).expect("a 256x256 picture");
            cache.insert(&ctx, &payload);
        }
        assert!(cache.len() <= MAX_CACHED_IMAGES, "{} kept", cache.len());
        assert!(
            cache.bytes() <= MAX_CACHE_BYTES,
            "{} bytes kept",
            cache.bytes()
        );
    }

    #[test]
    fn the_least_recently_drawn_picture_is_the_one_dropped() {
        let ctx = egui::Context::default();
        let mut cache = ImageCache::default();
        let mut ids = Vec::new();
        for index in 0..MAX_CACHED_IMAGES as u8 {
            let payload = ImagePayload::new(2, 2, vec![index; 16]).expect("a small picture");
            ids.push(payload.id);
            cache.insert(&ctx, &payload);
        }
        assert_eq!(cache.len(), MAX_CACHED_IMAGES);

        // Draw the first one, which makes the second the least recently used.
        assert!(cache.use_texture(ids[0]).is_some());
        let extra = ImagePayload::new(2, 2, vec![250; 16]).expect("one more");
        cache.insert(&ctx, &extra);

        assert!(cache.holds(ids[0]), "the one just drawn must survive");
        assert!(cache.holds(extra.id));
        assert!(!cache.holds(ids[1]), "the least recently drawn one goes");
    }

    #[test]
    fn the_debug_form_of_a_cache_names_its_size_rather_than_its_pixels() {
        let ctx = egui::Context::default();
        let mut cache = ImageCache::default();
        cache.insert(
            &ctx,
            &ImagePayload::new(2, 2, vec![0xAB; 16]).expect("a picture"),
        );
        let debugged = format!("{cache:?}");
        assert!(debugged.contains("1 images"), "got {debugged}");
        assert!(debugged.contains("16 bytes"), "got {debugged}");
        assert!(!debugged.contains("171"), "the pixels leaked: {debugged}");
    }
}
