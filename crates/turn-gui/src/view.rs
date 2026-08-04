//! The window: status bar, sidebar, panes, attention queue, palette.
//!
//! Everything here is a function of display data the daemon supplied. [`TurnView`] is
//! deliberately a plain description of what is on screen rather than a handle on the
//! application, for two reasons: a snapshot test can construct any state it likes
//! without a socket, and nothing in the drawing code can compute a product rule,
//! because it has nothing to compute one from.
//!
//! What the user does comes back as [`ViewAction`]s. The window therefore cannot move
//! focus, approve a permission or start a process on its own — it can only report that
//! something was clicked, which is what keeps those guarantees out of reach of the draw
//! code.
//!
//! ## Accessibility is not a layer on top
//!
//! A GPU-drawn window has no DOM. If the rows are only pixels then a screen-reader user
//! has no window at all, and no snapshot test can catch that — which is why the session
//! list is composed of real, sensed widgets that put a `ListItem` node with a name and a
//! selected state into the AccessKit tree, and why `tests/snapshots.rs` drives that tree
//! rather than a parallel description of it.

use egui::{Align2, Color32, FontId, Rect, Response, RichText, Sense, Stroke, Ui, Vec2};
use turn_core::attention::AttentionPolicy;
use turn_core::event::Risk;
use turn_core::ids::{AttentionId, PaneId, SessionId};
use turn_core::model::Layout;
use turn_core::state::{AwaitingReason, DisplayState};
use turn_proto::cells::Grid;

use crate::keymap::{Command, Keymap};
use crate::palette::{self, Palette};
use crate::panes::{self, Arrangement, Divider, Side};
use crate::terminal::{self, PaneAction, PaneInteraction, PaneOptions};
use crate::theme::Theme;
use crate::thumbnails::{Thumbnail, Thumbnails};
use crate::transport::ConnectionState;

/// One row in the session list. The daemon supplies these already derived; the client
/// never computes a state.
#[derive(Debug, Clone)]
pub struct SessionRow {
    pub id: SessionId,
    pub name: String,
    pub state: DisplayState,
    /// The daemon's own word for the state — `YOUR TURN`, `PERMISSION`, `running`.
    /// Carried rather than derived so there is one wording, on the daemon's side.
    pub state_label: String,
    pub detail: String,
    pub badge: usize,
    /// True when the state came from a heuristic rather than the tool itself.
    pub provisional: bool,
    pub depth: usize,
    /// Silenced. A muted session still badges: muting quietens the interruption, not
    /// the evidence.
    pub muted: bool,
}

impl SessionRow {
    /// The accessible name: everything the visuals say, in words.
    ///
    /// A screen-reader user gets the state, whether it is a guess, the badge and the
    /// mute — the four things the row expresses with colour, a glyph and position.
    pub fn accessible_name(&self) -> String {
        let mut name = format!("{} — {}", self.name, self.state_label);
        if self.provisional {
            name.push_str(" (inferred)");
        }
        if !self.detail.is_empty() {
            name.push_str(&format!(" · {}", self.detail));
        }
        if self.badge > 0 {
            name.push_str(&format!(" · {} waiting", self.badge));
        }
        if self.muted {
            name.push_str(" · muted");
        }
        name
    }
}

/// A permission the user has to answer.
#[derive(Debug, Clone)]
pub struct PendingPermission {
    /// Which demand this is, so acting on the banner acts on what it is showing rather
    /// than on whatever happens to be first in the queue.
    pub attention_id: Option<AttentionId>,
    pub session_id: SessionId,
    pub session: String,
    pub summary: String,
    pub command: Option<String>,
    pub cwd: String,
    pub tool: String,
    pub risk: Risk,
    pub blocked_secs: u64,
    /// True when a heuristic inferred this rather than a hook reporting it.
    pub provisional: bool,
}

/// One demand in the queue.
#[derive(Debug, Clone)]
pub struct QueueItem {
    pub attention_id: AttentionId,
    pub session_id: SessionId,
    pub session_name: String,
    pub reason: AwaitingReason,
    pub summary: Option<String>,
    pub provisional: bool,
    /// A snoozed demand is still listed — hiding it would make a snooze feel like a
    /// deletion — and drawn as unavailable.
    pub actionable: bool,
}

impl QueueItem {
    /// The word for what is being asked. Never a colour on its own.
    pub fn reason_label(&self) -> &'static str {
        match self.reason {
            AwaitingReason::Permission => "permission",
            AwaitingReason::Question => "question",
            AwaitingReason::Credentials => "credentials",
            AwaitingReason::Input => "your turn",
        }
    }
}

/// One pane, with the screen to paint in it.
///
/// Borrowed rather than owned: a grid is the largest thing in the window and cloning
/// one per pane per frame would undo the work the run encoding does.
#[derive(Debug)]
pub struct PaneContent<'a> {
    pub pane_id: PaneId,
    pub title: String,
    pub grid: &'a Grid,
    pub focused: bool,
    /// Whether this pane is showing history rather than the live screen.
    pub scrolled: bool,
    /// Whether Turn's record of this pane reaches back to the attach.
    pub history_complete: bool,
}

/// The session overview: a thumbnail per session.
#[derive(Debug, Clone, Default)]
pub struct Overview {
    pub open: bool,
}

/// What the window is showing.
#[derive(Debug, Default)]
pub struct TurnView<'a> {
    pub sessions: Vec<SessionRow>,
    pub selected: Option<SessionId>,
    /// The daemon's layout for the selected session, which is what decides the
    /// geometry. `None` before the first `get_session` answers.
    pub layout: Option<Layout>,
    pub panes: Vec<PaneContent<'a>>,
    /// One live screen per session, for the overview's thumbnails.
    ///
    /// Separate from `panes` because they answer different questions: `panes` is what is
    /// on screen in the session being worked on, and this is one picture per session
    /// across the whole desk.
    pub overview_screens: Vec<(SessionId, &'a Grid)>,
    pub permission: Option<PendingPermission>,
    pub queue: Vec<QueueItem>,
    pub connection: Option<ConnectionState>,
    /// A failure worth showing, from a request that did not work.
    pub notice: Option<String>,
    pub overview: Overview,
    /// The attention policy in force, for the settings sheet.
    pub policy: Option<AttentionPolicy>,
    pub now_ms: i64,
}

/// The window's own mutable state: what is typed in the palette, and what is selected
/// in each pane.
#[derive(Debug, Default)]
pub struct ViewState {
    pub palette: Palette,
    pub panes: std::collections::HashMap<PaneId, PaneInteraction>,
    pub thumbnails: Thumbnails,
    /// Which command sheet is open, if any.
    pub shortcuts_open: bool,
    pub settings_open: bool,
}

impl ViewState {
    /// The interaction state for a pane, created on first sight.
    pub fn pane(&mut self, id: &PaneId) -> &mut PaneInteraction {
        self.panes.entry(id.clone()).or_default()
    }

    /// Whether anything is on screen that must not be interrupted.
    ///
    /// Fed straight to `update_user_activity`, which is what stops the focus governor
    /// moving somebody who is halfway through reading a permission prompt or choosing a
    /// command.
    pub fn is_sensitive(&self) -> bool {
        self.palette.open || self.shortcuts_open || self.settings_open
    }
}

/// What the user did.
#[derive(Debug, Clone, PartialEq)]
pub enum ViewAction {
    SelectSession(SessionId),
    /// A command the user chose, from a shortcut, the palette or a button.
    Run(Command),
    /// Something the user did inside a pane.
    Pane {
        pane_id: PaneId,
        action: PaneAction,
    },
    /// A divider was dragged. The fraction is of the parent split, which is what
    /// `resize_pane` wants.
    ResizeDivider {
        pane_id: PaneId,
        fraction: f32,
    },
    /// Go to a specific demand — the one the banner or the row is showing, never an
    /// arbitrary one.
    GotoAttention(AttentionId),
    DismissAttention(AttentionId),
    /// Close a sheet.
    CloseOverlay,
}

/// A region of the window, with an id of its own.
///
/// The salt is not decoration. `egui` derives a widget's id from its parent plus a
/// counter, so two sibling regions that lay out the same number of widgets produce the
/// same ids — and two widgets sharing an id share their interaction state, which shows up
/// as the command palette scrolling the session list. Naming each region makes that
/// impossible rather than unlikely.
fn region(rect: Rect, name: &'static str) -> egui::UiBuilder {
    egui::UiBuilder::new().max_rect(rect).id_salt(name)
}

const SIDEBAR_WIDTH: f32 = 264.0;
const QUEUE_WIDTH: f32 = 240.0;
const STATUS_HEIGHT: f32 = 26.0;
const ROW_HEIGHT: f32 = 40.0;
const PANE_HEADER: f32 = 22.0;

impl<'a> TurnView<'a> {
    /// Draws the whole window and returns what the user did.
    pub fn ui(
        &self,
        ui: &mut Ui,
        theme: &Theme,
        keymap: &Keymap,
        state: &mut ViewState,
    ) -> Vec<ViewAction> {
        let mut actions = Vec::new();
        let full = ui.available_rect_before_wrap();
        ui.painter().rect_filled(full, 0.0, theme.background);

        self.status_bar(ui, theme);
        if let Some(permission) = &self.permission {
            actions.extend(self.permission_banner(ui, theme, permission));
        }
        if let Some(notice) = &self.notice {
            self.notice_bar(ui, theme, notice);
        }

        let body = ui.available_rect_before_wrap();
        let queue_width = if self.queue.is_empty() {
            0.0
        } else {
            QUEUE_WIDTH
        };
        let sidebar = Rect::from_min_size(body.min, Vec2::new(SIDEBAR_WIDTH, body.height()));
        let centre = Rect::from_min_size(
            body.min + Vec2::new(SIDEBAR_WIDTH, 0.0),
            Vec2::new(
                (body.width() - SIDEBAR_WIDTH - queue_width).max(0.0),
                body.height(),
            ),
        );

        ui.scope_builder(region(sidebar, "sidebar"), |ui| {
            actions.extend(self.sidebar(ui, theme));
        });
        ui.painter()
            .vline(centre.min.x, body.y_range(), Stroke::new(1.0, theme.border));

        ui.scope_builder(region(centre.shrink(1.0), "panes"), |ui| {
            if self.overview.open {
                actions.extend(self.overview_grid(ui, theme, state));
            } else {
                actions.extend(self.pane_area(ui, theme, keymap, state));
            }
        });

        if queue_width > 0.0 {
            let queue = Rect::from_min_size(
                body.min + Vec2::new(body.width() - queue_width, 0.0),
                Vec2::new(queue_width, body.height()),
            );
            ui.painter()
                .vline(queue.min.x, body.y_range(), Stroke::new(1.0, theme.border));
            ui.scope_builder(region(queue.shrink(1.0), "queue"), |ui| {
                actions.extend(self.queue_panel(ui, theme));
            });
        }

        if state.palette.open {
            actions.extend(self.palette_overlay(ui, theme, keymap, state, full));
        } else if state.shortcuts_open {
            actions.extend(self.shortcuts_sheet(ui, theme, keymap, full));
        } else if state.settings_open {
            actions.extend(self.settings_sheet(ui, theme, full));
        }
        actions
    }

    fn status_bar(&self, ui: &mut Ui, theme: &Theme) {
        let rect = Rect::from_min_size(
            ui.available_rect_before_wrap().min,
            Vec2::new(ui.available_width(), STATUS_HEIGHT),
        );
        ui.painter().rect_filled(rect, 0.0, theme.panel);
        ui.painter()
            .hline(rect.x_range(), rect.max.y, Stroke::new(1.0, theme.border));

        let connection = self.connection.clone().unwrap_or(ConnectionState::Starting);
        // The connection state is a glyph, a word and a sentence — never a colour on its
        // own, so it survives a greyscale screenshot and a screen reader.
        let (colour, glyph) = match &connection {
            ConnectionState::Connected { .. } => (theme.done, "●"),
            ConnectionState::Connecting { .. } | ConnectionState::Starting => (theme.running, "◌"),
            ConnectionState::Disconnected { .. } => (theme.failure, "○"),
            ConnectionState::Incompatible { .. } => (theme.failure, "×"),
        };

        ui.scope_builder(region(rect.shrink2(Vec2::new(10.0, 5.0)), "status"), |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("TURN")
                        .color(theme.text)
                        .font(FontId::new(12.0, egui::FontFamily::Monospace)),
                );
                // Monospace, deliberately: the proportional face the body text uses
                // has no glyph for these and draws a missing-glyph box, which would
                // leave the connection state signalled by colour alone.
                ui.label(
                    RichText::new(glyph)
                        .color(colour)
                        .font(FontId::new(11.0, egui::FontFamily::Monospace)),
                );
                ui.label(RichText::new(connection.word()).color(colour).small());
                ui.label(
                    RichText::new(connection.detail())
                        .color(theme.text_faint)
                        .small(),
                );

                let waiting = self
                    .sessions
                    .iter()
                    .filter(|row| row.state.demands_user())
                    .count();
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if waiting > 0 {
                        // The one loud thing on screen.
                        ui.label(
                            RichText::new(format!("needs you · {waiting}"))
                                .color(theme.attention)
                                .small(),
                        );
                    } else {
                        ui.label(
                            RichText::new("nothing waiting")
                                .color(theme.text_faint)
                                .small(),
                        );
                    }
                });
            });
        });
        ui.advance_cursor_after_rect(rect);
    }

    fn notice_bar(&self, ui: &mut Ui, theme: &Theme, notice: &str) {
        let rect = Rect::from_min_size(
            ui.available_rect_before_wrap().min,
            Vec2::new(ui.available_width(), 22.0),
        );
        ui.painter().rect_filled(rect, 0.0, theme.raised);
        ui.painter()
            .hline(rect.x_range(), rect.max.y, Stroke::new(1.0, theme.border));
        ui.painter().text(
            rect.left_center() + Vec2::new(10.0, 0.0),
            Align2::LEFT_CENTER,
            notice,
            FontId::new(11.0, egui::FontFamily::Proportional),
            theme.failure,
        );
        ui.advance_cursor_after_rect(rect);
    }

    /// The permission banner: prominent, and never modal.
    ///
    /// Modal would be wrong. The user may want to look at another session before
    /// answering, and a dialog that blocked the window would make the parallelism the
    /// product exists for impossible at the exact moment it matters.
    ///
    /// Its buttons carry the demand's own id, so "go to this" goes to *this* one rather
    /// than to whatever the queue currently ranks first — which is the bug a banner
    /// showing one demand and acting on another would produce.
    fn permission_banner(
        &self,
        ui: &mut Ui,
        theme: &Theme,
        p: &PendingPermission,
    ) -> Vec<ViewAction> {
        let mut actions = Vec::new();
        let (risk_colour, risk_word) = match p.risk {
            Risk::High => (theme.failure, "HIGH RISK"),
            Risk::Medium => (theme.attention, "MEDIUM RISK"),
            Risk::Low => (theme.text_dim, "LOW RISK"),
        };
        let height = 132.0;
        let rect = Rect::from_min_size(
            ui.available_rect_before_wrap().min,
            Vec2::new(ui.available_width(), height),
        );
        ui.painter().rect_filled(rect, 0.0, theme.raised);
        // A left rule in the risk colour, plus the word: never colour alone.
        ui.painter().rect_filled(
            Rect::from_min_size(rect.min, Vec2::new(3.0, height)),
            0.0,
            risk_colour,
        );
        ui.painter()
            .hline(rect.x_range(), rect.max.y, Stroke::new(1.0, theme.border));

        ui.scope_builder(
            region(rect.shrink2(Vec2::new(14.0, 8.0)), "permission"),
            |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("!").color(risk_colour).strong());
                    ui.label(RichText::new("PERMISSION").color(theme.attention).small());
                    ui.label(RichText::new(risk_word).color(risk_colour).small());
                    ui.label(RichText::new(&p.session).color(theme.text).strong());
                    ui.label(
                        RichText::new(format!("blocked {}s", p.blocked_secs))
                            .color(theme.text_faint)
                            .small(),
                    );
                    if p.provisional {
                        // A heuristic's opinion is drawn as a guess, always.
                        ui.label(RichText::new("inferred").color(theme.provisional).small());
                    }
                });
                ui.label(RichText::new(&p.summary).color(theme.text));
                if let Some(command) = &p.command {
                    // The command in monospace: what it will actually run, verbatim,
                    // never paraphrased.
                    ui.label(RichText::new(command).monospace().color(theme.text));
                }
                ui.label(
                    RichText::new(format!("in {}   ·   tool: {}", p.cwd, p.tool))
                        .color(theme.text_dim)
                        .small(),
                );
                ui.horizontal(|ui| {
                    // Not "Approve". Turn cannot approve anything: the only way to
                    // answer an agent is to type into its terminal, and this button
                    // takes the user there to do it.
                    if ui.button("Go to this session").clicked() {
                        match &p.attention_id {
                            Some(id) => actions.push(ViewAction::GotoAttention(id.clone())),
                            None => actions.push(ViewAction::SelectSession(p.session_id.clone())),
                        }
                    }
                    if let Some(id) = &p.attention_id {
                        if ui.button("Dismiss").clicked() {
                            actions.push(ViewAction::DismissAttention(id.clone()));
                        }
                    }
                    ui.label(
                        RichText::new("Answer in the pane — Turn never approves anything for you")
                            .color(theme.text_faint)
                            .small(),
                    );
                });
            },
        );
        ui.advance_cursor_after_rect(rect);
        actions
    }

    fn sidebar(&self, ui: &mut Ui, theme: &Theme) -> Vec<ViewAction> {
        let mut actions = Vec::new();
        let area = ui.available_rect_before_wrap();
        ui.painter().rect_filled(area, 0.0, theme.panel);

        // The list itself is a node, so a screen reader announces "list, 30 items"
        // rather than reading thirty unrelated buttons.
        let list_id = ui.id().with("session-list");
        ui.ctx().accesskit_node_builder(list_id, |node| {
            node.set_role(egui::accesskit::Role::List);
            node.set_label(format!("Sessions, {} of them", self.sessions.len()));
        });

        if self.sessions.is_empty() {
            ui.painter().text(
                area.center_top() + Vec2::new(0.0, 40.0),
                Align2::CENTER_TOP,
                "no sessions",
                theme.ui_font.clone(),
                theme.text_faint,
            );
            return actions;
        }

        egui::ScrollArea::vertical()
            .id_salt("session-rows")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add_space(6.0);
                for row in &self.sessions {
                    let selected = self.selected.as_ref() == Some(&row.id);
                    if session_row(ui, theme, row, selected).clicked() {
                        actions.push(ViewAction::SelectSession(row.id.clone()));
                    }
                }
            });
        actions
    }

    /// The panes of the selected session, with their dividers.
    fn pane_area(
        &self,
        ui: &mut Ui,
        theme: &Theme,
        keymap: &Keymap,
        state: &mut ViewState,
    ) -> Vec<ViewAction> {
        let mut actions = Vec::new();
        let area = ui.available_rect_before_wrap();

        let Some(layout) = &self.layout else {
            // The chord, spelled out. "Press the palette shortcut" is useless to somebody
            // who does not know it, and the keymap is right here — including a chord the
            // user chose themselves.
            let hint = match (
                self.sessions.is_empty(),
                keymap.chord_for(Command::QuickNewSession),
            ) {
                (true, Some(chord)) => format!(
                    "no session open — press {} to start one",
                    chord.describe(keymap.platform())
                ),
                (true, None) => "no session open".to_string(),
                (false, _) => "select a session".to_string(),
            };
            ui.painter().text(
                area.center(),
                Align2::CENTER_CENTER,
                hint,
                theme.ui_font.clone(),
                theme.text_faint,
            );
            return actions;
        };

        let arrangement = panes::arrange(layout, area);
        for placed in &arrangement.panes {
            let header =
                Rect::from_min_size(placed.rect.min, Vec2::new(placed.rect.width(), PANE_HEADER));
            let body = Rect::from_min_max(header.left_bottom(), placed.rect.max);

            let content = self
                .panes
                .iter()
                .find(|content| content.pane_id == placed.pane_id);
            let focused = content.is_some_and(|content| content.focused);

            ui.painter().rect_filled(
                header,
                0.0,
                if focused { theme.raised } else { theme.panel },
            );
            let title = content
                .map(|content| content.title.clone())
                .or_else(|| placed.title.clone())
                .unwrap_or_else(|| format!("{:?}", placed.kind).to_lowercase());
            ui.painter().text(
                header.min + Vec2::new(8.0, 4.0),
                Align2::LEFT_TOP,
                title,
                FontId::new(11.0, egui::FontFamily::Monospace),
                if focused { theme.text } else { theme.text_dim },
            );
            if arrangement.zoomed {
                ui.painter().text(
                    header.right_top() + Vec2::new(-8.0, 4.0),
                    Align2::RIGHT_TOP,
                    "zoomed",
                    FontId::new(11.0, egui::FontFamily::Monospace),
                    theme.attention,
                );
            }
            ui.painter().hline(
                header.x_range(),
                header.max.y,
                Stroke::new(1.0, theme.border),
            );

            match content {
                Some(content) => {
                    let options = PaneOptions {
                        focused,
                        now_ms: self.now_ms,
                        scrolled: content.scrolled,
                        history_complete: content.history_complete,
                    };
                    let id = ui.id().with(("pane", placed.pane_id.as_str()));
                    let interaction = state.pane(&placed.pane_id);
                    for action in
                        terminal::show(ui, theme, body, content.grid, interaction, options, id)
                    {
                        actions.push(ViewAction::Pane {
                            pane_id: placed.pane_id.clone(),
                            action,
                        });
                    }
                }
                None => {
                    // A pane the window has not attached to yet, or one with no process.
                    // Said plainly rather than left blank, because a blank pane looks
                    // like a bug.
                    ui.painter().rect_filled(body, 0.0, theme.background);
                    ui.painter().text(
                        body.center(),
                        Align2::CENTER_CENTER,
                        "no process in this pane",
                        theme.ui_font.clone(),
                        theme.text_faint,
                    );
                }
            }
        }

        for divider in &arrangement.dividers {
            actions.extend(draggable_divider(ui, theme, divider));
        }
        actions
    }

    /// The session overview: one thumbnail per session, on a slow cadence.
    fn overview_grid(&self, ui: &mut Ui, theme: &Theme, state: &mut ViewState) -> Vec<ViewAction> {
        let mut actions = Vec::new();
        let area = ui.available_rect_before_wrap();
        ui.painter().rect_filled(area, 0.0, theme.background);

        // Refresh only what is on screen, and only when the cadence allows: `refresh`
        // answers false for a thumbnail taken less than its interval ago, so a draw
        // function calling this sixty times a second rebuilds nothing fifty-nine of them.
        for (session_id, grid) in &self.overview_screens {
            state.thumbnails.refresh(session_id, grid, self.now_ms);
        }

        let columns = ((area.width() / 260.0).floor() as usize).max(1);
        let cell_size = Vec2::new((area.width() - 12.0) / columns as f32 - 12.0, 140.0);
        for (index, row) in self.sessions.iter().enumerate() {
            let column = index % columns;
            let line = index / columns;
            let at = area.min
                + Vec2::new(
                    12.0 + column as f32 * (cell_size.x + 12.0),
                    12.0 + line as f32 * (cell_size.y + 12.0),
                );
            let tile = Rect::from_min_size(at, cell_size);
            if tile.min.y > area.max.y {
                break;
            }
            actions.extend(overview_tile(
                ui,
                theme,
                row,
                state.thumbnails.get(&row.id),
                tile,
                self.selected.as_ref() == Some(&row.id),
            ));
        }
        actions
    }

    /// The attention queue, in the daemon's order, with the next item unmistakable.
    fn queue_panel(&self, ui: &mut Ui, theme: &Theme) -> Vec<ViewAction> {
        let mut actions = Vec::new();
        let area = ui.available_rect_before_wrap();
        ui.painter().rect_filled(area, 0.0, theme.panel);

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(RichText::new("NEEDS YOU").color(theme.attention).small());
            ui.label(
                RichText::new(self.queue.len().to_string())
                    .color(theme.text_dim)
                    .small(),
            );
        });
        ui.add_space(4.0);

        for (index, item) in self.queue.iter().enumerate() {
            // The first actionable item is the one a shortcut would jump to, and it is
            // the one marked NEXT — not simply the first row, which may be snoozed.
            let is_next = index
                == self
                    .queue
                    .iter()
                    .position(|candidate| candidate.actionable)
                    .unwrap_or(usize::MAX);
            if queue_row(ui, theme, item, is_next).clicked() {
                actions.push(ViewAction::GotoAttention(item.attention_id.clone()));
            }
        }
        actions
    }

    fn palette_overlay(
        &self,
        ui: &mut Ui,
        theme: &Theme,
        keymap: &Keymap,
        state: &mut ViewState,
        full: Rect,
    ) -> Vec<ViewAction> {
        let mut actions = Vec::new();
        let width = 560.0_f32.min(full.width() - 40.0);
        let panel = Rect::from_min_size(
            egui::pos2(full.center().x - width / 2.0, full.min.y + 80.0),
            Vec2::new(width, 420.0_f32.min(full.height() - 120.0)),
        );
        // Dimmed, not blocked: the sessions behind stay readable, because the reason to
        // open the palette is often to check what is happening elsewhere.
        ui.painter()
            .rect_filled(full, 0.0, Color32::from_black_alpha(140));
        ui.painter().rect_filled(panel, 0.0, theme.panel);
        ui.painter().rect_stroke(
            panel,
            0.0,
            Stroke::new(1.0, theme.border),
            egui::StrokeKind::Outside,
        );

        let rows = palette::rows(&state.palette.query, keymap);
        let chosen = state.palette.selected.min(rows.len().saturating_sub(1));

        ui.scope_builder(region(panel.shrink(10.0), "palette"), |ui| {
            let field = ui.add(
                egui::TextEdit::singleline(&mut state.palette.query)
                    .hint_text("Type a command")
                    .desired_width(f32::INFINITY),
            );
            field.request_focus();
            ui.add_space(6.0);
            egui::ScrollArea::vertical()
                .id_salt("palette-rows")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    if rows.is_empty() {
                        ui.label(
                            RichText::new("no command matches")
                                .color(theme.text_faint)
                                .small(),
                        );
                    }
                    for (index, row) in rows.iter().enumerate() {
                        if palette_row(ui, theme, row, index == chosen).clicked() {
                            actions.push(ViewAction::Run(row.command));
                        }
                    }
                });
        });
        actions
    }

    fn shortcuts_sheet(
        &self,
        ui: &mut Ui,
        theme: &Theme,
        keymap: &Keymap,
        full: Rect,
    ) -> Vec<ViewAction> {
        let mut actions = Vec::new();
        let panel = full.shrink2(Vec2::new(full.width() * 0.15, full.height() * 0.1));
        ui.painter()
            .rect_filled(full, 0.0, Color32::from_black_alpha(150));
        ui.painter().rect_filled(panel, 0.0, theme.panel);

        ui.scope_builder(region(panel.shrink(14.0), "shortcuts"), |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("KEYBOARD").color(theme.text).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Close").clicked() {
                        actions.push(ViewAction::CloseOverlay);
                    }
                });
            });
            // A binding a running program will never see is worth saying out loud,
            // because the user chose it and the consequence is invisible otherwise.
            let shadowing = keymap.shadowing_the_terminal();
            if !shadowing.is_empty() {
                ui.label(
                    RichText::new(format!(
                        "{} of your bindings take a key that programs in the terminal need",
                        shadowing.len()
                    ))
                    .color(theme.attention)
                    .small(),
                );
            }
            egui::ScrollArea::vertical()
                .id_salt("shortcut-rows")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for bound in keymap.bindings() {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(bound.chord.describe(keymap.platform()))
                                    .monospace()
                                    .color(theme.text),
                            );
                            ui.label(
                                RichText::new(bound.command.title())
                                    .color(theme.text_dim)
                                    .small(),
                            );
                            if bound.chord.shadows_control_character(keymap.platform()) {
                                ui.label(
                                    RichText::new("hidden from the terminal")
                                        .color(theme.attention)
                                        .small(),
                                );
                            }
                        });
                    }
                });
        });
        actions
    }

    fn settings_sheet(&self, ui: &mut Ui, theme: &Theme, full: Rect) -> Vec<ViewAction> {
        let mut actions = Vec::new();
        let panel = full.shrink2(Vec2::new(full.width() * 0.2, full.height() * 0.15));
        ui.painter()
            .rect_filled(full, 0.0, Color32::from_black_alpha(150));
        ui.painter().rect_filled(panel, 0.0, theme.panel);

        ui.scope_builder(region(panel.shrink(14.0), "settings"), |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("SETTINGS").color(theme.text).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Close").clicked() {
                        actions.push(ViewAction::CloseOverlay);
                    }
                });
            });
            match &self.policy {
                Some(policy) => {
                    // Shown, not edited here: the policy belongs to the session and the
                    // daemon owns it. Displaying it read-only is honest; a control that
                    // pretended to change it would not be.
                    ui.label(
                        RichText::new("Attention policy for this session")
                            .color(theme.text_dim)
                            .small(),
                    );
                    ui.label(
                        RichText::new(format!(
                            "never interrupt while typing: {}",
                            policy.do_not_interrupt_while_typing
                        ))
                        .monospace()
                        .color(theme.text),
                    );
                    ui.label(
                        RichText::new(format!(
                            "focus only when idle: {}",
                            policy.focus_only_if_idle
                        ))
                        .monospace()
                        .color(theme.text),
                    );
                }
                None => {
                    ui.label(
                        RichText::new("select a session to see its attention policy")
                            .color(theme.text_faint)
                            .small(),
                    );
                }
            }
        });
        actions
    }
}

/// One session row, as a real widget with a place in the accessibility tree.
///
/// This is what makes the window usable with a screen reader. `allocate_exact_size`
/// gives a sensed rectangle with an id, and the AccessKit node hung off that id carries
/// the row's name, its state in words and whether it is the selected one — the three
/// things the painted row expresses through colour, a glyph and a highlight.
fn session_row(ui: &mut Ui, theme: &Theme, row: &SessionRow, selected: bool) -> Response {
    let height = if row.detail.is_empty() {
        28.0
    } else {
        ROW_HEIGHT
    };
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::click());

    if selected {
        ui.painter().rect_filled(rect, 0.0, theme.selection);
    } else if response.hovered() {
        ui.painter().rect_filled(rect, 0.0, theme.raised);
    }

    let (colour, glyph) = theme.state_marker(row.state);
    let indent = 10.0 + row.depth as f32 * 14.0;
    let painter = ui.painter();

    painter.text(
        rect.min + Vec2::new(indent, 5.0),
        Align2::LEFT_TOP,
        glyph,
        FontId::new(12.0, egui::FontFamily::Monospace),
        colour,
    );
    painter.text(
        rect.min + Vec2::new(indent + 16.0, 4.0),
        Align2::LEFT_TOP,
        &row.name,
        theme.ui_font.clone(),
        theme.text,
    );

    // The state's word, always, next to the glyph and the colour.
    let label = if row.provisional {
        format!("{} (inferred)", row.state_label)
    } else {
        row.state_label.clone()
    };
    let label_colour = if row.provisional {
        theme.provisional
    } else {
        colour
    };
    // Laid out from the measured width of the label rather than a fixed offset:
    // `PERMISSION` and `running (inferred)` are very different widths, and a guessed
    // column makes the longer one collide with the detail text.
    let label_rect = painter.text(
        rect.min + Vec2::new(indent + 16.0, 21.0),
        Align2::LEFT_TOP,
        label,
        FontId::new(11.0, egui::FontFamily::Monospace),
        label_colour,
    );
    if !row.detail.is_empty() {
        let detail_x = label_rect.max.x + 10.0;
        // The mute marker sits at the bottom right, on the same line as the detail, so
        // the room it needs is taken out of the detail's width rather than left to
        // overlap it.
        let reserved = if row.muted { 48.0 } else { 12.0 };
        let available = rect.max.x - detail_x - reserved;
        if available > 30.0 {
            // Clipped rather than allowed to run under the badge.
            painter
                .with_clip_rect(Rect::from_min_max(
                    egui::pos2(detail_x, rect.min.y),
                    egui::pos2(detail_x + available, rect.max.y),
                ))
                .text(
                    egui::pos2(detail_x, rect.min.y + 21.0),
                    Align2::LEFT_TOP,
                    &row.detail,
                    FontId::new(11.0, egui::FontFamily::Proportional),
                    theme.text_faint,
                );
        }
    }
    if row.badge > 0 {
        painter.text(
            rect.right_top() + Vec2::new(-12.0, 6.0),
            Align2::RIGHT_TOP,
            row.badge.to_string(),
            FontId::new(11.0, egui::FontFamily::Monospace),
            theme.attention,
        );
    }
    if row.muted {
        // A muted session still badges; the mute is said as well, so the two facts are
        // never confused.
        painter.text(
            rect.right_bottom() + Vec2::new(-12.0, -16.0),
            Align2::RIGHT_TOP,
            "muted",
            FontId::new(10.0, egui::FontFamily::Monospace),
            theme.text_faint,
        );
    }

    let name = row.accessible_name();
    describe_row(&response, &name, selected);
    response
}

/// Puts a row in the accessibility tree as a list item.
///
/// `widget_info` goes first and the node is written second, deliberately: `widget_info`
/// fills the node in from a `WidgetType`, which would set the role to `Button` and
/// overwrite what a screen reader needs to hear — that this is one row of a list, and
/// whether it is the selected one. Writing the node afterwards means the explicit role
/// wins on every frame, including the frame the row is clicked on, where `widget_info`
/// takes a different path.
fn describe_row(response: &Response, name: &str, selected: bool) {
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, name));
    response.ctx.accesskit_node_builder(response.id, |node| {
        node.set_role(egui::accesskit::Role::ListItem);
        node.set_label(name.to_string());
        node.set_selected(selected);
        node.add_action(egui::accesskit::Action::Click);
    });
}

/// One row of the attention queue.
fn queue_row(ui: &mut Ui, theme: &Theme, item: &QueueItem, is_next: bool) -> Response {
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 40.0), Sense::click());
    if is_next {
        // The next item is unmistakable: a filled left rule and the word NEXT, not
        // merely being at the top of a list.
        ui.painter().rect_filled(
            Rect::from_min_size(rect.min, Vec2::new(3.0, rect.height())),
            0.0,
            theme.attention,
        );
    }
    if response.hovered() {
        ui.painter().rect_filled(rect, 0.0, theme.raised);
    }
    let dim = !item.actionable;
    let text_colour = if dim { theme.text_faint } else { theme.text };
    let painter = ui.painter();
    let mut x = 10.0;
    if is_next {
        let next = painter.text(
            rect.min + Vec2::new(x, 4.0),
            Align2::LEFT_TOP,
            "NEXT",
            FontId::new(10.0, egui::FontFamily::Monospace),
            theme.attention,
        );
        x = next.max.x - rect.min.x + 6.0;
    }
    painter.text(
        rect.min + Vec2::new(x, 3.0),
        Align2::LEFT_TOP,
        &item.session_name,
        theme.ui_font.clone(),
        text_colour,
    );
    let mut second = format!("{} ", item.reason_label());
    if item.provisional {
        second.push_str("(inferred) ");
    }
    if !item.actionable {
        second.push_str("· snoozed ");
    }
    if let Some(summary) = &item.summary {
        second.push_str(summary);
    }
    painter.with_clip_rect(rect).text(
        rect.min + Vec2::new(10.0, 21.0),
        Align2::LEFT_TOP,
        second.trim(),
        FontId::new(11.0, egui::FontFamily::Proportional),
        if item.provisional {
            theme.provisional
        } else {
            theme.text_dim
        },
    );

    let name = format!(
        "{}{} — {}{}",
        if is_next { "next: " } else { "" },
        item.session_name,
        item.reason_label(),
        if item.actionable { "" } else { ", snoozed" }
    );
    describe_row(&response, &name, is_next);
    response
}

/// One row of the command palette.
fn palette_row(ui: &mut Ui, theme: &Theme, row: &palette::Row, selected: bool) -> Response {
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 26.0), Sense::click());
    if selected {
        ui.painter().rect_filled(rect, 0.0, theme.selection);
    }
    let painter = ui.painter();
    painter.text(
        rect.left_center() + Vec2::new(8.0, 0.0),
        Align2::LEFT_CENTER,
        row.title,
        theme.ui_font.clone(),
        theme.text,
    );
    painter.text(
        rect.right_center() + Vec2::new(-8.0, 0.0),
        Align2::RIGHT_CENTER,
        row.shortcut.clone().unwrap_or_default(),
        FontId::new(11.0, egui::FontFamily::Monospace),
        theme.text_faint,
    );
    painter.text(
        rect.right_center() + Vec2::new(-100.0, 0.0),
        Align2::RIGHT_CENTER,
        row.group,
        FontId::new(10.0, egui::FontFamily::Monospace),
        theme.text_faint,
    );

    let name = match &row.shortcut {
        Some(shortcut) => format!("{} — {} — {}", row.title, row.group, shortcut),
        None => format!("{} — {} — no shortcut", row.title, row.group),
    };
    describe_row(&response, &name, selected);
    response
}

/// One tile of the session overview.
fn overview_tile(
    ui: &mut Ui,
    theme: &Theme,
    row: &SessionRow,
    thumbnail: Option<&Thumbnail>,
    rect: Rect,
    selected: bool,
) -> Vec<ViewAction> {
    let mut actions = Vec::new();
    let response = ui.interact(
        rect,
        ui.id().with(("overview", row.id.as_str())),
        Sense::click(),
    );
    ui.painter().rect_filled(rect, 0.0, theme.panel);
    ui.painter().rect_stroke(
        rect,
        0.0,
        Stroke::new(
            if selected { 2.0 } else { 1.0 },
            if selected {
                theme.running
            } else {
                theme.border
            },
        ),
        egui::StrokeKind::Inside,
    );

    let (colour, glyph) = theme.state_marker(row.state);
    let header = Rect::from_min_size(rect.min, Vec2::new(rect.width(), 20.0));
    ui.painter().text(
        header.min + Vec2::new(6.0, 3.0),
        Align2::LEFT_TOP,
        format!("{glyph} {}", row.name),
        FontId::new(11.0, egui::FontFamily::Monospace),
        colour,
    );
    if row.badge > 0 {
        ui.painter().text(
            header.right_top() + Vec2::new(-6.0, 3.0),
            Align2::RIGHT_TOP,
            row.badge.to_string(),
            FontId::new(11.0, egui::FontFamily::Monospace),
            theme.attention,
        );
    }

    let picture = Rect::from_min_max(header.left_bottom(), rect.max).shrink(6.0);
    match thumbnail {
        Some(thumbnail) if !thumbnail.is_blank() => {
            let block = Vec2::new(
                picture.width() / thumbnail.cols as f32,
                picture.height() / thumbnail.rows as f32,
            );
            for line in 0..thumbnail.rows {
                for column in 0..thumbnail.cols {
                    let Some(cell) = thumbnail.block(line, column) else {
                        continue;
                    };
                    if cell.ink <= 0.0 {
                        continue;
                    }
                    let colour = cell
                        .colour
                        .map(|rgb| Color32::from_rgb(rgb.0, rgb.1, rgb.2))
                        .unwrap_or(theme.text_dim);
                    ui.painter().rect_filled(
                        Rect::from_min_size(
                            picture.min + Vec2::new(column as f32 * block.x, line as f32 * block.y),
                            block,
                        ),
                        0.0,
                        colour.gamma_multiply(cell.ink.clamp(0.15, 1.0)),
                    );
                }
            }
            if thumbnail.alternate_screen {
                ui.painter().text(
                    picture.right_bottom() + Vec2::new(-4.0, -12.0),
                    Align2::RIGHT_TOP,
                    "full screen",
                    FontId::new(9.0, egui::FontFamily::Monospace),
                    theme.text_faint,
                );
            }
        }
        _ => {
            ui.painter().text(
                picture.center(),
                Align2::CENTER_CENTER,
                "nothing on screen",
                FontId::new(10.0, egui::FontFamily::Proportional),
                theme.text_faint,
            );
        }
    }

    describe_row(&response, &row.accessible_name(), selected);
    if response.clicked() {
        actions.push(ViewAction::SelectSession(row.id.clone()));
    }
    actions
}

/// A divider the user can drag.
///
/// The drag is turned into a fraction of the parent split, which is what `resize_pane`
/// takes, and it is sent as it happens rather than on release: a divider that only moved
/// when let go would feel broken.
fn draggable_divider(ui: &mut Ui, theme: &Theme, divider: &Divider) -> Vec<ViewAction> {
    let mut actions = Vec::new();
    let id = ui
        .id()
        .with(("divider", divider.before.as_str(), divider.after.as_str()));
    let response = ui.interact(divider.grab_rect(), id, Sense::drag());
    let hovered = response.hovered() || response.dragged();
    ui.painter().rect_filled(
        divider.rect,
        0.0,
        if hovered { theme.running } else { theme.border },
    );
    if hovered {
        ui.ctx().set_cursor_icon(match divider.direction {
            turn_core::model::Direction::Horizontal => egui::CursorIcon::ResizeHorizontal,
            turn_core::model::Direction::Vertical => egui::CursorIcon::ResizeVertical,
        });
    }
    if response.dragged() {
        if let Some(fraction) = divider.fraction_for_drag(response.drag_delta()) {
            actions.push(ViewAction::ResizeDivider {
                pane_id: divider.before.clone(),
                fraction,
            });
        }
    }
    actions
}

/// Which way an arrow key moves between panes.
pub fn side_for(command: Command) -> Option<Side> {
    match command {
        Command::FocusPaneLeft => Some(Side::Left),
        Command::FocusPaneRight => Some(Side::Right),
        Command::FocusPaneUp => Some(Side::Up),
        Command::FocusPaneDown => Some(Side::Down),
        _ => None,
    }
}

/// The pane a directional command would move to, given what is on screen.
pub fn neighbour_for(arrangement: &Arrangement, from: &PaneId, command: Command) -> Option<PaneId> {
    panes::neighbour(arrangement, from, side_for(command)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, state: DisplayState) -> SessionRow {
        SessionRow {
            id: SessionId::from_stored(format!("sess_{name:0>11}")),
            name: name.into(),
            state,
            state_label: state.label().to_string(),
            detail: String::new(),
            badge: 0,
            provisional: false,
            depth: 0,
            muted: false,
        }
    }

    /// The accessible name has to carry everything the visuals do, because that is all
    /// a screen-reader user gets.
    #[test]
    fn an_accessible_name_says_everything_the_row_shows() {
        let mut needy = row("Fix climbing bugs", DisplayState::NeedsPermission);
        needy.detail = "1 running · 3 panes".into();
        needy.badge = 2;
        let name = needy.accessible_name();
        assert!(name.contains("Fix climbing bugs"));
        assert!(name.contains("PERMISSION"), "the state in words: {name}");
        assert!(name.contains("1 running"));
        assert!(name.contains("2 waiting"), "the badge is a number: {name}");
        assert!(!name.contains("muted"));
    }

    /// A guess must be audible as a guess, not only visible as a different colour.
    #[test]
    fn an_inferred_state_says_so_in_words() {
        let mut guessed = row("npm run dev", DisplayState::Running);
        guessed.provisional = true;
        assert!(
            guessed.accessible_name().contains("(inferred)"),
            "got {}",
            guessed.accessible_name()
        );
    }

    /// Muting silences the interruption, not the evidence — and the accessible name has
    /// to make both facts available.
    #[test]
    fn a_muted_session_still_reports_its_badge_and_its_mute() {
        let mut muted = row("Draft release notes", DisplayState::CompletedTurn);
        muted.muted = true;
        muted.badge = 3;
        let name = muted.accessible_name();
        assert!(name.contains("3 waiting"));
        assert!(name.contains("muted"));
    }

    #[test]
    fn every_reason_a_session_can_want_you_has_a_word() {
        for reason in [
            AwaitingReason::Permission,
            AwaitingReason::Question,
            AwaitingReason::Credentials,
            AwaitingReason::Input,
        ] {
            let item = QueueItem {
                attention_id: AttentionId::new(),
                session_id: SessionId::from_stored("sess_queue000001"),
                session_name: "Fix it".into(),
                reason,
                summary: None,
                provisional: false,
                actionable: true,
            };
            assert!(!item.reason_label().is_empty(), "{reason:?} has no word");
        }
    }

    #[test]
    fn the_directional_commands_map_to_sides_and_nothing_else_does() {
        assert_eq!(side_for(Command::FocusPaneLeft), Some(Side::Left));
        assert_eq!(side_for(Command::FocusPaneRight), Some(Side::Right));
        assert_eq!(side_for(Command::FocusPaneUp), Some(Side::Up));
        assert_eq!(side_for(Command::FocusPaneDown), Some(Side::Down));
        assert_eq!(side_for(Command::ZoomPane), None);
        assert_eq!(side_for(Command::CyclePane), None);
    }

    /// An overlay is a sensitive operation: the focus governor must not move somebody
    /// who is halfway through choosing a command or reading a permission.
    #[test]
    fn an_open_overlay_counts_as_something_that_must_not_be_interrupted() {
        let mut state = ViewState::default();
        assert!(!state.is_sensitive());
        state.palette.open();
        assert!(state.is_sensitive());
        state.palette.close();
        state.shortcuts_open = true;
        assert!(state.is_sensitive());
        state.shortcuts_open = false;
        state.settings_open = true;
        assert!(state.is_sensitive());
    }

    #[test]
    fn a_pane_gets_its_own_interaction_state_the_first_time_it_is_seen() {
        let mut state = ViewState::default();
        let first = PaneId::new();
        let second = PaneId::new();
        state.pane(&first).selection = Some(crate::terminal::selection::Selection::new(
            crate::terminal::selection::CellPos::new(0, 0),
            crate::terminal::selection::SelectionKind::Linear,
        ));
        assert!(state.pane(&first).selection.is_some());
        assert!(
            state.pane(&second).selection.is_none(),
            "two panes must hold separate selections"
        );
    }
}
