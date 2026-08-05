//! The terminal pane: where users live.
//!
//! ## Painting by runs, not by cells
//!
//! A 40x120 pane is 4,800 cells. One text draw per cell would be 4,800 galleys a
//! frame, per pane, and a desk of thirty sessions would be hopeless. So a row is
//! painted as **runs** — consecutive cells sharing a colour and an attribute set — and
//! the run encoding already exists: `Grid::row_runs` is the same function the protocol
//! uses to put the row on the wire. A prompt line is three draw calls instead of a
//! hundred and twenty, and there is only one definition of what a run is.
//!
//! ## Only what is on screen
//!
//! A pane clipped by the window paints only the rows inside the clip rectangle.
//! [`visible_rows`] works that out, and it is a pure function so the arithmetic is
//! tested rather than eyeballed.
//!
//! ## The accessibility tree
//!
//! A GPU-drawn terminal has no DOM, so if nothing is put in the AccessKit tree the pane
//! does not exist for a screen reader. The pane therefore registers itself with
//! `Role::Terminal` and its screen as the node's value — the lines, not four thousand
//! one-character labels.

pub mod feed;
pub mod keys;
pub mod mouse;
pub mod selection;

use egui::{Align2, Color32, FontId, Pos2, Rect, Response, Sense, Stroke, Ui, Vec2};
use turn_proto::cells::{CellAttrs, CellRun, Grid, Rgb};
use turn_proto::PtySize;

use crate::panes::size_in_cells;
use crate::repaint::cursor_visible;
use crate::theme::Theme;
use selection::{cell_at, CellPos, Selection, SelectionKind};

/// What a pane wants done as a result of the user's input.
///
/// Returned rather than performed, so the pane draws and the application decides. That
/// is also what keeps the product rules out of the draw code: a pane cannot move focus
/// or approve anything, it can only report that a key was pressed.
#[derive(Debug, Clone, PartialEq)]
pub enum PaneAction {
    /// Bytes typed into the pane's pty. Pending permission prompts can be answered
    /// only through this direct human-input action.
    Write(Vec<u8>),
    /// The pane is a different size in cells and the pty must be told.
    Resize(PtySize),
    /// The user clicked in this pane.
    Focus,
    /// Text to put on the clipboard.
    Copy(String),
    /// The viewport should move by this many rows, positive being backwards into
    /// history.
    Scroll(i32),
}

/// How a pane is being shown this frame.
#[derive(Debug, Clone, Copy)]
pub struct PaneOptions {
    /// Whether this pane is visually focused. Only a focused pane blinks its cursor,
    /// and normally only a focused pane receives keystrokes. This is deliberately visual
    /// state rather than the final input gate: an onboarding sheet can sit over a pane
    /// that remains focused in the persisted Layout.
    pub focused: bool,
    /// Whether keyboard events may be encoded for the PTY in this frame.
    ///
    /// Kept separate from [`Self::focused`] so a modal sheet can hold the window's
    /// keyboard lease without lying about which pane will be focused when it closes.
    /// Callers must set this to false whenever a sensitive overlay is open.
    pub accepts_input: bool,
    /// Wall-clock milliseconds, for the cursor phase.
    pub now_ms: i64,
    /// Whether the pane is showing history rather than the live screen, which suppresses
    /// the cursor and shows a marker instead.
    pub scrolled: bool,
    /// Whether Turn's record of this pane is complete, so the marker can say where the
    /// record begins rather than implying it goes back for ever.
    pub history_complete: bool,
}

impl Default for PaneOptions {
    fn default() -> Self {
        Self {
            focused: false,
            accepts_input: false,
            now_ms: 0,
            scrolled: false,
            history_complete: true,
        }
    }
}

/// What the pane remembers between frames.
#[derive(Debug, Clone, Default)]
pub struct PaneInteraction {
    pub selection: Option<Selection>,
    /// The size in cells last reported, so a resize request is sent on a change rather
    /// than every frame.
    reported_size: Option<PtySize>,
}

impl PaneInteraction {
    /// The selected text, if any.
    ///
    /// `None` rather than an empty string for a selection covering nothing: a stray
    /// click must not silently wipe whatever the user had on the clipboard.
    pub fn selected_text(&self, grid: &Grid) -> Option<String> {
        let selection = self.selection.as_ref()?;
        let text = selection.text(grid);
        if text.is_empty() {
            None
        } else {
            Some(text)
        }
    }

    pub fn clear_selection(&mut self) {
        self.selection = None;
    }

    /// The size last reported to the pty, for a caller that wants to avoid asking twice.
    pub fn reported_size(&self) -> Option<PtySize> {
        self.reported_size
    }
}

/// How tall the "you are looking at history" bar is.
pub const SCROLL_MARKER_HEIGHT: f32 = 18.0;

/// Where row zero of the grid is drawn.
///
/// A scrolled pane gives the marker its own strip rather than drawing it over the top
/// row: covering the first line of output is exactly the wrong thing to do to somebody
/// who scrolled back in order to read it. Shared by the painting and the pointer
/// arithmetic, because a selection that disagreed with the paint by eighteen points
/// would put the highlight on the wrong line.
pub fn grid_origin(rect: Rect, options: PaneOptions) -> Pos2 {
    if options.scrolled {
        rect.min + Vec2::new(0.0, SCROLL_MARKER_HEIGHT)
    } else {
        rect.min
    }
}

/// The rows of `grid` that fall inside `clip`, given where row zero is drawn.
///
/// Inclusive of the partially visible rows at each edge: a row half off the bottom of
/// the window still has to be drawn, or the pane appears to end early.
pub fn visible_rows(grid: &Grid, origin: Pos2, cell: Vec2, clip: Rect) -> std::ops::Range<u16> {
    if cell.y <= 0.0 || grid.rows == 0 || clip.max.y <= clip.min.y {
        return 0..0;
    }
    let first = ((clip.min.y - origin.y) / cell.y).floor().max(0.0);
    let last = ((clip.max.y - origin.y) / cell.y).ceil().max(0.0);
    let first = (first as u32).min(grid.rows as u32) as u16;
    let last = (last as u32).min(grid.rows as u32) as u16;
    first..last.max(first)
}

/// The colours a run is painted in, with reversal, dimming and selection applied.
fn colours(run: &CellRun, theme: &Theme, selected: bool) -> (Color32, Option<Color32>) {
    let mut fg = run.fg.map(to_colour).unwrap_or(theme.text);
    let mut bg = run.bg.map(to_colour);

    // The protocol resolves reversed video when it can, and sets this flag only for the
    // one case it cannot: both colours were the theme's own. A client that swapped a
    // cell without the flag would reverse it twice and end up invisible.
    if run.attrs.has(CellAttrs::INVERSE) {
        bg = Some(fg);
        fg = theme.background;
    }
    if run.attrs.has(CellAttrs::DIM) {
        fg = fg.gamma_multiply(0.6);
    }
    if selected {
        bg = Some(theme.selection);
    }
    (fg, bg)
}

fn to_colour(rgb: Rgb) -> Color32 {
    Color32::from_rgb(rgb.0, rgb.1, rgb.2)
}

/// Paints a grid.
///
/// Pure drawing: no input and no state, so a snapshot test can exercise the painting
/// without an interaction model in the way.
pub fn paint(
    ui: &Ui,
    theme: &Theme,
    rect: Rect,
    grid: &Grid,
    selection: Option<&Selection>,
    options: PaneOptions,
) {
    let painter = ui.painter().with_clip_rect(rect);
    painter.rect_filled(rect, 0.0, theme.background);

    let cell = theme.cell_size;
    let origin = grid_origin(rect, options);
    let clip = rect.intersect(ui.clip_rect());

    for row in visible_rows(grid, origin, cell, clip) {
        let mut col: u16 = 0;
        for run in grid.row_runs(row) {
            let width = run.cells;
            if width == 0 {
                continue;
            }
            // A run may be partly selected, so a run straddling the boundary is painted
            // as two spans rather than being highlighted whole.
            let mut span_start = col;
            while span_start < col + width {
                let selected = selection.is_some_and(|s| s.contains(row, span_start));
                let mut span_end = span_start + 1;
                while span_end < col + width
                    && selection.is_some_and(|s| s.contains(row, span_end)) == selected
                {
                    span_end += 1;
                }
                paint_span(
                    &painter, theme, &run, col, span_start, span_end, origin, cell, row, selected,
                );
                span_start = span_end;
            }
            col = col.saturating_add(width);
        }
    }

    paint_cursor(&painter, theme, grid, origin, cell, options);
    paint_scroll_marker(&painter, theme, rect, grid, options);
}

/// Paints one contiguous, uniformly selected part of a run.
#[allow(clippy::too_many_arguments)]
fn paint_span(
    painter: &egui::Painter,
    theme: &Theme,
    run: &CellRun,
    run_start: u16,
    from: u16,
    to: u16,
    origin: Pos2,
    cell: Vec2,
    row: u16,
    selected: bool,
) {
    let (fg, bg) = colours(run, theme, selected);
    let span = Rect::from_min_size(
        origin + Vec2::new(from as f32 * cell.x, row as f32 * cell.y),
        Vec2::new((to - from) as f32 * cell.x, cell.y),
    );
    if let Some(bg) = bg {
        painter.rect_filled(span, 0.0, bg);
    }
    if run.text.is_empty() {
        return;
    }

    // Only the characters of this span, so a partly selected run is not drawn twice. A
    // wide cell is one glyph in a one-cell run, so it is taken whole.
    let text: String = if run.attrs.has(CellAttrs::WIDE) {
        run.text.clone()
    } else {
        run.text
            .chars()
            .skip((from - run_start) as usize)
            .take((to - from) as usize)
            .collect()
    };
    if text.is_empty() {
        return;
    }

    // Weight comes from the family, and egui's default monospace has one face. The
    // honest rendering of bold is therefore a brighter colour, which keeps every glyph
    // on its own column — faking weight by shearing or double-drawing would break the
    // grid, which is the one thing a terminal cannot afford.
    let colour = if run.attrs.has(CellAttrs::BOLD) {
        fg.gamma_multiply(1.4)
    } else {
        fg
    };
    painter.text(
        span.left_top(),
        Align2::LEFT_TOP,
        text,
        theme.mono.clone(),
        colour,
    );
    if run.attrs.has(CellAttrs::UNDERLINE) {
        painter.hline(span.x_range(), span.max.y - 2.0, Stroke::new(1.0, colour));
    }
    if run.attrs.has(CellAttrs::ITALIC) {
        // Same reason as bold: no italic face, so the slant is expressed as a faint rule
        // rather than by shearing glyphs off their columns.
        painter.hline(
            span.x_range(),
            span.min.y + 1.0,
            Stroke::new(1.0, colour.gamma_multiply(0.4)),
        );
    }
}

fn paint_cursor(
    painter: &egui::Painter,
    theme: &Theme,
    grid: &Grid,
    origin: Pos2,
    cell: Vec2,
    options: PaneOptions,
) {
    // No cursor in a scrolled view: the cursor is on the live screen, and drawing it at
    // the same coordinates over history would put it on an unrelated character.
    if options.scrolled {
        return;
    }
    let Some((row, col)) = grid.cursor else {
        return;
    };
    if row >= grid.rows || col >= grid.cols {
        return;
    }
    // Only a focused pane blinks. Thirty blinking cursors would be a distraction, and a
    // repaint every half second per pane.
    if options.focused && !cursor_visible(options.now_ms, true) {
        return;
    }
    let at = Rect::from_min_size(
        origin + Vec2::new(col as f32 * cell.x, row as f32 * cell.y),
        cell,
    );
    if options.focused {
        // A filled block with the character knocked out of it, which is what a terminal
        // cursor looks like and is legible at any font size.
        painter.rect_filled(at, 0.0, theme.cursor);
        if let Some(under) = grid.cell(row, col) {
            if !under.text.is_empty() {
                painter.text(
                    at.left_top(),
                    Align2::LEFT_TOP,
                    &under.text,
                    theme.mono.clone(),
                    theme.background,
                );
            }
        }
    } else {
        // An outline for an unfocused pane: present, so the user can see where typing
        // would go, without competing for attention.
        painter.rect_stroke(
            at,
            0.0,
            Stroke::new(1.0, theme.text_faint),
            egui::StrokeKind::Inside,
        );
    }
}

/// The words shown when a pane is scrolled back.
///
/// A function rather than inline formatting so the one thing worth checking — that an
/// incomplete record says so — is testable without a window.
pub fn scroll_marker_label(rows: usize, history_complete: bool) -> String {
    if history_complete {
        format!("{rows} rows back · any key returns")
    } else {
        // Short enough to survive a narrow pane, because the half of this sentence that
        // matters is the half that says the record is partial.
        format!("{rows} rows back · record starts here")
    }
}

fn paint_scroll_marker(
    painter: &egui::Painter,
    theme: &Theme,
    rect: Rect,
    grid: &Grid,
    options: PaneOptions,
) {
    if !options.scrolled {
        return;
    }
    let bar = Rect::from_min_size(rect.min, Vec2::new(rect.width(), SCROLL_MARKER_HEIGHT));
    painter.rect_filled(bar, 0.0, theme.raised);
    painter.hline(bar.x_range(), bar.max.y, Stroke::new(1.0, theme.border));
    painter.text(
        bar.left_center() + Vec2::new(8.0, 0.0),
        Align2::LEFT_CENTER,
        scroll_marker_label(grid.scrollback_offset, options.history_complete),
        FontId::new(11.0, egui::FontFamily::Monospace),
        if options.history_complete {
            theme.text_dim
        } else {
            theme.attention
        },
    );
}

/// Draws a pane and collects what the user did to it.
///
/// `id` distinguishes panes so egui tracks their interaction independently, which is
/// what lets two panes hold separate selections.
pub fn show(
    ui: &mut Ui,
    theme: &Theme,
    rect: Rect,
    grid: &Grid,
    state: &mut PaneInteraction,
    options: PaneOptions,
    id: egui::Id,
) -> Vec<PaneAction> {
    let mut actions = Vec::new();
    let response = ui.interact(rect, id, Sense::click_and_drag());

    // The size in cells, reported when it changes rather than every frame: a
    // `resize_pty` per frame during a window drag would be hundreds of requests. Measured
    // from the *live* geometry, without the scroll marker's strip: the pty's size must not
    // change just because the user looked at history.
    let size = size_in_cells(rect, theme.cell_size);
    if state.reported_size != Some(size) {
        state.reported_size = Some(size);
        actions.push(PaneAction::Resize(size));
    }

    paint(ui, theme, rect, grid, state.selection.as_ref(), options);
    describe_for_screen_reader(&response, grid, options);

    if response.clicked() && !options.focused {
        actions.push(PaneAction::Focus);
    }
    collect_pointer(
        ui,
        &response,
        theme,
        rect,
        grid,
        state,
        options,
        &mut actions,
    );
    if options.focused && options.accepts_input {
        collect_keys(ui, grid, state, &mut actions);
    }
    actions
}

/// Puts the pane in the accessibility tree.
///
/// Without this the pane does not exist for a screen reader: there is no DOM behind a
/// GPU-drawn terminal, and a wall of individually labelled cells would be unusable even
/// if there were. The value is the screen's lines, which is what a screen reader reads.
fn describe_for_screen_reader(response: &Response, grid: &Grid, options: PaneOptions) {
    response.ctx.accesskit_node_builder(response.id, |node| {
        node.set_role(egui::accesskit::Role::Terminal);
        node.set_label(terminal_label(grid, options));
        node.set_value(grid.text());
        node.add_action(egui::accesskit::Action::Focus);
    });
}

/// The accessible name of a pane.
///
/// Says the geometry and whether the view is live, because a screen-reader user has no
/// other way to know either — and reading history as though it were the live screen is
/// the kind of confusion that costs somebody an hour.
pub fn terminal_label(grid: &Grid, options: PaneOptions) -> String {
    let mut label = format!("Terminal, {} rows by {} columns", grid.rows, grid.cols);
    if options.scrolled {
        label.push_str(&format!(", scrolled back {} rows", grid.scrollback_offset));
    }
    if grid.alternate_screen {
        label.push_str(", full-screen application");
    }
    if options.focused {
        label.push_str(", focused");
    }
    label
}

/// Selection, focus, the wheel, and mouse reporting.
#[allow(clippy::too_many_arguments)]
fn collect_pointer(
    ui: &Ui,
    response: &Response,
    theme: &Theme,
    rect: Rect,
    grid: &Grid,
    state: &mut PaneInteraction,
    options: PaneOptions,
    actions: &mut Vec<PaneAction>,
) {
    let cell = theme.cell_size;
    let modifiers = ui.input(|i| i.modifiers);
    let mouse_modifiers = mouse::MouseModifiers {
        shift: modifiers.shift,
        alt: modifiers.alt,
        ctrl: modifiers.ctrl,
    };

    // A program that asked for the mouse gets the mouse — unless Shift is held, which is
    // the universal escape hatch for selecting text inside a full-screen tool.
    let reporting = grid.modes.mouse.reports() && !modifiers.shift;

    let origin = grid_origin(rect, options);
    if let Some(pos) = response.interact_pointer_pos() {
        let at = cell_at(pos, origin, cell, grid);
        if reporting {
            let event = if response.drag_started() || response.clicked() {
                Some(mouse::MouseEvent::Press(mouse::Button::Left))
            } else if response.drag_stopped() {
                Some(mouse::MouseEvent::Release(mouse::Button::Left))
            } else if response.dragged() {
                Some(mouse::MouseEvent::Drag(mouse::Button::Left))
            } else {
                None
            };
            if let Some(event) = event {
                if let Some(bytes) =
                    mouse::encode(event, at.row, at.col, mouse_modifiers, grid.modes.mouse)
                {
                    actions.push(PaneAction::Write(bytes));
                }
            }
        } else {
            // Turn's own selection. Alt makes it a block, for copying one column out of
            // tabular output.
            let kind = if modifiers.alt {
                SelectionKind::Block
            } else {
                SelectionKind::Linear
            };
            if response.drag_started() {
                state.selection = Some(Selection::new(at, kind));
            } else if response.dragged() {
                if let Some(selection) = &mut state.selection {
                    selection.extend_to(at);
                }
            } else if response.clicked() {
                // A plain click clears rather than starting an empty selection, so the
                // next copy does not silently produce nothing.
                state.clear_selection();
            }
        }
    }

    // The wheel: a mouse report if the program asked for one, otherwise Turn's history.
    if response.hovered() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll.abs() > 0.5 && cell.y > 0.0 {
            let notches = (scroll / cell.y).round() as i32;
            if notches != 0 {
                if reporting {
                    let button = if notches > 0 {
                        mouse::Button::WheelUp
                    } else {
                        mouse::Button::WheelDown
                    };
                    let at = ui
                        .input(|i| i.pointer.hover_pos())
                        .map(|pos| cell_at(pos, origin, cell, grid))
                        .unwrap_or(CellPos::new(0, 0));
                    // Bounded, so a trackpad flick cannot put a hundred reports on the
                    // socket in one frame.
                    for _ in 0..notches.abs().min(8) {
                        if let Some(bytes) = mouse::encode(
                            mouse::MouseEvent::Wheel(button),
                            at.row,
                            at.col,
                            mouse_modifiers,
                            grid.modes.mouse,
                        ) {
                            actions.push(PaneAction::Write(bytes));
                        }
                    }
                } else if !grid.alternate_screen {
                    actions.push(PaneAction::Scroll(notches));
                }
            }
        }
    }
}

/// Keystrokes, text, copy and paste.
fn collect_keys(ui: &Ui, grid: &Grid, state: &mut PaneInteraction, actions: &mut Vec<PaneAction>) {
    let events = ui.input(|i| i.events.clone());
    for event in events {
        match event {
            egui::Event::Text(text) => {
                if !text.is_empty() {
                    actions.push(PaneAction::Write(keys::encode_text(&text)));
                }
            }
            egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } => {
                if let Some(bytes) = keys::encode_key(key, &modifiers, &grid.modes) {
                    actions.push(PaneAction::Write(bytes));
                }
            }
            egui::Event::Copy | egui::Event::Cut => {
                if let Some(text) = state.selected_text(grid) {
                    actions.push(PaneAction::Copy(text));
                }
            }
            egui::Event::Paste(text) => {
                actions.push(PaneAction::Write(keys::encode_paste(&text, &grid.modes)));
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid() -> Grid {
        Grid::blank(40, 120)
    }

    /// The performance rule: a pane clipped by the window paints the rows on screen and
    /// not the forty it has.
    #[test]
    fn only_the_rows_inside_the_clip_rectangle_are_painted() {
        let cell = Vec2::new(8.0, 17.0);
        // A window showing the middle of the pane.
        let clip = Rect::from_min_max(Pos2::new(0.0, 170.0), Pos2::new(960.0, 340.0));
        assert_eq!(visible_rows(&grid(), Pos2::ZERO, cell, clip), 10..20);
    }

    /// A row half off the edge still has to be drawn, or the pane looks like it ends
    /// early.
    #[test]
    fn a_partly_visible_row_is_still_painted() {
        let cell = Vec2::new(8.0, 17.0);
        let clip = Rect::from_min_max(Pos2::new(0.0, 8.0), Pos2::new(960.0, 26.0));
        assert_eq!(
            visible_rows(&grid(), Pos2::ZERO, cell, clip),
            0..2,
            "the halves of rows 0 and 1 are both on screen"
        );
    }

    #[test]
    fn the_visible_range_is_clamped_to_the_grid_rather_than_running_past_it() {
        let cell = Vec2::new(8.0, 17.0);
        let huge = Rect::from_min_max(Pos2::new(0.0, -1_000.0), Pos2::new(960.0, 100_000.0));
        assert_eq!(visible_rows(&grid(), Pos2::ZERO, cell, huge), 0..40);

        // A pane scrolled entirely off the top of the window paints nothing.
        let above = Rect::from_min_max(Pos2::new(0.0, -500.0), Pos2::new(960.0, -400.0));
        assert!(visible_rows(&grid(), Pos2::ZERO, cell, above).is_empty());
    }

    #[test]
    fn a_pane_with_no_cell_size_yet_paints_nothing_rather_than_dividing_by_zero() {
        let clip = Rect::from_min_max(Pos2::ZERO, Pos2::new(100.0, 100.0));
        assert!(visible_rows(&grid(), Pos2::ZERO, Vec2::ZERO, clip).is_empty());
        let empty = Rect::from_min_max(Pos2::ZERO, Pos2::ZERO);
        assert!(visible_rows(&grid(), Pos2::ZERO, Vec2::new(8.0, 17.0), empty).is_empty());
    }

    /// The protocol resolves reversed video where it can and flags only the case it
    /// cannot. A client that swapped an unflagged cell would reverse it twice.
    #[test]
    fn only_a_flagged_cell_is_reversed_by_the_renderer() {
        let theme = Theme::dark();
        let plain = CellRun {
            text: "a".into(),
            cells: 1,
            fg: Some(Rgb::new(0, 200, 0)),
            bg: None,
            attrs: CellAttrs::default(),
        };
        let (fg, bg) = colours(&plain, &theme, false);
        assert_eq!(fg, Color32::from_rgb(0, 200, 0));
        assert_eq!(bg, None, "an unflagged cell must be painted as it arrived");

        let flagged = CellRun {
            attrs: CellAttrs::default().with(CellAttrs::INVERSE),
            fg: None,
            ..plain
        };
        let (fg, bg) = colours(&flagged, &theme, false);
        assert_eq!(
            bg,
            Some(theme.text),
            "the theme's foreground moves to the back"
        );
        assert_eq!(fg, theme.background);
    }

    #[test]
    fn a_selected_run_is_painted_on_the_selection_colour_whatever_it_asked_for() {
        let theme = Theme::dark();
        let run = CellRun {
            text: "a".into(),
            cells: 1,
            fg: None,
            bg: Some(Rgb::new(90, 0, 0)),
            attrs: CellAttrs::default(),
        };
        assert_eq!(colours(&run, &theme, true).1, Some(theme.selection));
    }

    #[test]
    fn a_pane_remembers_nothing_selected_until_something_is() {
        let mut state = PaneInteraction::default();
        assert_eq!(state.selected_text(&grid()), None);

        let text = Grid::from_lines(&["hello"], 10);
        let mut selection = Selection::new(CellPos::new(0, 0), SelectionKind::Linear);
        selection.extend_to(CellPos::new(0, 3));
        state.selection = Some(selection);
        assert_eq!(state.selected_text(&text).as_deref(), Some("hel"));

        state.clear_selection();
        assert_eq!(state.selected_text(&text), None);
    }

    /// An empty selection must not put an empty string on the clipboard: a stray click
    /// would silently wipe whatever the user had copied.
    #[test]
    fn a_selection_covering_nothing_offers_no_text_to_copy() {
        let state = PaneInteraction {
            selection: Some(Selection::new(CellPos::new(2, 2), SelectionKind::Linear)),
            ..PaneInteraction::default()
        };
        assert_eq!(state.selected_text(&Grid::from_lines(&["hello"], 10)), None);
    }

    /// A screen-reader user has no other way to know that what they are reading is
    /// history rather than what the program is doing now.
    #[test]
    fn the_accessible_name_says_whether_the_view_is_live() {
        let mut grid = Grid::blank(24, 80);
        let live = terminal_label(&grid, PaneOptions::default());
        assert!(live.contains("24 rows by 80 columns"), "got {live}");
        assert!(!live.contains("scrolled"));

        grid.scrollback_offset = 12;
        let scrolled = terminal_label(
            &grid,
            PaneOptions {
                scrolled: true,
                ..PaneOptions::default()
            },
        );
        assert!(scrolled.contains("scrolled back 12 rows"), "got {scrolled}");

        grid.alternate_screen = true;
        let tui = terminal_label(
            &grid,
            PaneOptions {
                focused: true,
                ..PaneOptions::default()
            },
        );
        assert!(tui.contains("full-screen application"), "got {tui}");
        assert!(tui.contains("focused"));
    }

    /// A user who scrolled back did so in order to read the top line, so the marker must
    /// not be painted over it.
    #[test]
    fn the_scroll_marker_takes_its_own_strip_rather_than_covering_the_first_row() {
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 340.0));
        let live = grid_origin(rect, PaneOptions::default());
        assert_eq!(live, rect.min);

        let scrolled = grid_origin(
            rect,
            PaneOptions {
                scrolled: true,
                ..PaneOptions::default()
            },
        );
        assert_eq!(scrolled.y, rect.min.y + SCROLL_MARKER_HEIGHT);
        assert_eq!(scrolled.x, rect.min.x);
        // A strip too short to hold its own text would be a marker nobody can read.
        const _: () = assert!(SCROLL_MARKER_HEIGHT >= 12.0);
    }

    /// Looking at history must not resize the process. A pty told it lost a row every
    /// time somebody scrolled would reflow the program under them.
    #[test]
    fn scrolling_back_does_not_change_the_size_the_pty_is_told() {
        let theme = Theme::dark();
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(8.0 * 40.0, 17.0 * 20.0));
        assert_eq!(
            size_in_cells(rect, theme.cell_size),
            PtySize::new(20, 40),
            "the size is measured from the pane, not from what is left after the marker"
        );
    }

    /// The user must be told when the history they are looking at does not go all the
    /// way back, rather than being left to assume it does.
    #[test]
    fn the_scroll_marker_says_when_the_record_is_incomplete() {
        let complete = scroll_marker_label(40, true);
        assert!(complete.contains("40 rows"));
        assert!(!complete.contains("not kept"));

        let partial = scroll_marker_label(40, false);
        assert!(
            partial.contains("starts here"),
            "a partial record must say where it begins: {partial}"
        );
        assert_ne!(
            partial, complete,
            "the two cases must not read the same, or the warning is invisible"
        );
    }
}
