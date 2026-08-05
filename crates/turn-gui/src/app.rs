//! The window itself: one frame, in order.
//!
//! Everything with a decision in it lives elsewhere — [`crate::desk`] for state,
//! [`crate::keymap`] for what a keystroke means, [`crate::repaint`] for when to draw
//! again — so this module is the wiring, and it is short on purpose. A frame is:
//!
//! 1. Drain whatever arrived from the daemon and apply it.
//! 2. Notice what the user did to the *window* — focus, keystrokes — and report the
//!    activity the focus governor needs.
//! 3. Resolve keystrokes against the keymap, **consuming** the ones that are bindings so
//!    the terminal never sees a key the window took.
//! 4. Draw, and apply what the drawing reports back.
//! 5. Work out when to draw next, which for an idle desk is "never, until something
//!    happens".

use std::sync::Arc;

use eframe::egui;
use egui::Event;

use crate::activity::ActivityTracker;
use crate::announce::{perform, Announcement, Announcer, DesktopAnnouncer};
use crate::desk::{Desk, Reaction};
use crate::keymap::{Command, Keymap};
use crate::repaint::{next_cursor_phase, next_elapsed_tick, Deadlines};
use crate::theme::Theme;
use crate::transport::{Ask, DaemonLink};
use crate::view::{ViewAction, ViewState};

/// The application.
pub struct TurnApp {
    theme: Theme,
    keymap: Keymap,
    link: DaemonLink,
    desk: Desk,
    state: ViewState,
    activity: ActivityTracker,
    announcer: Box<dyn Announcer>,
}

impl TurnApp {
    /// Builds the application and starts its connection.
    pub fn new(ctx: &egui::Context, socket: std::path::PathBuf, keymap: Keymap) -> Self {
        let theme = Theme::dark();
        theme.install(ctx);

        // The transport wakes the window when a frame arrives. This is the whole
        // mechanism that lets an idle window sleep: nothing polls.
        let waker = ctx.clone();
        let link = DaemonLink::spawn(
            socket,
            env!("CARGO_PKG_VERSION"),
            Arc::new(move || waker.request_repaint()),
        );

        TurnApp {
            theme,
            keymap,
            link,
            desk: Desk::new(),
            state: ViewState::default(),
            activity: ActivityTracker::new(),
            announcer: Box::new(DesktopAnnouncer),
        }
    }

    /// Replaces the notifier, for a test that wants to see what would have been posted.
    pub fn with_announcer(mut self, announcer: Box<dyn Announcer>) -> Self {
        self.announcer = announcer;
        self
    }

    pub fn desk(&self) -> &Desk {
        &self.desk
    }

    /// Carries out a reaction.
    fn perform(&mut self, ctx: &egui::Context, reaction: Reaction) {
        match reaction {
            Reaction::Send { ask, request } => self.link.send(ask, request),
            Reaction::Announce(announcement) => {
                if let Announcement::Focus { .. } = &announcement {
                    // The daemon's governor cleared this one. Bringing the window
                    // forward is the visible half of that decision.
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                perform(&announcement, self.announcer.as_ref());
            }
            Reaction::Copy(text) => ctx.copy_text(text),
            Reaction::Notice(message) => tracing::info!(%message, "notice"),
        }
    }

    /// The commands the frame's keystrokes resolve to, removing them from the input.
    ///
    /// Consuming is the point. A binding the window handles must not also reach the
    /// process in the pane, and a key that is *not* a binding must reach it untouched —
    /// which together are the whole of "do not steal keys the terminal needs".
    fn take_commands(&mut self, ctx: &egui::Context) -> Vec<Command> {
        let keymap = &self.keymap;
        // While a sheet is open the window owns the keyboard, so a binding may take a key
        // a terminal would otherwise need: there is no terminal listening.
        let in_terminal = !self.state.is_sensitive();
        let mut commands = Vec::new();
        ctx.input_mut(|input| {
            input.events.retain(|event| match event {
                Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => match keymap.resolve(*key, modifiers, in_terminal) {
                    // Cmd+Enter is contextual in the unified tree. Leave the event for
                    // the tree widget; everywhere else the existing Next Attention
                    // binding remains global and is consumed here.
                    Some(Command::NextAttention) if self.state.tree_has_focus => true,
                    Some(command) => {
                        commands.push(command);
                        false
                    }
                    None => true,
                },
                _ => true,
            });
        });
        commands
    }

    /// Reports what the user is doing to the window, for the focus governor.
    fn observe_activity(&mut self, ctx: &egui::Context, now_ms: i64) {
        let (typed, focus_change) = ctx.input(|input| {
            let typed = input.events.iter().any(|event| {
                matches!(
                    event,
                    Event::Key { pressed: true, .. } | Event::Text(_) | Event::Paste(_)
                )
            });
            let focus = input.events.iter().rev().find_map(|event| match event {
                Event::WindowFocused(focused) => Some(*focused),
                _ => None,
            });
            (typed, focus)
        });
        if typed {
            self.activity.keystroke(now_ms);
        }
        if let Some(focused) = focus_change {
            self.activity.window_focus(focused);
        }
        self.activity.active_session(self.desk.selected().cloned());
        // A sheet open, or a permission being read, is something that must not be
        // interrupted. The governor uses it to refuse a focus jump outright.
        self.activity.sensitive_operation(self.state.is_sensitive());
    }

    /// Handles the keys a sheet owns while it is open.
    ///
    /// Returns true when the key was for the sheet, so it is not also treated as input.
    fn steer_overlays(&mut self, ctx: &egui::Context) -> Vec<Command> {
        if !self.state.palette.open {
            if self.state.shortcuts_open
                || self.state.settings_open
                || self.state.attention_panel_open
            {
                let escape = ctx
                    .input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
                if escape {
                    self.state.shortcuts_open = false;
                    self.state.settings_open = false;
                    self.state.attention_panel_open = false;
                }
            }
            return Vec::new();
        }

        let mut chosen = Vec::new();
        let count = self.state.palette.matches().len();
        ctx.input_mut(|input| {
            if input.consume_key(egui::Modifiers::NONE, egui::Key::Escape) {
                self.state.palette.close();
                return;
            }
            if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                self.state.palette.move_selection(1, count);
            }
            if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                self.state.palette.move_selection(-1, count);
            }
            if input.consume_key(egui::Modifiers::NONE, egui::Key::Enter) {
                if let Some(command) = self.state.palette.chosen() {
                    chosen.push(command);
                }
            }
        });
        if !chosen.is_empty() {
            self.state.palette.close();
        }
        chosen
    }

    /// The commands the window handles itself rather than sending on.
    fn handle_locally(&mut self, command: Command) -> bool {
        match command {
            Command::OpenPalette => {
                self.state.palette.open();
                true
            }
            Command::SwitchSession => {
                self.state.palette.open();
                self.state.palette.set_query("session");
                true
            }
            Command::ShowKeyboardShortcuts => {
                self.state.shortcuts_open = !self.state.shortcuts_open;
                true
            }
            Command::OpenSettings => {
                self.state.settings_open = !self.state.settings_open;
                true
            }
            Command::ToggleAttentionPanel => {
                self.state.attention_panel_open = !self.state.attention_panel_open;
                true
            }
            Command::FocusWorkspaceTree => {
                self.state.tree_has_focus = true;
                true
            }
            _ => false,
        }
    }
}

impl eframe::App for TurnApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let now_ms = turn_core::now_ms();

        // 1. Whatever arrived from the daemon.
        for message in self.link.drain() {
            for reaction in self.desk.apply_inbound(message, now_ms) {
                self.perform(&ctx, reaction);
            }
        }
        // Clone only when the daemon-owned projection actually changed. Comparing the
        // small revisioned value each frame avoids cloning hundreds of rows while still
        // carrying tree-state responses that intentionally keep the same revision.
        if self.state.hierarchy.as_ref() != self.desk.hierarchy() {
            self.state.hierarchy = self.desk.hierarchy().cloned();
        }
        if &self.state.preview_history != self.desk.preview_histories() {
            self.state.preview_history = self.desk.preview_histories().clone();
        }
        self.state.write_conflict_open = self.desk.write_conflict().is_some();

        // 2. What the user is doing to the window.
        self.observe_activity(&ctx, now_ms);

        // 3. Keystrokes: the sheets first, then the keymap.
        let mut commands = self.steer_overlays(&ctx);
        commands.extend(self.take_commands(&ctx));
        for command in commands {
            if self.handle_locally(command) {
                continue;
            }
            for reaction in self.desk.dispatch(command, now_ms) {
                self.perform(&ctx, reaction);
            }
        }

        // 4. Draw, and apply what the drawing reports.
        self.desk.refresh_screens();
        let actions = {
            let view = self.desk.view(now_ms);
            view.ui(ui, &self.theme, &self.keymap, &mut self.state)
        };
        let arrangement = self.desk.arrange(ui.available_rect_before_wrap());
        self.desk.remember_arrangement(arrangement);
        for action in actions {
            match action {
                ViewAction::CloseOverlay => {
                    self.state.shortcuts_open = false;
                    self.state.settings_open = false;
                    self.state.attention_panel_open = false;
                    self.state.workspace_draft = None;
                }
                action @ ViewAction::CreateWorkspace { .. } => {
                    self.state.workspace_draft = None;
                    for reaction in self.desk.apply_view_action(action, now_ms) {
                        self.perform(&ctx, reaction);
                    }
                }
                ViewAction::Run(command) if self.handle_locally(command) => {}
                other => {
                    for reaction in self.desk.apply_view_action(other, now_ms) {
                        self.perform(&ctx, reaction);
                    }
                }
            }
        }
        for action in self.state.take_hierarchy_actions() {
            for reaction in self.desk.apply_hierarchy_action(action) {
                self.perform(&ctx, reaction);
            }
        }

        // 5. The activity report, on a change and not on a timer.
        if let Some(context) = self.activity.take_update(now_ms) {
            self.link.send(
                Ask::Activity,
                turn_proto::Request::UpdateUserActivity { context },
            );
        }

        // 6. When to draw next. An idle hierarchy sleeps until input or a daemon push;
        // clocks such as a focused cursor request one delayed frame at their deadline.
        self.repaint_plan(now_ms).apply(&ctx);
    }
}

impl TurnApp {
    /// When the window will next draw, if nothing happens before then.
    ///
    /// Public because it is the product's most explicit performance criterion, and a
    /// criterion nobody can read is not one: `tests/snapshots.rs` asserts on this for a
    /// real application, and [`RepaintPlan::is_idle`] answering true is what "an idle desk
    /// of thirty sessions costs nothing" means in code.
    pub fn repaint_plan(&self, now_ms: i64) -> crate::repaint::RepaintPlan {
        self.deadlines(now_ms).plan(now_ms)
    }

    /// What is on screen that changes by the clock.
    ///
    /// Kept in one place so adding a reason to repaint means naming it here, where the
    /// cost is visible, rather than dropping a `request_repaint` into a draw function.
    fn deadlines(&self, now_ms: i64) -> Deadlines {
        let focused_cursor = self
            .desk
            .active_pane()
            .is_some_and(|_| !self.state.is_sensitive());
        Deadlines {
            cursor_blink_at: focused_cursor.then(|| next_cursor_phase(now_ms)),
            // Only when something with an elapsed time is actually on screen.
            elapsed_tick_at: (!self.desk.queue().is_empty()).then(|| next_elapsed_tick(now_ms)),
            typing_expires_at: self.activity.wake_at(now_ms),
            reconnect_at: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::{Overrides, Platform};

    /// A window with nothing open asks for no frames at all: no pane, so no cursor to
    /// blink; no queue, so no elapsed time to count; nothing typed, so nothing for the
    /// focus governor to wait for.
    #[test]
    fn a_window_with_nothing_open_asks_for_no_frames() {
        let ctx = egui::Context::default();
        let app = TurnApp::new(
            &ctx,
            std::path::PathBuf::from("/tmp/turn-no-such-daemon-for-tests.sock"),
            Keymap::build(&Overrides::new(), Platform::MAC),
        );
        let now = 1_700_000_000_000;
        assert!(
            app.repaint_plan(now).is_idle(),
            "got {:?}",
            app.repaint_plan(now)
        );
    }

    /// And when something *does* change by the clock, the window asks for one frame at
    /// that moment rather than for a continuous repaint.
    #[test]
    fn a_deadline_produces_one_delayed_frame_rather_than_a_continuous_repaint() {
        let deadlines = Deadlines {
            cursor_blink_at: Some(1_700_000_000_400),
            ..Deadlines::default()
        };
        match deadlines.plan(1_700_000_000_000) {
            crate::repaint::RepaintPlan::After(delay) => {
                assert!(
                    delay > std::time::Duration::ZERO,
                    "a zero delay is a continuous repaint"
                );
            }
            other => panic!("expected a delayed frame, got {other:?}"),
        }
    }

    /// The keymap the window runs with must be the one for the keyboard in front of the
    /// user, and it must not have taken a key a terminal needs.
    #[test]
    fn the_window_runs_with_a_keymap_that_leaves_the_terminal_alone() {
        for platform in [Platform::MAC, Platform::PC] {
            let keymap = Keymap::build(&Overrides::new(), platform);
            assert!(
                keymap.shadowing_the_terminal().is_empty(),
                "the default map must not shadow a control character on {platform:?}"
            );
            assert!(keymap.conflicts().is_empty());
        }
    }
}
