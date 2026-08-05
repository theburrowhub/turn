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

use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc};

use eframe::egui;
use egui::Event;

use crate::activity::ActivityTracker;
use crate::announce::{perform, Announcement, Announcer, DesktopAnnouncer};
use crate::companion::{CompanionEvent, CompanionMonitor};
use crate::desk::{Desk, Reaction};
use crate::keymap::{Command, Keymap};
use crate::repaint::{next_cursor_phase, next_elapsed_tick, Deadlines};
use crate::theme::Theme;
use crate::transport::{Ask, DaemonLink};
use crate::view::{LayoutEditorOrigin, LayoutTemplateDraft, ViewAction, ViewState};

#[derive(Debug)]
struct FolderDialogResult {
    request_id: u64,
    path: Option<PathBuf>,
}

/// Owns the native chooser boundary without making the pure `TurnView` platform-aware.
///
/// `rfd` needs the dialog future to be created on the application thread on macOS. The
/// future itself is then awaited off-thread and wakes egui exactly once on completion,
/// so an open chooser causes neither a blocked renderer nor an idle repaint loop.
struct FolderDialog {
    next_request_id: u64,
    sender: mpsc::Sender<FolderDialogResult>,
    results: mpsc::Receiver<FolderDialogResult>,
}

impl Default for FolderDialog {
    fn default() -> Self {
        let (sender, results) = mpsc::channel();
        Self {
            next_request_id: 1,
            sender,
            results,
        }
    }
}

impl FolderDialog {
    fn open(
        &mut self,
        frame: &eframe::Frame,
        ctx: &egui::Context,
        current_root: &str,
    ) -> Result<u64, String> {
        let request_id = self.next_request_id;
        self.next_request_id = self.next_request_id.wrapping_add(1).max(1);

        let mut dialog = rfd::AsyncFileDialog::new()
            .set_title("Choose project folder")
            .set_parent(frame)
            .set_can_create_directories(true);
        let current_root = Path::new(current_root.trim());
        if current_root.is_dir() {
            dialog = dialog.set_directory(current_root);
        }
        // Constructing the future here is intentional: rfd schedules NSOpenPanel from
        // the application's main thread, while the worker below only awaits completion.
        let future = dialog.pick_folder();
        let sender = self.sender.clone();
        let repaint = ctx.clone();
        std::thread::Builder::new()
            .name(format!("turn-folder-picker-{request_id}"))
            .spawn(move || {
                let path = pollster::block_on(future).map(|handle| handle.path().to_path_buf());
                let _ = sender.send(FolderDialogResult { request_id, path });
                repaint.request_repaint();
            })
            .map_err(|error| format!("Could not open the project folder chooser: {error}"))?;
        Ok(request_id)
    }

    fn drain(&self) -> Vec<FolderDialogResult> {
        self.results.try_iter().collect()
    }
}

/// The application.
pub struct TurnApp {
    theme: Theme,
    keymap: Keymap,
    link: DaemonLink,
    desk: Desk,
    state: ViewState,
    activity: ActivityTracker,
    announcer: Box<dyn Announcer>,
    companion_events: Option<mpsc::Receiver<CompanionEvent>>,
    folder_dialog: FolderDialog,
    pending_folder_request: Option<u64>,
}

impl TurnApp {
    /// Builds the application and starts its connection.
    pub fn new(ctx: &egui::Context, socket: std::path::PathBuf, keymap: Keymap) -> Self {
        Self::new_with_companion(ctx, socket, keymap, None, None)
    }

    /// Builds the application while preserving a companion-launch failure in the
    /// visible notice area. The socket link still retries: an operator may repair the
    /// package or start the daemon without restarting the window.
    pub fn new_with_startup_error(
        ctx: &egui::Context,
        socket: std::path::PathBuf,
        keymap: Keymap,
        startup_error: Option<String>,
    ) -> Self {
        Self::new_with_companion(ctx, socket, keymap, startup_error, None)
    }

    /// Builds the application and retains a detached companion only long enough to
    /// reap it and surface a later startup/runtime failure. The monitor never kills the
    /// process, including when the window is closed.
    pub fn new_with_companion(
        ctx: &egui::Context,
        socket: std::path::PathBuf,
        keymap: Keymap,
        startup_error: Option<String>,
        monitor: Option<CompanionMonitor>,
    ) -> Self {
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

        let mut desk = Desk::new();
        if let Some(error) = startup_error {
            desk.show_companion_notice(error);
        }
        let companion_events = monitor.map(|monitor| {
            let waker = ctx.clone();
            monitor.watch(Arc::new(move || waker.request_repaint()))
        });

        TurnApp {
            theme,
            keymap,
            link,
            desk,
            state: ViewState::default(),
            activity: ActivityTracker::new(),
            announcer: Box::new(DesktopAnnouncer),
            companion_events,
            folder_dialog: FolderDialog::default(),
            pending_folder_request: None,
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
            Reaction::Notice(message) => {
                tracing::info!(%message, "notice");
                self.desk.show_notice(message);
            }
            Reaction::WorkspaceCreated {
                workspace_id,
                continue_to_session,
            } => {
                self.pending_folder_request = None;
                self.state.workspace_picker_pending = false;
                self.state.workspace_draft = None;
                if continue_to_session {
                    self.state.session_draft = Some(self.desk.new_session_draft_for(workspace_id));
                }
            }
            Reaction::SessionCreated { .. } | Reaction::SessionCreationCancelled => {
                self.state.session_draft = None;
            }
            Reaction::WorkspaceCreationFailed(message) => {
                if let Some(draft) = self.state.workspace_draft.as_mut() {
                    draft.submitting = false;
                    draft.error = Some(message);
                }
            }
            Reaction::SessionCreationFailed(message) => {
                if let Some(draft) = self.state.session_draft.as_mut() {
                    draft.submitting = false;
                    draft.error = Some(message);
                }
            }
            Reaction::TemplateCreated { template_id } => {
                let origin = self.state.layout_draft.as_ref().map(|draft| draft.origin);
                self.state.layout_draft = None;
                if origin == Some(LayoutEditorOrigin::NewSession) {
                    if let Some(session) = self.state.session_draft.as_mut() {
                        session.template_id = Some(template_id);
                    }
                }
            }
            Reaction::TemplateCreationFailed(message) => {
                if let Some(draft) = self.state.layout_draft.as_mut() {
                    draft.submitting = false;
                    draft.error = Some(message);
                }
            }
            Reaction::ContextHandoffPrepared(handoff) => {
                if let Some(draft) = self.state.context_handoff.as_mut().filter(|draft| {
                    draft.session_id == handoff.session_id
                        && draft.source_node_id == handoff.source_node_id
                        && draft.target_node_id.as_ref() == Some(&handoff.target_node_id)
                }) {
                    draft.preparing = false;
                    draft.delivering = false;
                    draft.error = None;
                    draft.prepared = Some(handoff);
                }
            }
            Reaction::ContextHandoffDelivered { handoff_id } => {
                if let Some(draft) = self.state.context_handoff.as_mut().filter(|draft| {
                    draft
                        .prepared
                        .as_ref()
                        .is_some_and(|handoff| handoff.handoff_id == handoff_id)
                }) {
                    draft.delivering = false;
                    draft.delivered = true;
                    draft.error = None;
                }
            }
            Reaction::ContextHandoffPrepareFailed {
                session_id,
                source_node_id,
                target_node_id,
                message,
            } => {
                if let Some(draft) = self.state.context_handoff.as_mut().filter(|draft| {
                    draft.session_id == session_id
                        && draft.source_node_id == source_node_id
                        && draft.target_node_id.as_ref() == Some(&target_node_id)
                }) {
                    draft.preparing = false;
                    draft.error = Some(message);
                }
            }
            Reaction::ContextHandoffDeliveryFailed {
                handoff_id,
                message,
            } => {
                if let Some(draft) = self.state.context_handoff.as_mut().filter(|draft| {
                    draft
                        .prepared
                        .as_ref()
                        .is_some_and(|handoff| handoff.handoff_id == handoff_id)
                }) {
                    draft.delivering = false;
                    draft.error = Some(message);
                }
            }
            Reaction::ContextHandoffInvalidated => {
                self.state.context_handoff = None;
            }
        }
    }

    fn open_workspace_directory_chooser(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        if self.pending_folder_request.is_some() {
            return;
        }
        let Some(current_root) = self
            .state
            .workspace_draft
            .as_ref()
            .map(|draft| draft.root.clone())
        else {
            return;
        };
        match self.folder_dialog.open(frame, ctx, &current_root) {
            Ok(request_id) => {
                self.pending_folder_request = Some(request_id);
                self.state.workspace_picker_pending = true;
                if let Some(draft) = self.state.workspace_draft.as_mut() {
                    draft.error = None;
                }
            }
            Err(error) => {
                if let Some(draft) = self.state.workspace_draft.as_mut() {
                    draft.error = Some(error);
                }
            }
        }
    }

    fn apply_folder_dialog_result(&mut self, result: FolderDialogResult) {
        if self.pending_folder_request != Some(result.request_id) {
            return;
        }
        self.pending_folder_request = None;
        self.state.workspace_picker_pending = false;
        let (Some(path), Some(draft)) = (result.path, self.state.workspace_draft.as_mut()) else {
            return;
        };
        if let Err(error) = draft.select_directory(&path) {
            draft.error = Some(error);
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
            let creation_cancellable = self
                .state
                .workspace_draft
                .as_ref()
                .is_some_and(|draft| !draft.submitting)
                || self
                    .state
                    .session_draft
                    .as_ref()
                    .is_some_and(|draft| !draft.submitting)
                || self
                    .state
                    .layout_draft
                    .as_ref()
                    .is_some_and(|draft| !draft.submitting)
                || self
                    .state
                    .context_handoff
                    .as_ref()
                    .is_some_and(|draft| !draft.preparing && !draft.delivering)
                || self.state.lifecycle_confirmation.is_some();
            if self.state.shortcuts_open
                || self.state.settings_open
                || self.state.attention_panel_open
                || creation_cancellable
            {
                let escape = ctx
                    .input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
                if escape {
                    if self.state.layout_draft.is_some() {
                        self.state.layout_draft = None;
                        return Vec::new();
                    }
                    self.state.shortcuts_open = false;
                    self.state.settings_open = false;
                    self.state.attention_panel_open = false;
                    self.state.workspace_draft = None;
                    self.state.session_draft = None;
                    self.state.lifecycle_confirmation = None;
                    self.state.context_handoff = None;
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
        let visible_selection = self.state.selected_tree.clone().or_else(|| {
            self.state
                .hierarchy
                .as_ref()
                .and_then(|hierarchy| hierarchy.tree_state.selected.clone())
        });
        self.desk.set_navigation_hint(visible_selection.clone());

        let form_submitting = self
            .state
            .workspace_draft
            .as_ref()
            .is_some_and(|draft| draft.submitting)
            || self
                .state
                .session_draft
                .as_ref()
                .is_some_and(|draft| draft.submitting);
        if matches!(
            command,
            Command::NewWorkspace | Command::NewSession | Command::QuickNewSession
        ) && (form_submitting || self.desk.creation_in_progress())
        {
            self.desk
                .show_notice("finish the Workspace or Session creation already in progress");
            return true;
        }

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
            Command::PassContext => {
                self.state.context_handoff = self.state.hierarchy.as_ref().and_then(|snapshot| {
                    crate::view::ContextHandoffDraft::from_selection(
                        snapshot,
                        visible_selection.as_ref(),
                    )
                });
                if self.state.context_handoff.is_none() {
                    self.desk.show_notice(
                        "select an Agent with another Agent in the same Session first",
                    );
                }
                true
            }
            Command::NewWorkspace => {
                self.state.session_draft = None;
                self.state.workspace_draft = Some(crate::view::WorkspaceDraft::new(false));
                true
            }
            Command::NewSession => {
                self.state.workspace_draft = None;
                self.state.session_draft = self.desk.new_session_draft();
                if self.state.session_draft.is_none() {
                    self.state.workspace_draft = Some(crate::view::WorkspaceDraft::new(true));
                }
                true
            }
            Command::CloseSession => {
                let selected = self.desk.selected().cloned();
                self.state.lifecycle_confirmation = selected.and_then(|session_id| {
                    self.desk
                        .sessions()
                        .iter()
                        .find(|session| session.id == session_id)
                        .map(|session| crate::view::LifecycleConfirmation::EndSession {
                            session_id,
                            name: session.name.clone(),
                            running_count: session.running_count,
                        })
                });
                if self.state.lifecycle_confirmation.is_none() {
                    self.desk.show_notice("select a Session before ending it");
                }
                true
            }
            Command::QuickNewSession if !self.desk.has_workspaces() => {
                self.state.session_draft = None;
                self.state.workspace_draft = Some(crate::view::WorkspaceDraft::new(true));
                true
            }
            _ => false,
        }
    }
}

impl eframe::App for TurnApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let now_ms = turn_core::now_ms();

        for result in self.folder_dialog.drain() {
            self.apply_folder_dialog_result(result);
        }

        if let Some(events) = &self.companion_events {
            for event in events.try_iter() {
                match event {
                    CompanionEvent::Contended(message) if self.desk.connection().is_live() => {
                        tracing::debug!(%message, "the daemon startup race was resolved by the handshake");
                    }
                    CompanionEvent::Contended(message) => {
                        tracing::warn!(%message, "the daemon companion encountered contention");
                        self.desk.show_companion_notice(message);
                    }
                    CompanionEvent::Failed(message) => {
                        tracing::error!(%message, "the daemon companion exited");
                        self.desk.show_companion_notice(message);
                    }
                }
            }
        }

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
        // Modal sheets own the keyboard. Resolving the global keymap behind them could
        // otherwise stop a process, archive a Session, or open another sheet while a
        // destructive confirmation is still on screen. The palette's own navigation
        // deliberately returns a chosen Command from `steer_overlays` above.
        if !self.state.is_sensitive() {
            commands.extend(self.take_commands(&ctx));
        }
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
                    self.state.session_draft = None;
                    self.state.layout_draft = None;
                    self.state.lifecycle_confirmation = None;
                    self.state.context_handoff = None;
                    self.pending_folder_request = None;
                    self.state.workspace_picker_pending = false;
                }
                ViewAction::ChooseWorkspaceDirectory => {
                    self.open_workspace_directory_chooser(&ctx, frame);
                }
                ViewAction::OpenLayoutEditor(origin) => {
                    self.state.layout_draft = Some(LayoutTemplateDraft::two_shells(origin));
                }
                ViewAction::CloseLayoutEditor => {
                    self.state.layout_draft = None;
                }
                action @ (ViewAction::CreateWorkspace { .. }
                | ViewAction::CreateSessionFromTemplate { .. }
                | ViewAction::CreateLayoutTemplate { .. }) => {
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

    #[test]
    fn cmd_n_on_an_empty_desk_starts_workspace_then_session_onboarding() {
        let ctx = egui::Context::default();
        let mut app = TurnApp::new(
            &ctx,
            std::path::PathBuf::from("/tmp/turn-no-such-daemon-for-onboarding.sock"),
            Keymap::build(&Overrides::new(), Platform::MAC),
        );

        assert!(app.handle_locally(Command::NewSession));
        let draft = app
            .state
            .workspace_draft
            .as_ref()
            .expect("Cmd+N must present a useful first step");
        assert!(draft.continue_to_session);
        assert!(app.state.session_draft.is_none());
    }

    #[test]
    fn cmd_n_uses_the_workspace_the_tree_already_shows_as_selected() {
        let ctx = egui::Context::default();
        let mut app = TurnApp::new(
            &ctx,
            std::path::PathBuf::from("/tmp/turn-no-such-daemon-for-selection.sock"),
            Keymap::build(&Overrides::new(), Platform::MAC),
        );
        let now = 1_700_000_000_000;
        let first = turn_core::model::Workspace::new("first", "/repo/first", now);
        let second = turn_core::model::Workspace::new("second", "/repo/second", now + 1);
        let second_id = second.id.clone();
        app.desk.apply_inbound(
            crate::transport::Inbound::Answer {
                ask: Ask::Hierarchy,
                response: Box::new(turn_proto::Response::Hierarchy {
                    snapshot: Box::new(turn_proto::HierarchySnapshot {
                        revision: 1,
                        tree_state: turn_proto::TreeSurfaceState {
                            surface_id: "main-window".into(),
                            selected: Some(turn_proto::HierarchyKey::workspace(first.id.clone())),
                            expanded: Vec::new(),
                        },
                        workspaces: vec![
                            turn_proto::WorkspaceTreeView {
                                workspace: turn_proto::WorkspaceSummary::from_workspace(
                                    &first,
                                    &[],
                                ),
                                checkouts: Vec::new(),
                                write_lease: None,
                                sessions: Vec::new(),
                            },
                            turn_proto::WorkspaceTreeView {
                                workspace: turn_proto::WorkspaceSummary::from_workspace(
                                    &second,
                                    &[],
                                ),
                                checkouts: Vec::new(),
                                write_lease: None,
                                sessions: Vec::new(),
                            },
                        ],
                    }),
                }),
            },
            now,
        );
        app.state.hierarchy = app.desk.hierarchy().cloned();
        app.state.selected_tree = Some(turn_proto::HierarchyKey::workspace(second_id.clone()));

        assert!(app.handle_locally(Command::NewSession));
        assert_eq!(
            app.state
                .session_draft
                .as_ref()
                .map(|draft| &draft.workspace_id),
            Some(&second_id)
        );
    }

    #[test]
    fn new_workspace_is_a_discoverable_standalone_flow() {
        let ctx = egui::Context::default();
        let mut app = TurnApp::new(
            &ctx,
            std::path::PathBuf::from("/tmp/turn-no-such-daemon-for-workspace.sock"),
            Keymap::build(&Overrides::new(), Platform::MAC),
        );

        assert!(app.handle_locally(Command::NewWorkspace));
        let draft = app.state.workspace_draft.as_ref().unwrap();
        assert!(!draft.continue_to_session);
    }

    #[test]
    fn a_failed_creation_keeps_the_users_session_draft() {
        let ctx = egui::Context::default();
        let mut app = TurnApp::new(
            &ctx,
            std::path::PathBuf::from("/tmp/turn-no-such-daemon-for-errors.sock"),
            Keymap::build(&Overrides::new(), Platform::MAC),
        );
        let workspace_id = turn_core::ids::WorkspaceId::from_stored("ws_onboarding");
        let mut draft = crate::view::SessionDraft::new(workspace_id, None);
        draft.name = "Fix the startup flow".into();
        draft.submitting = true;
        app.state.session_draft = Some(draft);

        app.perform(
            &ctx,
            Reaction::SessionCreationFailed("the directory disappeared".into()),
        );

        let draft = app.state.session_draft.as_ref().unwrap();
        assert_eq!(draft.name, "Fix the startup flow");
        assert!(!draft.submitting);
        assert_eq!(draft.error.as_deref(), Some("the directory disappeared"));
    }

    #[test]
    fn a_created_workspace_continues_into_a_session_draft_for_that_workspace() {
        let ctx = egui::Context::default();
        let mut app = TurnApp::new(
            &ctx,
            std::path::PathBuf::from("/tmp/turn-no-such-daemon-for-continuation.sock"),
            Keymap::build(&Overrides::new(), Platform::MAC),
        );
        let workspace_id = turn_core::ids::WorkspaceId::from_stored("ws_continuation");
        app.state.workspace_draft = Some(crate::view::WorkspaceDraft::new(true));

        app.perform(
            &ctx,
            Reaction::WorkspaceCreated {
                workspace_id: workspace_id.clone(),
                continue_to_session: true,
            },
        );

        assert!(app.state.workspace_draft.is_none());
        assert_eq!(
            app.state
                .session_draft
                .as_ref()
                .map(|draft| &draft.workspace_id),
            Some(&workspace_id)
        );
    }

    #[test]
    fn cancelling_the_native_folder_chooser_preserves_the_workspace_draft() {
        let ctx = egui::Context::default();
        let mut app = TurnApp::new(
            &ctx,
            std::path::PathBuf::from("/tmp/turn-no-such-daemon-for-folder-cancel.sock"),
            Keymap::build(&Overrides::new(), Platform::MAC),
        );
        let mut draft = crate::view::WorkspaceDraft::new(true);
        draft.root = "/Users/x/original".into();
        draft.name = "Original name".into();
        draft.name_is_derived = false;
        let before = draft.clone();
        app.state.workspace_draft = Some(draft);
        app.state.workspace_picker_pending = true;
        app.pending_folder_request = Some(41);

        app.apply_folder_dialog_result(FolderDialogResult {
            request_id: 41,
            path: None,
        });

        assert_eq!(app.state.workspace_draft.as_ref(), Some(&before));
        assert!(!app.state.workspace_picker_pending);
        assert!(app.pending_folder_request.is_none());
    }

    #[test]
    fn a_late_folder_result_cannot_mutate_a_reopened_workspace_form() {
        let ctx = egui::Context::default();
        let mut app = TurnApp::new(
            &ctx,
            std::path::PathBuf::from("/tmp/turn-no-such-daemon-for-stale-folder.sock"),
            Keymap::build(&Overrides::new(), Platform::MAC),
        );
        let mut draft = crate::view::WorkspaceDraft::new(false);
        draft.root = "/Users/x/current".into();
        draft.name = "Current".into();
        app.state.workspace_draft = Some(draft.clone());
        app.state.workspace_picker_pending = true;
        app.pending_folder_request = Some(52);

        app.apply_folder_dialog_result(FolderDialogResult {
            request_id: 51,
            path: Some(std::path::PathBuf::from("/Users/x/stale")),
        });

        assert_eq!(app.state.workspace_draft.as_ref(), Some(&draft));
        assert_eq!(app.pending_folder_request, Some(52));
        assert!(app.state.workspace_picker_pending);
    }

    #[test]
    fn a_command_notice_is_visible_in_the_window() {
        let ctx = egui::Context::default();
        let mut app = TurnApp::new(
            &ctx,
            std::path::PathBuf::from("/tmp/turn-no-such-daemon-for-notice.sock"),
            Keymap::build(&Overrides::new(), Platform::MAC),
        );

        app.perform(&ctx, Reaction::Notice("No template is available".into()));

        assert_eq!(
            app.desk.view(0).notice.as_deref(),
            Some("No template is available")
        );
    }

    #[test]
    fn a_companion_launch_failure_is_visible_in_the_window() {
        let temp = tempfile::tempdir().unwrap();
        let ctx = egui::Context::default();
        let app = TurnApp::new_with_startup_error(
            &ctx,
            temp.path().join("missing.sock"),
            Keymap::build(&Overrides::new(), Platform::MAC),
            Some("Could not start turnd; see /tmp/turnd.log".into()),
        );

        assert_eq!(
            app.desk().view(1_700_000_000_000).notice.as_deref(),
            Some("Could not start turnd; see /tmp/turnd.log")
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
