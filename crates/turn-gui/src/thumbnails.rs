//! Small pictures of what every session is doing, for the overview.
//!
//! The overview shows thirty panes at once. Painting each one cell by cell would be
//! thirty full grids a frame, which is the one thing the window must not do — so a
//! thumbnail is a **downsample**, taken on a slow cadence and reused between takes.
//!
//! Two decisions carry the module:
//!
//! * **Never per frame.** A thumbnail is rebuilt at most once every
//!   [`REFRESH_INTERVAL_MS`], and only for panes the overview is actually showing.
//!   The cadence is checked by [`Thumbnails::due`], which takes `now_ms`, so the rule
//!   is a tested property rather than a hope about how often the caller calls.
//! * **A thumbnail is blocks, not text.** At overview size a glyph is two pixels
//!   across and unreadable, so what is drawn is the *shape* of the output: which cells
//!   have ink, and what colour. That is what makes a session recognisable at a glance —
//!   a test log, a diff, a full-screen tool all look different — without pretending to
//!   be legible.

use std::collections::HashMap;

use turn_core::ids::SessionId;
use turn_proto::cells::Grid;

/// How often a thumbnail may be rebuilt.
///
/// Two seconds is fast enough that the overview does not feel stale while a build
/// scrolls, and slow enough that thirty of them cost nothing. A session that changed
/// in between is still shown — one refresh behind — which is the right trade for a
/// picture the size of a postage stamp.
pub const REFRESH_INTERVAL_MS: i64 = 2_000;

/// The size of a thumbnail, in blocks.
///
/// Chosen to be roughly the aspect of a terminal so a full-screen tool's layout is
/// recognisable, and small enough that one is 400 rectangles rather than 4,800.
pub const THUMBNAIL_COLS: usize = 24;
pub const THUMBNAIL_ROWS: usize = 12;

/// One block of a thumbnail: how much ink is in the cells it covers, and their colour.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Block {
    /// How full the block is, `0.0` to `1.0`. Drawn as opacity, so a dense log looks
    /// dense and a prompt looks like one line.
    pub ink: f32,
    /// The most saturated foreground colour in the block, if any of its cells had one.
    /// Carried so a red failure is visible in the overview without reading it.
    pub colour: Option<turn_proto::cells::Rgb>,
}

/// A downsampled screen.
#[derive(Debug, Clone, PartialEq)]
pub struct Thumbnail {
    pub blocks: Vec<Block>,
    pub cols: usize,
    pub rows: usize,
    /// When this was taken, so the cadence can be enforced and a stale one labelled.
    pub taken_ms: i64,
    /// True when the pane was showing a full-screen application, which the overview
    /// marks: a TUI's thumbnail looks like a solid rectangle and would otherwise be
    /// indistinguishable from a wall of output.
    pub alternate_screen: bool,
}

impl Thumbnail {
    pub fn block(&self, row: usize, col: usize) -> Option<&Block> {
        if row >= self.rows || col >= self.cols {
            return None;
        }
        self.blocks.get(row * self.cols + col)
    }

    /// Whether anything at all is in this thumbnail, so an empty pane can be drawn as
    /// empty rather than as a grid of invisible blocks.
    pub fn is_blank(&self) -> bool {
        self.blocks.iter().all(|block| block.ink <= 0.0)
    }
}

/// Downsamples a grid.
///
/// Every block averages the ink of the cells that fall in it, so a line of text
/// produces a faint band rather than either disappearing or filling the block. A cell
/// with a background counts as ink, because a highlighted line is exactly the thing the
/// overview should show.
pub fn downsample(grid: &Grid, taken_ms: i64) -> Thumbnail {
    let cols = THUMBNAIL_COLS.min(grid.cols as usize).max(1);
    let rows = THUMBNAIL_ROWS.min(grid.rows as usize).max(1);
    let mut blocks = Vec::with_capacity(rows * cols);

    for block_row in 0..rows {
        // Integer spans, so every cell belongs to exactly one block and none is counted
        // twice — which would make a thumbnail brighter than the screen it came from.
        let row_start = block_row * grid.rows as usize / rows;
        let row_end = ((block_row + 1) * grid.rows as usize / rows).max(row_start + 1);
        for block_col in 0..cols {
            let col_start = block_col * grid.cols as usize / cols;
            let col_end = ((block_col + 1) * grid.cols as usize / cols).max(col_start + 1);

            let mut inked = 0usize;
            let mut total = 0usize;
            let mut colour: Option<turn_proto::cells::Rgb> = None;
            let mut best_saturation = 0u16;

            for row in row_start..row_end {
                for col in col_start..col_end {
                    total += 1;
                    let Some(cell) = grid.cell(row as u16, col as u16) else {
                        continue;
                    };
                    if cell.is_blank() {
                        continue;
                    }
                    inked += 1;
                    if let Some(rgb) = cell.fg {
                        let saturation = saturation(rgb);
                        if saturation > best_saturation {
                            best_saturation = saturation;
                            colour = Some(rgb);
                        }
                    }
                }
            }
            blocks.push(Block {
                ink: if total == 0 {
                    0.0
                } else {
                    inked as f32 / total as f32
                },
                colour,
            });
        }
    }

    Thumbnail {
        blocks,
        cols,
        rows,
        taken_ms,
        alternate_screen: grid.alternate_screen,
    }
}

/// How far a colour is from grey, so the block picks the one that carries meaning.
///
/// A line of white text and a line of red text are both ink; only one of them is the
/// user's signal, and the overview should show the red.
fn saturation(rgb: turn_proto::cells::Rgb) -> u16 {
    let max = rgb.0.max(rgb.1).max(rgb.2) as u16;
    let min = rgb.0.min(rgb.1).min(rgb.2) as u16;
    max - min
}

/// The thumbnails the overview is showing, and when each was taken.
#[derive(Debug, Default)]
pub struct Thumbnails {
    taken: HashMap<SessionId, Thumbnail>,
}

impl Thumbnails {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a session's thumbnail is due to be rebuilt.
    ///
    /// The cadence lives here rather than in the caller so that "never per frame" is a
    /// property of the type: a draw function calling this sixty times a second gets
    /// `false` fifty-nine times.
    pub fn due(&self, session: &SessionId, now_ms: i64) -> bool {
        match self.taken.get(session) {
            None => true,
            Some(thumbnail) => now_ms.saturating_sub(thumbnail.taken_ms) >= REFRESH_INTERVAL_MS,
        }
    }

    /// Takes a thumbnail if one is due, and reports whether it did.
    pub fn refresh(&mut self, session: &SessionId, grid: &Grid, now_ms: i64) -> bool {
        if !self.due(session, now_ms) {
            return false;
        }
        self.taken.insert(session.clone(), downsample(grid, now_ms));
        true
    }

    pub fn get(&self, session: &SessionId) -> Option<&Thumbnail> {
        self.taken.get(session)
    }

    /// When the earliest thumbnail on screen will be due, for the repaint plan.
    ///
    /// `None` when the overview is showing nothing, which is the usual case and costs
    /// no wake-up at all.
    pub fn next_due_ms(&self, showing: &[SessionId]) -> Option<i64> {
        showing
            .iter()
            .map(|session| match self.taken.get(session) {
                // Never taken: due immediately.
                None => 0,
                Some(thumbnail) => thumbnail.taken_ms + REFRESH_INTERVAL_MS,
            })
            .min()
    }

    /// Forgets a session's thumbnail, when it closes.
    pub fn forget(&mut self, session: &SessionId) {
        self.taken.remove(session);
    }

    pub fn len(&self) -> usize {
        self.taken.len()
    }

    pub fn is_empty(&self) -> bool {
        self.taken.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use turn_proto::cells::Rgb;

    const T0: i64 = 1_700_000_000_000;

    fn session(name: &str) -> SessionId {
        SessionId::from_stored(name)
    }

    fn grid_with_text(rows: u16, cols: u16, lines: &[&str]) -> Grid {
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

    /// The rule the module exists for.
    #[test]
    fn a_thumbnail_is_not_rebuilt_more_often_than_the_cadence_allows() {
        let mut thumbnails = Thumbnails::new();
        let id = session("sess_thumb0001");
        let grid = grid_with_text(40, 120, &["hello"]);

        assert!(thumbnails.refresh(&id, &grid, T0), "the first take happens");
        // Sixty frames a second for the next two seconds.
        for frame in 1..120 {
            let now = T0 + frame * 16;
            assert!(
                !thumbnails.refresh(&id, &grid, now),
                "a frame at +{}ms must not rebuild",
                frame * 16
            );
        }
        assert!(
            thumbnails.refresh(&id, &grid, T0 + REFRESH_INTERVAL_MS),
            "and it does refresh once the cadence has elapsed"
        );
    }

    #[test]
    fn a_session_that_has_never_been_seen_is_due_immediately() {
        let thumbnails = Thumbnails::new();
        assert!(thumbnails.due(&session("sess_new00000001"), T0));
        assert_eq!(
            thumbnails.next_due_ms(&[session("sess_new00000001")]),
            Some(0),
            "an unseen session must not wait for the cadence to show anything at all"
        );
    }

    #[test]
    fn an_overview_showing_nothing_costs_no_wake_up() {
        let thumbnails = Thumbnails::new();
        assert_eq!(thumbnails.next_due_ms(&[]), None);
    }

    #[test]
    fn the_next_wake_up_is_the_soonest_thumbnail_on_screen() {
        let mut thumbnails = Thumbnails::new();
        let early = session("sess_early000001");
        let late = session("sess_late0000001");
        let grid = grid_with_text(10, 20, &["x"]);
        thumbnails.refresh(&early, &grid, T0);
        thumbnails.refresh(&late, &grid, T0 + 500);

        assert_eq!(
            thumbnails.next_due_ms(&[early.clone(), late]),
            Some(T0 + REFRESH_INTERVAL_MS)
        );
        // A session the overview is not showing does not schedule anything.
        assert_eq!(
            thumbnails.next_due_ms(&[]),
            None,
            "closing the overview must stop the cadence entirely"
        );
    }

    /// The shape of the output is what makes a session recognisable, so a dense screen
    /// and a prompt must not downsample to the same picture.
    #[test]
    fn a_dense_screen_and_a_prompt_produce_different_pictures() {
        let mut dense_lines = Vec::new();
        for _ in 0..40 {
            dense_lines.push("x".repeat(120));
        }
        let dense_refs: Vec<&str> = dense_lines.iter().map(String::as_str).collect();
        let dense = downsample(&grid_with_text(40, 120, &dense_refs), T0);
        let sparse = downsample(&grid_with_text(40, 120, &["~/turn $ "]), T0);

        let dense_ink: f32 = dense.blocks.iter().map(|b| b.ink).sum();
        let sparse_ink: f32 = sparse.blocks.iter().map(|b| b.ink).sum();
        assert!(
            dense_ink > sparse_ink * 5.0,
            "a full screen must look fuller: {dense_ink} against {sparse_ink}"
        );
        assert!(!dense.is_blank());
        assert!(!sparse.is_blank());
    }

    #[test]
    fn an_empty_pane_is_blank_rather_than_a_grid_of_invisible_blocks() {
        let thumbnail = downsample(&Grid::blank(40, 120), T0);
        assert!(thumbnail.is_blank());
        assert_eq!(thumbnail.rows, THUMBNAIL_ROWS);
        assert_eq!(thumbnail.cols, THUMBNAIL_COLS);
        assert_eq!(thumbnail.blocks.len(), THUMBNAIL_ROWS * THUMBNAIL_COLS);
    }

    /// A block averages its cells rather than taking the first: a line of text in a
    /// block of four rows should be a quarter full, not full or empty.
    #[test]
    fn a_block_averages_the_cells_it_covers() {
        // 12 rows into 12 blocks is one row each; 24 rows is two.
        let one_line_of_two = downsample(&grid_with_text(24, 24, &["x".repeat(24).as_str()]), T0);
        let top_left = one_line_of_two
            .block(0, 0)
            .expect("the top left block exists");
        assert!(
            (top_left.ink - 0.5).abs() < 0.01,
            "one inked row of two is half: got {}",
            top_left.ink
        );
    }

    /// A red failure has to be visible in the overview without reading it, so the block
    /// keeps the most saturated colour rather than the first or an average.
    #[test]
    fn a_block_keeps_the_colour_that_carries_meaning() {
        let mut grid = Grid::blank(12, 24);
        for col in 0..20u16 {
            if let Some(cell) = grid.cell_mut(0, col) {
                cell.text = "x".into();
                cell.fg = Some(Rgb::new(0xd6, 0xd6, 0xd6));
            }
        }
        if let Some(cell) = grid.cell_mut(0, 3) {
            cell.text = "E".into();
            cell.fg = Some(Rgb::new(0xe0, 0x5a, 0x5a));
        }
        let thumbnail = downsample(&grid, T0);
        let block = thumbnail
            .block(0, 3)
            .expect("the block with the error in it");
        assert_eq!(
            block.colour,
            Some(Rgb::new(0xe0, 0x5a, 0x5a)),
            "grey text must not outvote a red one"
        );
    }

    /// A highlighted line is ink even where its cells have no text, because a selected
    /// or reversed row is exactly what the overview should show.
    #[test]
    fn a_cell_with_only_a_background_counts_as_ink() {
        let mut grid = Grid::blank(12, 24);
        for col in 0..24u16 {
            if let Some(cell) = grid.cell_mut(5, col) {
                cell.bg = Some(Rgb::new(0x2a, 0x3a, 0x50));
            }
        }
        let thumbnail = downsample(&grid, T0);
        let block = thumbnail.block(5, 0).expect("the highlighted row");
        assert!(block.ink > 0.0, "a highlighted row must be visible");
    }

    /// A TUI's thumbnail is a solid rectangle and would otherwise be indistinguishable
    /// from a wall of output, so the flag travels with it.
    #[test]
    fn a_full_screen_program_is_marked_so_the_overview_can_say_so() {
        let mut grid = grid_with_text(20, 40, &["lazygit"]);
        grid.alternate_screen = true;
        assert!(downsample(&grid, T0).alternate_screen);
        assert!(!downsample(&Grid::blank(20, 40), T0).alternate_screen);
    }

    #[test]
    fn a_grid_smaller_than_a_thumbnail_is_not_stretched_into_empty_blocks() {
        let thumbnail = downsample(&grid_with_text(4, 6, &["ab", "cd"]), T0);
        assert_eq!(thumbnail.rows, 4);
        assert_eq!(thumbnail.cols, 6);
        assert_eq!(thumbnail.blocks.len(), 24);
        assert!(
            thumbnail.block(4, 0).is_none(),
            "out of range must not wrap"
        );
    }

    #[test]
    fn closing_a_session_forgets_its_picture() {
        let mut thumbnails = Thumbnails::new();
        let id = session("sess_gone0000001");
        thumbnails.refresh(&id, &grid_with_text(10, 20, &["x"]), T0);
        assert_eq!(thumbnails.len(), 1);
        thumbnails.forget(&id);
        assert!(thumbnails.is_empty());
        assert!(thumbnails.get(&id).is_none());
    }

    #[test]
    fn a_thumbnail_records_when_it_was_taken_so_a_stale_one_can_be_labelled() {
        let mut thumbnails = Thumbnails::new();
        let id = session("sess_when0000001");
        thumbnails.refresh(&id, &grid_with_text(10, 20, &["x"]), T0);
        assert_eq!(thumbnails.get(&id).map(|t| t.taken_ms), Some(T0));
    }
}
