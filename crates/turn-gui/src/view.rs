//! The window: status, unified hierarchy, session context, panes and contextual overlays.
//!
//! Everything here is a function of display data the daemon supplied. [`TurnView`] is
//! deliberately a plain description of what is on screen rather than a handle on the
//! application, for two reasons: a snapshot test can construct any state it likes
//! without a socket, and nothing in the drawing code can compute a product rule,
//! because it has nothing to compute one from. Protocol v3 supplies one
//! Workspace -> Session -> Process projection; the legacy flat rows exist only while
//! that first snapshot is in flight.
//!
//! What the user does comes back as [`ViewAction`]s. The window therefore cannot move
//! focus, approve a permission or start a process on its own — it can only report that
//! something was clicked, which is what keeps those guarantees out of reach of the draw
//! code.
//!
//! ## Accessibility is not a layer on top
//!
//! A GPU-drawn window has no DOM. If the rows are only pixels then a screen-reader user
//! has no window at all. The unified navigation is therefore a real AccessKit `Tree`
//! whose sensed `TreeItem` rows state their type, state and confidence in words.

use std::collections::HashMap;

use egui::{
    Align2, Color32, FontId, Key, Modifiers, Rect, Response, RichText, Sense, Stroke, Ui, Vec2,
};
use turn_core::attention::AttentionPolicy;
use turn_core::event::Risk;
use turn_core::ids::{AttentionId, LeaseId, NodeId, PaneId, SessionId, TemplateId, WorkspaceId};
use turn_core::model::{
    ActivityPreview, Direction, Layout, LayoutPreset, NodeKind, Pane, PaneKind, PreviewVisibility,
    RelationshipKind, RestoreBehaviour,
};
use turn_core::state::{AwaitingReason, DisplayState};
use turn_proto::cells::Grid;
use turn_proto::{
    HierarchyKey, HierarchySnapshot, NodePaneCapability, NodePaneView, ProtoErrorContext,
    SessionConflictAlternative, SessionTreeView, TemplateSummary, TreeNodeView, TreeSurfaceState,
    WorkspaceSummary, WorkspaceTreeView,
};

use crate::keymap::{Command, Keymap};
use crate::palette::{self, Palette};
use crate::panes::{self, Arrangement, Divider, Side};
use crate::terminal::{self, PaneAction, PaneInteraction, PaneOptions};
use crate::theme::Theme;
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

/// A surface-scoped Pane that intentionally sits outside the saved Layout.
#[derive(Debug)]
pub struct TemporaryPaneContent<'a> {
    pub pane: &'a NodePaneView,
    pub node: Option<&'a TreeNodeView>,
    pub previews: &'a [ActivityPreview],
    pub grid: Option<&'a Grid>,
}

/// What the window is showing.
#[derive(Debug, Default)]
pub struct TurnView<'a> {
    /// Creation choices supplied by the daemon. They are not another navigator.
    pub workspaces: &'a [WorkspaceSummary],
    pub templates: &'a [TemplateSummary],
    pub sessions: Vec<SessionRow>,
    pub selected: Option<SessionId>,
    /// The daemon's layout for the selected session, which is what decides the
    /// geometry. `None` before the first `get_session` answers.
    pub layout: Option<Layout>,
    pub panes: Vec<PaneContent<'a>>,
    /// At most one explicit temporary Pane for this window. Rendering it must never
    /// insert its PaneId into `layout`.
    pub temporary_pane: Option<TemporaryPaneContent<'a>>,
    pub permission: Option<PendingPermission>,
    /// The daemon's ordered queue still backs the global Next Attention action, but is
    /// not rendered as a second permanent navigation panel.
    pub queue: Vec<QueueItem>,
    pub connection: Option<ConnectionState>,
    /// A failure worth showing, from a request that did not work.
    pub notice: Option<String>,
    /// Typed checkout conflict, rendered as a recovery flow rather than parsed text.
    pub write_conflict: Option<&'a ProtoErrorContext>,
    /// The attention policy in force, for the settings sheet.
    pub policy: Option<AttentionPolicy>,
    pub now_ms: i64,
}

/// The window's own mutable state: what is typed in the palette, and what is selected
/// in each pane.
#[derive(Debug, Default)]
pub struct ViewState {
    pub palette: Palette,
    pub panes: HashMap<PaneId, PaneInteraction>,
    /// Which command sheet is open, if any.
    pub shortcuts_open: bool,
    pub settings_open: bool,
    /// Explicit, temporary view of the daemon-owned Attention Queue. It is an
    /// overlay, never a second persistent navigator beside the hierarchy.
    pub attention_panel_open: bool,
    pub write_conflict_open: bool,
    /// First-run workspace form. It is window-local until the user submits it;
    /// no half-written path enters daemon state.
    pub workspace_draft: Option<WorkspaceDraft>,
    /// A native folder sheet is outstanding. It disables duplicate Browse/Create
    /// actions but causes no polling or continuous repaint.
    pub workspace_picker_pending: bool,
    /// The explicit New Session sheet. `Cmd+N` opens this; only Quick New may bypass it.
    pub session_draft: Option<SessionDraft>,
    /// Reusable visual editor shared by New Session and Settings. It contains only
    /// a local draft; no process starts until a Session is explicitly created.
    pub layout_draft: Option<LayoutTemplateDraft>,
    /// The daemon-owned navigation projection for this window. `None` is kept as a
    /// narrow compatibility path while the first hierarchy snapshot is in flight.
    pub hierarchy: Option<HierarchySnapshot>,
    /// An optimistic selection for immediate keyboard and pointer feedback. The daemon
    /// remains authoritative and replaces it when a later snapshot selects a new row.
    pub selected_tree: Option<HierarchyKey>,
    /// One-shot request to reveal a selection whose ancestors were just expanded.
    pub scroll_tree_to: Option<HierarchyKey>,
    /// Per-row expansion overrides awaiting acknowledgement from the daemon.
    pub tree_expansion: HashMap<HierarchyKey, bool>,
    /// Whether navigation keys belong to the hierarchy rather than the terminal.
    pub tree_has_focus: bool,
    /// A read-only overlay. Opening it never changes the pane layout or terminal focus.
    pub quick_preview: Option<HierarchyKey>,
    /// Bounded, stable/redacted semantic history fetched on demand for Quick Preview.
    pub preview_history: HashMap<NodeId, Vec<ActivityPreview>>,
    /// The inspector is contextual and only occupies space for a selected Process.
    pub inspector_open: bool,
    hierarchy_actions: Vec<HierarchyAction>,
    observed_tree_state: Option<TreeSurfaceState>,
    observed_temporary_pane: Option<PaneId>,
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
        self.palette.open
            || self.shortcuts_open
            || self.settings_open
            || self.attention_panel_open
            || self.write_conflict_open
            || self.workspace_draft.is_some()
            || self.session_draft.is_some()
            || self.layout_draft.is_some()
            || self.quick_preview.is_some()
    }

    /// Drains typed hierarchy intents after drawing.
    ///
    /// This side channel keeps [`ViewAction`] source-compatible with the current Desk
    /// while protocol-v3 hierarchy routing is wired in. It can be removed once every
    /// caller consumes hierarchy actions directly.
    pub fn take_hierarchy_actions(&mut self) -> Vec<HierarchyAction> {
        std::mem::take(&mut self.hierarchy_actions)
    }

    fn push_hierarchy_action(&mut self, action: HierarchyAction) {
        self.hierarchy_actions.push(action);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceDraft {
    pub name: String,
    pub root: String,
    /// True while `name` is the suggestion derived from `root`. Typing a custom name
    /// makes it false; choosing another folder intentionally starts a new suggestion.
    pub name_is_derived: bool,
    /// `Cmd+N` with an empty desk is a two-step onboarding flow. An explicit New
    /// Workspace command stops after the Workspace is created.
    pub continue_to_session: bool,
    /// One-shot focus request for the first editable field when the sheet opens.
    pub request_name_focus: bool,
    pub submitting: bool,
    pub error: Option<String>,
}

impl WorkspaceDraft {
    pub fn new(continue_to_session: bool) -> Self {
        let root = std::env::current_dir()
            .ok()
            .filter(|path| path != std::path::Path::new("/"))
            .and_then(|path| path.to_str().map(str::to_owned))
            .unwrap_or_default();
        let name = if root.is_empty() {
            String::new()
        } else {
            turn_core::model::Workspace::name_from_path(&root)
        };
        Self {
            name,
            root,
            name_is_derived: true,
            continue_to_session,
            request_name_focus: true,
            submitting: false,
            error: None,
        }
    }

    /// Applies an explicit native-folder selection and derives its editable default name.
    pub fn select_directory(&mut self, path: &std::path::Path) -> Result<(), String> {
        let root = path
            .to_str()
            .ok_or_else(|| "The selected folder name is not valid Unicode.".to_string())?;
        let name = path
            .file_name()
            .and_then(|part| part.to_str())
            .filter(|part| !part.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| turn_core::model::Workspace::name_from_path(root));
        self.root = root.to_string();
        self.name = name;
        self.name_is_derived = true;
        self.request_name_focus = true;
        self.error = None;
        Ok(())
    }

    /// Keeps a still-unedited suggestion in sync when a power user types a path.
    fn refresh_derived_name(&mut self) {
        if self.name_is_derived {
            self.name = if self.root.trim().is_empty() {
                String::new()
            } else {
                turn_core::model::Workspace::name_from_path(self.root.trim())
            };
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionDraft {
    pub workspace_id: WorkspaceId,
    pub template_id: Option<TemplateId>,
    pub name: String,
    pub task: String,
    /// One-shot focus request for the task name when the sheet opens.
    pub request_name_focus: bool,
    pub submitting: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutEditorOrigin {
    NewSession,
    Settings,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CellCommandDraft {
    /// Empty means the Workspace's configured shell. It is never passed to a shell
    /// for interpretation; a non-empty value is an executable program.
    pub program: String,
    /// Shell-style quoting is parsed into argv, but operators are not evaluated.
    pub arguments: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LayoutTemplateDraft {
    pub name: String,
    pub layout: Layout,
    pub selected: PaneId,
    pub cells: HashMap<PaneId, CellCommandDraft>,
    pub dragged_pane: Option<PaneId>,
    pub origin: LayoutEditorOrigin,
    pub submitting: bool,
    pub error: Option<String>,
}

impl LayoutTemplateDraft {
    pub fn two_shells(origin: LayoutEditorOrigin) -> Self {
        let left = Pane::new(PaneKind::Shell)
            .with_title("shell")
            .with_restore(RestoreBehaviour::Relaunch);
        let selected = left.id.clone();
        let right = Pane::new(PaneKind::Shell)
            .with_title("shell")
            .with_restore(RestoreBehaviour::Relaunch);
        let mut layout = Layout::single(left);
        layout.split(&selected, Direction::Horizontal, right);
        layout.active = Some(selected.clone());
        let cells = layout
            .panes()
            .into_iter()
            .map(|pane| (pane.id.clone(), CellCommandDraft::default()))
            .collect();
        Self {
            name: String::new(),
            layout,
            selected,
            cells,
            dragged_pane: None,
            origin,
            submitting: false,
            error: None,
        }
    }

    fn split_selected(&mut self, direction: Direction) {
        if self.layout.pane_count() >= 16 {
            self.error = Some("A layout can contain at most 16 cells.".into());
            return;
        }
        let pane = Pane::new(PaneKind::Shell)
            .with_title("shell")
            .with_restore(RestoreBehaviour::Relaunch);
        let id = pane.id.clone();
        if self.layout.split(&self.selected, direction, pane) {
            self.cells.insert(id.clone(), CellCommandDraft::default());
            self.selected = id;
            self.error = None;
        }
    }

    fn remove_selected(&mut self) {
        let removed = self.selected.clone();
        if self.layout.close(&removed) {
            self.cells.remove(&removed);
            if let Some(next) = self.layout.active.clone() {
                self.selected = next;
            }
            self.error = None;
        } else {
            self.error = Some("A layout needs at least one cell.".into());
        }
    }

    fn materialized_layout(&self) -> Result<Layout, String> {
        let mut layout = self.layout.clone();
        let ids: Vec<_> = layout
            .panes()
            .into_iter()
            .map(|pane| pane.id.clone())
            .collect();
        for id in ids {
            let command = self.cells.get(&id).cloned().unwrap_or_default();
            let pane = layout
                .get_mut(&id)
                .ok_or_else(|| "A layout cell disappeared while saving.".to_string())?;
            let program = command.program.trim();
            if program.is_empty() {
                pane.kind = PaneKind::Shell;
                pane.command = None;
                pane.args.clear();
                pane.title = Some("shell".into());
            } else {
                let args = shell_words::split(command.arguments.trim())
                    .map_err(|error| format!("Invalid arguments for {program}: {error}"))?;
                pane.kind = if matches!(program, "claude" | "codex" | "gemini" | "opencode") {
                    PaneKind::Agent
                } else {
                    PaneKind::Terminal
                };
                pane.command = Some(program.to_string());
                pane.args = args;
                pane.title = Some(program.rsplit('/').next().unwrap_or(program).to_string());
            }
            pane.node_id = None;
            pane.restore = RestoreBehaviour::Relaunch;
        }
        layout.zoomed = None;
        layout.normalise();
        Ok(layout)
    }

    fn cell_label(&self, pane_id: &PaneId) -> String {
        self.cells
            .get(pane_id)
            .map(|cell| cell.program.trim())
            .filter(|program| !program.is_empty())
            .unwrap_or("Shell")
            .to_string()
    }
}

impl SessionDraft {
    pub fn new(workspace_id: WorkspaceId, template_id: Option<TemplateId>) -> Self {
        Self {
            workspace_id,
            template_id,
            name: String::new(),
            task: String::new(),
            request_name_focus: true,
            submitting: false,
            error: None,
        }
    }
}

/// A hierarchy interaction, kept separate from terminal and legacy session actions.
/// Selection, opening and focus are intentionally different operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HierarchyAction {
    Select {
        surface_id: String,
        key: HierarchyKey,
    },
    SetExpanded {
        surface_id: String,
        key: HierarchyKey,
        expanded: bool,
    },
    QuickPreview {
        surface_id: String,
        session_id: SessionId,
        node_id: NodeId,
    },
    SetPreviewVisibility {
        session_id: SessionId,
        node_id: NodeId,
        visibility: PreviewVisibility,
    },
    OpenTemporaryPane {
        surface_id: String,
        session_id: SessionId,
        node_id: NodeId,
    },
    FocusPaneForNode {
        surface_id: String,
        session_id: SessionId,
        node_id: NodeId,
    },
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
        before: PaneId,
        after: PaneId,
        fraction: f32,
    },
    EqualizeDivider {
        before: PaneId,
        after: PaneId,
    },
    ApplyLayoutPreset(LayoutPreset),
    /// Go to a specific demand — the one the banner or the row is showing, never an
    /// arbitrary one.
    GotoAttention(AttentionId),
    DismissAttention(AttentionId),
    SnoozeAttention {
        attention_id: AttentionId,
        until_ms: i64,
    },
    MuteAttentionSession {
        session_id: SessionId,
        until_ms: Option<i64>,
    },
    /// Stopping an Agent is independent from closing any of its views.
    TerminateNode {
        session_id: SessionId,
        node_id: NodeId,
    },
    CreateWorkspace {
        name: String,
        root: String,
        continue_to_session: bool,
    },
    /// Opens the native project-folder chooser. The view only reports the intent;
    /// platform UI is owned by `TurnApp` outside this pure renderer.
    ChooseWorkspaceDirectory,
    CreateSessionFromTemplate {
        workspace_id: WorkspaceId,
        template_id: TemplateId,
        name: String,
        task: Option<String>,
    },
    OpenLayoutEditor(LayoutEditorOrigin),
    CloseLayoutEditor,
    CreateLayoutTemplate {
        name: String,
        layout: Layout,
    },
    ReleaseWorkspaceLease {
        workspace_id: WorkspaceId,
        lease_id: LeaseId,
        expected_generation: u64,
    },
    ResolveWriteConflict(SessionConflictAlternative),
    /// Closing a temporary view always keeps the Agent/Process alive.
    CloseTemporaryPane {
        session_id: SessionId,
        pane_id: PaneId,
    },
    /// Close a sheet.
    CloseOverlay,
}

/// A region of the window, with an id of its own.
///
/// The salt is not decoration. `egui` derives a widget's id from its parent plus a
/// counter, so two sibling regions that lay out the same number of widgets produce the
/// same ids — and two widgets sharing an id share their interaction state, which shows up
/// as the command palette scrolling the hierarchy. Naming each region makes that
/// impossible rather than unlikely.
fn region(rect: Rect, name: &'static str) -> egui::UiBuilder {
    egui::UiBuilder::new().max_rect(rect).id_salt(name)
}

const SIDEBAR_WIDTH: f32 = 344.0;
const INSPECTOR_WIDTH: f32 = 264.0;
const STATUS_HEIGHT: f32 = 26.0;
const ROW_HEIGHT: f32 = 40.0;
const PANE_HEADER: f32 = 22.0;

#[derive(Clone, Copy)]
enum HierarchyRow<'a> {
    Workspace(&'a WorkspaceTreeView),
    Session {
        workspace: &'a WorkspaceTreeView,
        session: &'a SessionTreeView,
    },
    Process {
        session: &'a SessionTreeView,
        node: &'a TreeNodeView,
    },
}

impl HierarchyRow<'_> {
    fn key(self) -> HierarchyKey {
        match self {
            Self::Workspace(workspace) => HierarchyKey::workspace(workspace.workspace.id.clone()),
            Self::Session { session, .. } => HierarchyKey::session(session.session.id.clone()),
            Self::Process { node, .. } => HierarchyKey::process(node.node_id.clone()),
        }
    }

    fn depth(self) -> usize {
        match self {
            Self::Workspace(_) => 0,
            Self::Session { .. } => 1,
            Self::Process { node, .. } => node.depth.saturating_add(2),
        }
    }

    fn child_count(self) -> usize {
        match self {
            Self::Workspace(workspace) => workspace.sessions.len(),
            Self::Session { session, .. } => {
                session.nodes.iter().filter(|node| node.depth == 0).count()
            }
            Self::Process { node, .. } => node.child_count,
        }
    }

    fn parent_key(self) -> Option<HierarchyKey> {
        match self {
            Self::Workspace(_) => None,
            Self::Session { workspace, .. } => {
                Some(HierarchyKey::workspace(workspace.workspace.id.clone()))
            }
            Self::Process { session, node } => Some(match &node.parent {
                Some(parent) => HierarchyKey::process(parent.clone()),
                None => HierarchyKey::session(session.session.id.clone()),
            }),
        }
    }

    fn height(self) -> f32 {
        match self {
            Self::Workspace(_) => 34.0,
            Self::Session { .. } => 46.0,
            Self::Process { .. } => 56.0,
        }
    }

    fn accessible_name(self, focused_pane: bool) -> String {
        match self {
            Self::Workspace(workspace) => {
                let summary = &workspace.workspace;
                let mut name = format!(
                    "Workspace {} — {} sessions",
                    summary.name, summary.session_count
                );
                if summary.sessions_needing_user > 0 {
                    name.push_str(&format!(
                        " — {} sessions need attention",
                        summary.sessions_needing_user
                    ));
                }
                if summary.lease_reconciliation_required {
                    name.push_str(" — write lease needs reconciliation");
                }
                name
            }
            Self::Session { session, .. } => {
                let summary = &session.session;
                let mut name = format!(
                    "Session {} — mode {} — {}",
                    summary.name,
                    summary.mode.label(),
                    summary.state_label
                );
                if summary.badge_count > 0 {
                    name.push_str(&format!(
                        " — {} attention demand{}",
                        summary.badge_count,
                        if summary.badge_count == 1 { "" } else { "s" }
                    ));
                }
                if summary.muted {
                    name.push_str(" — muted");
                }
                name
            }
            Self::Process { node, .. } => {
                let mut name = format!(
                    "{} {} — {}",
                    node_kind_label(node.kind),
                    process_title(node),
                    node.state_label
                );
                if node.relationship_is_provisional {
                    name.push_str(" — relationship inferred");
                }
                if let Some(preview) = visible_preview(node) {
                    name.push_str(&format!(" — {}", preview.normalized_text));
                }
                if focused_pane {
                    name.push_str(" — focused pane");
                } else if !node.pane_bindings.is_empty() {
                    name.push_str(" — pane open");
                }
                name
            }
        }
    }
}

fn node_kind_label(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Agent => "AGENT",
        NodeKind::Subagent => "SUBAGENT",
        NodeKind::Shell => "SHELL",
        NodeKind::Terminal => "TERMINAL",
        NodeKind::Tui => "TUI",
        NodeKind::Server => "SERVER",
        NodeKind::Watcher => "WATCHER",
        NodeKind::TestRunner => "TESTS",
        NodeKind::Build => "BUILD",
        NodeKind::Background => "BACKGROUND",
        NodeKind::TmuxSession => "TMUX SESSION",
        NodeKind::TmuxPane => "TMUX PANE",
        NodeKind::Unknown => "PROCESS",
    }
}

fn process_title(node: &TreeNodeView) -> &str {
    node.agent
        .as_ref()
        .map(|agent| agent.name.display_name.as_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(&node.title)
}

fn visible_preview(node: &TreeNodeView) -> Option<&turn_core::model::ActivityPreview> {
    if matches!(node.preview_visibility, PreviewVisibility::Hide)
        || node
            .activity_preview
            .as_ref()
            .is_some_and(|preview| preview.contains_sensitive_data && !preview.redacted)
    {
        None
    } else {
        node.activity_preview.as_ref()
    }
}

/// Quick Preview consumes the protocol's newest-first history as-is. Keeping
/// this selection separate from painting makes the ordering contract testable:
/// index zero is the highlighted current item, followed by the three preceding
/// stable facts.
fn quick_preview_history(history: &[ActivityPreview]) -> Vec<ActivityPreview> {
    history
        .iter()
        .filter(|preview| !preview.contains_sensitive_data || preview.redacted)
        .take(4)
        .cloned()
        .collect()
}

fn effective_selection(snapshot: &HierarchySnapshot, state: &ViewState) -> Option<HierarchyKey> {
    state
        .selected_tree
        .clone()
        .or_else(|| snapshot.tree_state.selected.clone())
}

fn workspace_for_key(snapshot: &HierarchySnapshot, key: &HierarchyKey) -> Option<WorkspaceId> {
    match key {
        HierarchyKey::Workspace { workspace_id } => Some(workspace_id.clone()),
        HierarchyKey::Session { session_id } => snapshot.workspaces.iter().find_map(|workspace| {
            workspace
                .sessions
                .iter()
                .any(|session| &session.session.id == session_id)
                .then(|| workspace.workspace.id.clone())
        }),
        HierarchyKey::Process { node_id } => snapshot.workspaces.iter().find_map(|workspace| {
            workspace
                .sessions
                .iter()
                .any(|session| session.nodes.iter().any(|node| &node.node_id == node_id))
                .then(|| workspace.workspace.id.clone())
        }),
    }
}

fn row_is_expanded(snapshot: &HierarchySnapshot, state: &ViewState, key: &HierarchyKey) -> bool {
    state
        .tree_expansion
        .get(key)
        .copied()
        .unwrap_or_else(|| snapshot.tree_state.expanded.contains(key))
}

fn visible_hierarchy_rows<'a>(
    snapshot: &'a HierarchySnapshot,
    state: &ViewState,
) -> Vec<HierarchyRow<'a>> {
    let mut rows = Vec::new();
    for workspace in &snapshot.workspaces {
        let workspace_row = HierarchyRow::Workspace(workspace);
        let workspace_key = workspace_row.key();
        rows.push(workspace_row);
        if !row_is_expanded(snapshot, state, &workspace_key) {
            continue;
        }

        for session in &workspace.sessions {
            let session_row = HierarchyRow::Session { workspace, session };
            let session_key = session_row.key();
            rows.push(session_row);
            if !row_is_expanded(snapshot, state, &session_key) {
                continue;
            }

            let mut collapsed_depth = None;
            for node in &session.nodes {
                if let Some(depth) = collapsed_depth {
                    if node.depth > depth {
                        continue;
                    }
                    collapsed_depth = None;
                }
                let process_row = HierarchyRow::Process { session, node };
                let process_key = process_row.key();
                rows.push(process_row);
                if node.child_count > 0 && !row_is_expanded(snapshot, state, &process_key) {
                    collapsed_depth = Some(node.depth);
                }
            }
        }
    }
    rows
}

fn selected_process<'a>(
    snapshot: &'a HierarchySnapshot,
    selected: Option<&HierarchyKey>,
) -> Option<&'a TreeNodeView> {
    let HierarchyKey::Process { node_id } = selected? else {
        return None;
    };
    snapshot
        .workspaces
        .iter()
        .flat_map(|workspace| &workspace.sessions)
        .flat_map(|session| &session.nodes)
        .find(|node| &node.node_id == node_id)
}

fn active_session_context<'a>(
    snapshot: &'a HierarchySnapshot,
    active: Option<&SessionId>,
) -> Option<(&'a WorkspaceTreeView, &'a SessionTreeView)> {
    let active = active?;
    snapshot.workspaces.iter().find_map(|workspace| {
        workspace
            .sessions
            .iter()
            .find(|session| &session.session.id == active)
            .map(|session| (workspace, session))
    })
}

impl<'a> TurnView<'a> {
    fn preferred_template_id(&self, workspace_id: &WorkspaceId) -> Option<TemplateId> {
        let configured = self
            .workspaces
            .iter()
            .find(|workspace| &workspace.id == workspace_id)
            .and_then(|workspace| workspace.default_template.as_ref());
        configured
            .and_then(|id| self.templates.iter().find(|template| &template.id == id))
            .or_else(|| self.templates.first())
            .map(|template| template.id.clone())
    }

    fn new_session_draft(
        &self,
        snapshot: &HierarchySnapshot,
        state: &ViewState,
    ) -> Option<SessionDraft> {
        let workspace_id = effective_selection(snapshot, state)
            .as_ref()
            .and_then(|key| workspace_for_key(snapshot, key))
            .or_else(|| {
                snapshot
                    .workspaces
                    .first()
                    .map(|branch| branch.workspace.id.clone())
            })?;
        let template_id = self.preferred_template_id(&workspace_id);
        Some(SessionDraft::new(workspace_id, template_id))
    }

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

        // Temporarily take the snapshot so the UI may update its local interaction
        // state without cloning the complete process tree every frame.
        let hierarchy = state.hierarchy.take();
        let incoming_tree_state = hierarchy
            .as_ref()
            .map(|snapshot| snapshot.tree_state.clone());
        if incoming_tree_state != state.observed_tree_state {
            let previous_selection = state
                .observed_tree_state
                .as_ref()
                .and_then(|tree| tree.selected.as_ref());
            let next_selection = incoming_tree_state
                .as_ref()
                .and_then(|tree| tree.selected.as_ref());
            if previous_selection != next_selection {
                state.scroll_tree_to = next_selection.cloned();
            }
            state.selected_tree = None;
            state.tree_expansion.clear();
            state.observed_tree_state = incoming_tree_state;
        }
        let temporary_pane_id = self
            .temporary_pane
            .as_ref()
            .map(|temporary| temporary.pane.binding.pane_id.clone());
        if temporary_pane_id != state.observed_temporary_pane {
            if temporary_pane_id.is_some() {
                // Opening a temporary terminal is an explicit request for its
                // keyboard lease. It must not leave navigation consuming Enter,
                // arrows or Space in the background.
                state.tree_has_focus = false;
            }
            state.observed_temporary_pane = temporary_pane_id;
        }

        actions.extend(self.status_bar(ui, theme, keymap, hierarchy.as_ref()));
        if let Some(permission) = &self.permission {
            actions.extend(self.permission_banner(ui, theme, permission));
        }
        if let Some(notice) = &self.notice {
            self.notice_bar(ui, theme, notice);
        }

        if state.quick_preview.is_some()
            && ui.input_mut(|input| input.consume_key(Modifiers::NONE, Key::Escape))
        {
            state.quick_preview = None;
        } else if !state.is_sensitive() {
            if let Some(temporary) = &self.temporary_pane {
                if ui.input_mut(|input| input.consume_key(Modifiers::NONE, Key::Escape)) {
                    actions.push(ViewAction::CloseTemporaryPane {
                        session_id: temporary.pane.binding.session_id.clone(),
                        pane_id: temporary.pane.binding.pane_id.clone(),
                    });
                    state.tree_has_focus = true;
                }
            }
        }

        let body = ui.available_rect_before_wrap();
        let sidebar_width = SIDEBAR_WIDTH
            .min((body.width() * 0.42).max(80.0))
            .min(body.width());
        let current_tree_selection = hierarchy
            .as_ref()
            .and_then(|snapshot| effective_selection(snapshot, state));
        let selected_node = hierarchy
            .as_ref()
            .and_then(|snapshot| selected_process(snapshot, current_tree_selection.as_ref()));
        let active_context = hierarchy
            .as_ref()
            .and_then(|snapshot| active_session_context(snapshot, self.selected.as_ref()));
        let inspector_is_overlay = body.width() < 960.0;
        let inspector_width =
            if state.inspector_open && selected_node.is_some() && !inspector_is_overlay {
                INSPECTOR_WIDTH.min((body.width() - sidebar_width).max(0.0) * 0.4)
            } else {
                0.0
            };
        let sidebar = Rect::from_min_size(body.min, Vec2::new(sidebar_width, body.height()));
        let centre = Rect::from_min_size(
            body.min + Vec2::new(sidebar_width, 0.0),
            Vec2::new(
                (body.width() - sidebar_width - inspector_width).max(0.0),
                body.height(),
            ),
        );

        ui.scope_builder(region(sidebar, "sidebar"), |ui| {
            match hierarchy.as_ref() {
                Some(snapshot) => {
                    actions.extend(self.hierarchy_sidebar(ui, theme, snapshot, state));
                }
                // Startup and legacy fixtures get one fallback list, never a second
                // navigation surface beside the hierarchy.
                None => actions.extend(self.sidebar(ui, theme)),
            }
        });
        ui.painter()
            .vline(centre.min.x, body.y_range(), Stroke::new(1.0, theme.border));

        let context_height = if active_context.is_some() {
            46.0_f32.min(centre.height())
        } else {
            0.0
        };
        let footer_height = if self.layout.is_some() {
            23.0_f32.min((centre.height() - context_height).max(0.0))
        } else {
            0.0
        };
        let context_rect =
            Rect::from_min_size(centre.min, Vec2::new(centre.width(), context_height));
        let pane_rect = Rect::from_min_max(
            centre.min + Vec2::new(0.0, context_height),
            centre.max - Vec2::new(0.0, footer_height),
        );
        let footer_rect = Rect::from_min_size(
            pane_rect.left_bottom(),
            Vec2::new(centre.width(), footer_height),
        );
        if let Some((workspace, session)) = active_context {
            ui.scope_builder(region(context_rect, "session-context"), |ui| {
                actions.extend(self.session_context_bar(ui, theme, workspace, session));
            });
        }
        ui.scope_builder(region(pane_rect.shrink(1.0), "panes"), |ui| {
            let pane_actions = self.pane_area(ui, theme, keymap, state, hierarchy.as_ref());
            if pane_actions.iter().any(|action| {
                matches!(
                    action,
                    ViewAction::Pane {
                        action: PaneAction::Focus | PaneAction::Write(_),
                        ..
                    }
                )
            }) {
                state.tree_has_focus = false;
            }
            actions.extend(pane_actions);
        });
        if let Some(temporary) = &self.temporary_pane {
            ui.scope_builder(region(pane_rect.shrink(8.0), "temporary-pane"), |ui| {
                let temporary_actions = self.temporary_pane_overlay(ui, theme, state, temporary);
                if temporary_actions.iter().any(|action| {
                    matches!(
                        action,
                        ViewAction::Pane {
                            action: PaneAction::Focus | PaneAction::Write(_),
                            ..
                        }
                    )
                }) {
                    state.tree_has_focus = false;
                }
                if temporary_actions
                    .iter()
                    .any(|action| matches!(action, ViewAction::CloseTemporaryPane { .. }))
                {
                    state.tree_has_focus = true;
                }
                actions.extend(temporary_actions);
            });
        }
        if footer_height > 0.0 {
            ui.scope_builder(region(footer_rect, "pane-status"), |ui| {
                self.pane_status_bar(ui, theme, active_context.map(|(_, session)| session));
            });
        }

        if inspector_width > 0.0 {
            let inspector = Rect::from_min_size(
                centre.right_top(),
                Vec2::new(inspector_width, body.height()),
            );
            ui.painter().vline(
                inspector.min.x,
                body.y_range(),
                Stroke::new(1.0, theme.border),
            );
            ui.scope_builder(region(inspector.shrink(1.0), "inspector"), |ui| {
                if let Some(node) = selected_node {
                    self.process_inspector(ui, theme, node, state);
                }
            });
        } else if state.inspector_open && inspector_is_overlay {
            if let Some(node) = selected_node {
                self.inspector_overlay(ui, theme, node, state, body);
            }
        }

        // Our overlays are custom-drawn rather than `egui::Window`s, so they need an
        // explicit hit-test shield. Registered after the background and before the
        // sheet's own controls, it swallows clicks/drags aimed through the dimmed layer
        // while leaving the foreground controls interactive.
        if state.is_sensitive() || self.write_conflict.is_some() {
            ui.interact(
                full,
                ui.id().with("modal-input-shield"),
                Sense::click_and_drag(),
            );
        }

        if let (Some(snapshot), Some(key)) = (hierarchy.as_ref(), state.quick_preview.clone()) {
            if let Some(node) = selected_process(snapshot, Some(&key)) {
                self.quick_preview_overlay(ui, theme, snapshot, node, state, full);
            } else {
                state.quick_preview = None;
            }
        }

        if state.layout_draft.is_some() {
            actions.extend(self.layout_editor_overlay(ui, theme, state, full));
        } else if state.workspace_draft.is_some() {
            actions.extend(self.workspace_creator_overlay(ui, theme, state, full));
        } else if let Some(conflict) = self.write_conflict {
            actions.extend(self.write_conflict_overlay(ui, theme, conflict, full));
        } else if state.session_draft.is_some() {
            actions.extend(self.session_creator_overlay(ui, theme, state, full));
        } else if state.attention_panel_open {
            actions.extend(self.attention_queue_overlay(ui, theme, full));
        } else if state.palette.open {
            actions.extend(self.palette_overlay(ui, theme, keymap, state, full));
        } else if state.shortcuts_open {
            actions.extend(self.shortcuts_sheet(ui, theme, keymap, full));
        } else if state.settings_open {
            actions.extend(self.settings_sheet(ui, theme, state, full));
        }
        state.hierarchy = hierarchy;
        actions
    }

    fn status_bar(
        &self,
        ui: &mut Ui,
        theme: &Theme,
        keymap: &Keymap,
        hierarchy: Option<&HierarchySnapshot>,
    ) -> Vec<ViewAction> {
        let mut actions = Vec::new();
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

                let waiting = hierarchy.map_or_else(
                    || {
                        self.sessions
                            .iter()
                            .filter(|row| row.state.demands_user() || row.badge > 0)
                            .count()
                    },
                    |snapshot| {
                        snapshot
                            .workspaces
                            .iter()
                            .map(|workspace| workspace.workspace.sessions_needing_user)
                            .sum()
                    },
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if waiting > 0 {
                        let shortcut = keymap
                            .chord_for(Command::NextAttention)
                            .map(|chord| chord.describe(keymap.platform()))
                            .unwrap_or_default();
                        let label = if shortcut.is_empty() {
                            format!("Next attention · {waiting}")
                        } else {
                            format!("Next attention · {waiting} · {shortcut}")
                        };
                        if ui
                            .add(egui::Button::new(
                                RichText::new(label).color(theme.attention).small(),
                            ))
                            .on_hover_text("Go to the next actionable item in the attention queue")
                            .clicked()
                        {
                            actions.push(ViewAction::Run(Command::NextAttention));
                        }
                        if ui
                            .add(egui::Button::new(
                                RichText::new("Queue").color(theme.text_dim).small(),
                            ))
                            .on_hover_text("Open the Attention Queue")
                            .clicked()
                        {
                            actions.push(ViewAction::Run(Command::ToggleAttentionPanel));
                        }
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
        actions
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

    fn write_conflict_overlay(
        &self,
        ui: &mut Ui,
        theme: &Theme,
        conflict: &ProtoErrorContext,
        full: Rect,
    ) -> Vec<ViewAction> {
        let ProtoErrorContext::WorkspaceWriteLeaseConflict {
            owner,
            lease,
            alternatives,
            ..
        } = conflict
        else {
            return Vec::new();
        };
        let mut actions = Vec::new();
        let owner_state = self
            .sessions
            .iter()
            .find(|session| session.id == owner.session_id)
            .map(|session| session.state_label.as_str())
            .unwrap_or("active");
        let width = 650.0_f32.min((full.width() - 36.0).max(0.0));
        let height = 340.0_f32.min((full.height() - 36.0).max(0.0));
        let panel = Rect::from_center_size(full.center(), Vec2::new(width, height));
        ui.painter()
            .rect_filled(full, 0.0, Color32::from_black_alpha(170));
        ui.painter().rect_filled(panel, 0.0, theme.panel);
        ui.painter().rect_stroke(
            panel,
            0.0,
            Stroke::new(1.0, theme.attention),
            egui::StrokeKind::Outside,
        );

        ui.scope_builder(region(panel.shrink(18.0), "write-lease-conflict"), |ui| {
            ui.label(
                RichText::new("PRIMARY CHECKOUT ALREADY HAS A WRITER")
                    .monospace()
                    .color(theme.attention)
                    .strong(),
            );
            ui.add_space(8.0);
            ui.label(
                RichText::new("This workspace already has an active writing session:")
                    .color(theme.text_dim),
            );
            ui.group(|ui| {
                ui.label(RichText::new(&owner.session_name).color(theme.text).strong());
                ui.label(
                    RichText::new(format!(
                        "{} · {} · {}",
                        owner.mode.label(),
                        owner_state,
                        owner.branch.as_deref().unwrap_or("no branch")
                    ))
                    .monospace()
                    .color(theme.text_dim),
                );
                ui.label(
                    RichText::new(format!("checkout {} · {}", lease.checkout_id, owner.cwd))
                        .monospace()
                        .color(theme.text_faint)
                        .small(),
                );
            });
            ui.add_space(8.0);
            ui.label(RichText::new("Choose how to continue:").color(theme.text));
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                let mut choice = |ui: &mut Ui,
                                  alternative: SessionConflictAlternative,
                                  label: &str,
                                  tooltip: &str| {
                    if alternatives.contains(&alternative)
                        && ui.button(label).on_hover_text(tooltip).clicked()
                    {
                        actions.push(ViewAction::ResolveWriteConflict(alternative));
                    }
                };
                choice(
                    ui,
                    SessionConflictAlternative::FocusOwner,
                    "Focus existing session",
                    "Use the Session that already owns the primary checkout",
                );
                choice(
                    ui,
                    SessionConflictAlternative::CreateReadOnly,
                    "Open read-only session",
                    "Turn will launch nothing unless it can enforce the write guard technically",
                );
                choice(
                    ui,
                    SessionConflictAlternative::CreateIsolatedWorktree,
                    "Create isolated worktree",
                    "Creates a separate directory and branch; ports, Docker and databases may still collide",
                );
                choice(
                    ui,
                    SessionConflictAlternative::Cancel,
                    "Cancel",
                    "Create nothing",
                );
            });
            ui.add_space(10.0);
            ui.label(
                RichText::new(
                    "Turn never starts a second writer on the same checkout. Worktrees do not isolate ports, Docker, global caches, credentials or local services.",
                )
                .color(theme.text_faint)
                .small(),
            );
        });
        actions
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

    /// The only navigation surface once protocol v3 has supplied a hierarchy.
    fn hierarchy_sidebar(
        &self,
        ui: &mut Ui,
        theme: &Theme,
        snapshot: &HierarchySnapshot,
        state: &mut ViewState,
    ) -> Vec<ViewAction> {
        let mut actions = Vec::new();
        let area = ui.available_rect_before_wrap();
        ui.painter().rect_filled(area, 0.0, theme.panel);

        // Buttons occupy the first row; keyboard help gets a separate baseline below
        // them. Keeping both inside the old 44px header made them overlap at the normal
        // 344px sidebar width on Retina displays.
        let header = Rect::from_min_size(area.min, Vec2::new(area.width(), 50.0));
        ui.painter().text(
            header.min + Vec2::new(10.0, 6.0),
            Align2::LEFT_TOP,
            "WORKSPACES",
            FontId::new(11.0, egui::FontFamily::Monospace),
            theme.text_dim,
        );
        ui.painter().text(
            header.min + Vec2::new(10.0, 34.0),
            Align2::LEFT_TOP,
            "Space preview · Enter focus · ⌘Enter temporary",
            FontId::new(10.0, egui::FontFamily::Monospace),
            theme.text_faint,
        );
        ui.painter().hline(
            header.x_range(),
            header.max.y,
            Stroke::new(1.0, theme.border),
        );
        let action_rect = Rect::from_min_size(
            header.right_top() + Vec2::new(-208.0, 4.0),
            Vec2::new(198.0, 26.0),
        );
        ui.scope_builder(region(action_rect, "hierarchy-create-actions"), |ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("+ Workspace").clicked() {
                    state.workspace_draft = Some(WorkspaceDraft::new(false));
                }
                if ui
                    .add_enabled(
                        !snapshot.workspaces.is_empty(),
                        egui::Button::new("+ Session").small(),
                    )
                    .clicked()
                {
                    state.session_draft = self.new_session_draft(snapshot, state);
                }
            });
        });
        ui.advance_cursor_after_rect(header);

        let rows = visible_hierarchy_rows(snapshot, state);
        let tree_id = ui.id().with("workspace-session-process-tree");
        ui.ctx().accesskit_node_builder(tree_id, |node| {
            node.set_role(egui::accesskit::Role::Tree);
            node.set_label(format!(
                "Workspaces, sessions and processes, {} rows",
                rows.len()
            ));
        });

        if snapshot.workspaces.is_empty() {
            ui.vertical_centered(|ui| {
                ui.add_space(28.0);
                ui.label(RichText::new("No workspaces yet").color(theme.text_dim));
                ui.label(
                    RichText::new("Create a project root before starting a Session")
                        .color(theme.text_faint)
                        .small(),
                );
                ui.add_space(8.0);
                if ui.button("Create workspace").clicked() {
                    state.workspace_draft = Some(WorkspaceDraft::new(true));
                }
            });
            return actions;
        }

        let selected = effective_selection(snapshot, state);
        egui::ScrollArea::vertical()
            .id_salt("hierarchy-rows")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add_space(4.0);
                for row in &rows {
                    let key = row.key();
                    let is_selected = selected.as_ref() == Some(&key);
                    let expanded = row_is_expanded(snapshot, state, &key);
                    let focused_pane = match row {
                        HierarchyRow::Process { node, .. } => {
                            node.pane_bindings.iter().any(|binding| {
                                self.panes
                                    .iter()
                                    .any(|pane| pane.focused && pane.pane_id == binding.pane_id)
                            })
                        }
                        _ => false,
                    };
                    let active_session = match row {
                        HierarchyRow::Session { session, .. } => {
                            self.selected.as_ref() == Some(&session.session.id)
                        }
                        _ => false,
                    };
                    let response = hierarchy_row(
                        ui,
                        theme,
                        *row,
                        is_selected,
                        expanded,
                        focused_pane,
                        active_session,
                    );
                    if state.scroll_tree_to.as_ref() == Some(&key) {
                        response.scroll_to_me(Some(egui::Align::Center));
                        state.scroll_tree_to = None;
                    }
                    let mut accessible_expansion = None;
                    ui.input_mut(|input| {
                        input.consume_accesskit_action_requests(
                            response.id,
                            |request| match request.action {
                                egui::accesskit::Action::Expand => {
                                    accessible_expansion = Some(true);
                                    true
                                }
                                egui::accesskit::Action::Collapse => {
                                    accessible_expansion = Some(false);
                                    true
                                }
                                _ => false,
                            },
                        );
                    });
                    if let Some(expanded) = accessible_expansion {
                        state.tree_has_focus = true;
                        set_hierarchy_expanded(state, snapshot, key.clone(), expanded);
                    }
                    let caret_clicked = response.clicked()
                        && response.interact_pointer_pos().is_some_and(|pointer| {
                            pointer.x
                                <= response.rect.min.x + 10.0 + row.depth() as f32 * 14.0 + 15.0
                        })
                        && row.child_count() > 0;

                    if response.clicked() {
                        state.tree_has_focus = true;
                        response.request_focus();
                        if caret_clicked {
                            set_hierarchy_expanded(state, snapshot, key.clone(), !expanded);
                        } else {
                            set_hierarchy_selection(state, snapshot, key.clone());
                        }
                    }
                    if response.double_clicked() && !caret_clicked {
                        actions.extend(open_or_focus_hierarchy_row(state, snapshot, *row));
                    }
                    if let HierarchyRow::Process { node, .. } = row {
                        response.context_menu(|ui| {
                            if ui.button("Quick Preview").clicked() {
                                state.quick_preview =
                                    Some(HierarchyKey::process(node.node_id.clone()));
                                state.push_hierarchy_action(HierarchyAction::QuickPreview {
                                    surface_id: snapshot.tree_state.surface_id.clone(),
                                    session_id: node.session_id.clone(),
                                    node_id: node.node_id.clone(),
                                });
                                ui.close();
                            }
                            if ui.button("Open temporary pane").clicked() {
                                state.push_hierarchy_action(HierarchyAction::OpenTemporaryPane {
                                    surface_id: snapshot.tree_state.surface_id.clone(),
                                    session_id: node.session_id.clone(),
                                    node_id: node.node_id.clone(),
                                });
                                ui.close();
                            }
                            if !node.pane_bindings.is_empty()
                                && ui.button("Focus open pane").clicked()
                            {
                                state.push_hierarchy_action(HierarchyAction::FocusPaneForNode {
                                    surface_id: snapshot.tree_state.surface_id.clone(),
                                    session_id: node.session_id.clone(),
                                    node_id: node.node_id.clone(),
                                });
                                ui.close();
                            }
                            if ui.button("Show details").clicked() {
                                state.inspector_open = true;
                                ui.close();
                            }
                            let previews_hidden =
                                node.preview_visibility == PreviewVisibility::Hide;
                            if ui
                                .button(if previews_hidden {
                                    "Show activity preview"
                                } else {
                                    "Hide activity preview"
                                })
                                .clicked()
                            {
                                state.push_hierarchy_action(
                                    HierarchyAction::SetPreviewVisibility {
                                        session_id: node.session_id.clone(),
                                        node_id: node.node_id.clone(),
                                        visibility: if previews_hidden {
                                            PreviewVisibility::Show
                                        } else {
                                            PreviewVisibility::Hide
                                        },
                                    },
                                );
                                ui.close();
                            }
                            if !node.lifecycle.is_terminal() {
                                ui.separator();
                                let label = if node.is_agentic {
                                    "Stop Agent"
                                } else {
                                    "Stop Process"
                                };
                                if ui
                                    .button(RichText::new(label).color(theme.failure))
                                    .clicked()
                                {
                                    actions.push(ViewAction::TerminateNode {
                                        session_id: node.session_id.clone(),
                                        node_id: node.node_id.clone(),
                                    });
                                    ui.close();
                                }
                            }
                        });
                    }
                    if response.secondary_clicked() {
                        state.tree_has_focus = true;
                        response.request_focus();
                        set_hierarchy_selection(state, snapshot, key);
                        state.inspector_open = matches!(row, HierarchyRow::Process { .. });
                    }
                }
            });

        if state.tree_has_focus {
            let updated_rows = visible_hierarchy_rows(snapshot, state);
            actions.extend(handle_hierarchy_keyboard(
                ui,
                snapshot,
                state,
                &updated_rows,
            ));
        }
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

    /// Context for the active Session, deliberately a header rather than a tab strip.
    fn session_context_bar(
        &self,
        ui: &mut Ui,
        theme: &Theme,
        workspace: &WorkspaceTreeView,
        session: &SessionTreeView,
    ) -> Vec<ViewAction> {
        let mut actions = Vec::new();
        let area = ui.available_rect_before_wrap();
        ui.painter().rect_filled(area, 0.0, theme.raised);
        ui.painter()
            .hline(area.x_range(), area.max.y, Stroke::new(1.0, theme.border));

        let summary = &session.session;
        let session_needs_attention = summary.needs_user || summary.badge_count > 0;
        let (state_colour, glyph) = if session_needs_attention {
            (theme.attention, "◆")
        } else {
            theme.state_marker(summary.display_state)
        };
        let branch = summary
            .git_branch
            .as_deref()
            .or_else(|| {
                workspace
                    .checkouts
                    .iter()
                    .find(|checkout| checkout.id == summary.checkout_id)
                    .and_then(|checkout| checkout.branch.as_deref())
            })
            .unwrap_or("no branch");
        let title_clip = Rect::from_min_max(
            area.min,
            egui::pos2((area.max.x - 230.0).max(area.min.x), area.max.y),
        );
        let painter = ui.painter().with_clip_rect(title_clip);
        painter.text(
            area.min + Vec2::new(12.0, 5.0),
            Align2::LEFT_TOP,
            format!("{}  ›  {}", workspace.workspace.name, summary.name),
            theme.ui_font.clone(),
            theme.text,
        );
        painter.text(
            area.min + Vec2::new(12.0, 25.0),
            Align2::LEFT_TOP,
            format!(
                "{} · {branch} · {glyph} {} · {}",
                summary.mode.label(),
                summary.state_label,
                summary.cwd
            ),
            FontId::new(10.0, egui::FontFamily::Monospace),
            state_colour,
        );
        let owned_lease = workspace
            .write_lease
            .as_ref()
            .filter(|lease| lease.session_id == summary.id);
        let toolbar = Rect::from_min_size(
            area.right_top() + Vec2::new(-218.0, 7.0),
            Vec2::new(208.0, 32.0),
        );
        ui.scope_builder(region(toolbar, "session-layout-toolbar"), |ui| {
            ui.horizontal(|ui| {
                ui.menu_button("+ Pane", |ui| {
                    if ui.button("Shell · split right").clicked() {
                        actions.push(ViewAction::Run(Command::SplitHorizontal));
                        ui.close();
                    }
                    if ui.button("Shell · split below").clicked() {
                        actions.push(ViewAction::Run(Command::SplitVertical));
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Agent · split right").clicked() {
                        actions.push(ViewAction::Run(Command::LaunchAgent));
                        ui.close();
                    }
                });
                ui.menu_button("Layout", |ui| {
                    for (label, preset) in [
                        ("Balance current splits", LayoutPreset::Balanced),
                        ("Equal columns", LayoutPreset::Columns),
                        ("Equal rows", LayoutPreset::Rows),
                        ("Main pane left", LayoutPreset::MainLeft),
                        ("Grid", LayoutPreset::Grid),
                    ] {
                        if ui.button(label).clicked() {
                            actions.push(ViewAction::ApplyLayoutPreset(preset));
                            ui.close();
                        }
                    }
                    ui.separator();
                    if ui.button("Save current layout as preset").clicked() {
                        actions.push(ViewAction::Run(Command::SaveLayoutAsTemplate));
                        ui.close();
                    }
                    if let Some(lease) = owned_lease {
                        ui.separator();
                        if ui
                            .add_enabled(
                                summary.running_count == 0,
                                egui::Button::new("Release write lease"),
                            )
                            .clicked()
                        {
                            actions.push(ViewAction::ReleaseWorkspaceLease {
                                workspace_id: workspace.workspace.id.clone(),
                                lease_id: lease.id.clone(),
                                expected_generation: lease.generation,
                            });
                            ui.close();
                        }
                    }
                });
                ui.label(
                    RichText::new(if session_needs_attention {
                        "◆ YOUR TURN"
                    } else {
                        summary.mode.label()
                    })
                    .monospace()
                    .size(10.0)
                    .color(if session_needs_attention {
                        theme.attention
                    } else {
                        theme.text_dim
                    }),
                );
            });
        });

        let context_id = ui.id().with("active-session-context");
        ui.ctx().accesskit_node_builder(context_id, |node| {
            node.set_role(egui::accesskit::Role::Group);
            node.set_label(format!(
                "Active session, workspace {}, session {}, mode {}, {}, branch {}",
                workspace.workspace.name,
                summary.name,
                summary.mode.label(),
                summary.state_label,
                branch
            ));
        });
        actions
    }

    fn workspace_creator_overlay(
        &self,
        ui: &mut Ui,
        theme: &Theme,
        state: &mut ViewState,
        full: Rect,
    ) -> Vec<ViewAction> {
        let mut actions = Vec::new();
        let panel = Rect::from_center_size(
            full.center(),
            Vec2::new(560.0_f32.min(full.width() - 32.0), 320.0),
        );
        ui.painter()
            .rect_filled(full, 0.0, Color32::from_black_alpha(150));
        ui.painter().rect_filled(panel, 0.0, theme.panel);
        ui.painter().rect_stroke(
            panel,
            0.0,
            Stroke::new(1.0, theme.border),
            egui::StrokeKind::Outside,
        );
        ui.scope_builder(region(panel.shrink(16.0), "workspace-creator"), |ui| {
            ui.ctx().accesskit_node_builder(ui.id(), |node| {
                node.set_role(egui::accesskit::Role::Dialog);
                node.set_label("Create Workspace");
                node.set_modal();
            });
            ui.label(RichText::new("CREATE WORKSPACE").color(theme.text).strong());
            ui.label(
                RichText::new("Choose the existing project directory Turn will supervise.")
                    .color(theme.text_dim)
                    .small(),
            );
            ui.add_space(10.0);
            let Some(draft) = state.workspace_draft.as_mut() else {
                return;
            };
            let root_label = ui.label(
                RichText::new("Project folder")
                    .color(theme.text_dim)
                    .small(),
            );
            let mut browse_clicked = false;
            let root_field = ui
                .horizontal(|ui| {
                    let button_width = if state.workspace_picker_pending {
                        92.0
                    } else {
                        76.0
                    };
                    let field_width = (ui.available_width()
                        - button_width
                        - ui.spacing().item_spacing.x)
                        .max(80.0);
                    let field = ui
                        .add_sized(
                            [field_width, 24.0],
                            egui::TextEdit::singleline(&mut draft.root)
                                .hint_text("/Users/you/projects/my-project"),
                        )
                        .labelled_by(root_label.id);
                    browse_clicked = ui
                        .add_enabled(
                            !state.workspace_picker_pending && !draft.submitting,
                            egui::Button::new(if state.workspace_picker_pending {
                                "Choosing…"
                            } else {
                                "Browse…"
                            }),
                        )
                        .on_hover_text("Choose an existing project folder")
                        .clicked();
                    field
                })
                .inner;
            if root_field.changed() {
                draft.refresh_derived_name();
            }
            let submit_from_root =
                root_field.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter));
            if browse_clicked {
                actions.push(ViewAction::ChooseWorkspaceDirectory);
            }
            let name_label = ui.label(RichText::new("Name").color(theme.text_dim).small());
            let name_field = ui
                .add(egui::TextEdit::singleline(&mut draft.name).desired_width(f32::INFINITY))
                .labelled_by(name_label.id);
            if draft.request_name_focus {
                name_field.request_focus();
                draft.request_name_focus = false;
            }
            if name_field.changed() {
                draft.name_is_derived = false;
            }
            let submit_from_name =
                name_field.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter));
            let connected = matches!(self.connection, Some(ConnectionState::Connected { .. }));
            let root_is_absolute = std::path::Path::new(draft.root.trim()).is_absolute();
            let valid = connected
                && !draft.submitting
                && !state.workspace_picker_pending
                && !draft.name.trim().is_empty()
                && root_is_absolute;
            if !root_is_absolute {
                ui.label(
                    RichText::new(
                        "Choose an absolute project folder so every process resolves the same checkout.",
                    )
                    .color(theme.attention)
                    .small(),
                );
            }
            if !connected {
                ui.label(
                    RichText::new(
                        "Waiting for the Turn daemon — nothing will be queued or created yet.",
                    )
                    .color(theme.failure)
                    .small(),
                );
            }
            if let Some(error) = &draft.error {
                ui.label(RichText::new(error).color(theme.failure).small());
            }
            if draft.continue_to_session {
                ui.label(
                    RichText::new("After this, choose the first Session and its template.")
                        .color(theme.text_faint)
                        .small(),
                );
            }
            let mut submit_clicked = false;
            ui.with_layout(egui::Layout::bottom_up(egui::Align::RIGHT), |ui| {
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!draft.submitting, egui::Button::new("Cancel"))
                        .clicked()
                    {
                        actions.push(ViewAction::CloseOverlay);
                    }
                    submit_clicked = ui
                        .add_enabled(
                            valid,
                            egui::Button::new(if draft.submitting {
                                "Creating…"
                            } else if draft.continue_to_session {
                                "Create and continue"
                            } else {
                                "Create workspace"
                            }),
                        )
                        .clicked();
                });
            });
            if valid && (submit_clicked || submit_from_name || submit_from_root) {
                draft.submitting = true;
                draft.error = None;
                actions.push(ViewAction::CreateWorkspace {
                    name: draft.name.trim().to_string(),
                    root: draft.root.trim().to_string(),
                    continue_to_session: draft.continue_to_session,
                });
            }
        });
        actions
    }

    fn session_creator_overlay(
        &self,
        ui: &mut Ui,
        theme: &Theme,
        state: &mut ViewState,
        full: Rect,
    ) -> Vec<ViewAction> {
        let mut actions = Vec::new();
        let panel = Rect::from_center_size(
            full.center(),
            Vec2::new(
                620.0_f32.min((full.width() - 32.0).max(280.0)),
                410.0_f32.min((full.height() - 32.0).max(300.0)),
            ),
        );
        ui.painter()
            .rect_filled(full, 0.0, Color32::from_black_alpha(150));
        ui.painter().rect_filled(panel, 10.0, theme.panel);
        ui.painter().rect_stroke(
            panel,
            10.0,
            Stroke::new(1.0, theme.border),
            egui::StrokeKind::Outside,
        );
        ui.scope_builder(region(panel.shrink(20.0), "session-creator"), |ui| {
            ui.ctx().accesskit_node_builder(ui.id(), |node| {
                node.set_role(egui::accesskit::Role::Dialog);
                node.set_label("New Session");
                node.set_modal();
            });
            ui.label(
                RichText::new("New session")
                    .size(21.0)
                    .color(theme.text)
                    .strong(),
            );
            ui.label(
                RichText::new("Choose a workspace and a reusable layout. The name is optional.")
                .color(theme.text_dim)
                .small(),
            );
            ui.add_space(14.0);
            let Some(draft) = state.session_draft.as_mut() else {
                return;
            };

            if !self
                .workspaces
                .iter()
                .any(|workspace| workspace.id == draft.workspace_id)
            {
                if let Some(workspace) = self.workspaces.first() {
                    draft.workspace_id = workspace.id.clone();
                }
            }
            if draft.template_id.as_ref().is_none_or(|id| {
                !self.templates.iter().any(|template| &template.id == id)
            }) {
                draft.template_id = self.preferred_template_id(&draft.workspace_id);
            }

            let workspace_label =
                ui.label(RichText::new("Workspace").color(theme.text_dim).small());
            let workspace_name = self
                .workspaces
                .iter()
                .find(|workspace| workspace.id == draft.workspace_id)
                .map(|workspace| workspace.name.as_str())
                .unwrap_or("No workspace available");
            let workspace_combo = egui::ComboBox::from_id_salt("new-session-workspace")
                .selected_text(workspace_name)
                .width(ui.available_width())
                .show_ui(ui, |ui| {
                    for workspace in self.workspaces {
                        ui.selectable_value(
                            &mut draft.workspace_id,
                            workspace.id.clone(),
                            format!("{}  ·  {}", workspace.name, workspace.root),
                        );
                    }
                });
            workspace_combo.response.labelled_by(workspace_label.id);

            let template_label = ui.label(
                RichText::new("Layout preset")
                    .color(theme.text_dim)
                    .small(),
            );
            let template_name = draft
                .template_id
                .as_ref()
                .and_then(|id| self.templates.iter().find(|template| &template.id == id))
                .map(|template| template.name.as_str())
                .unwrap_or("Templates are loading…");
            ui.horizontal(|ui| {
                let combo_width = (ui.available_width() - 128.0).max(140.0);
                let template_combo = egui::ComboBox::from_id_salt("new-session-template")
                    .selected_text(template_name)
                    .width(combo_width)
                    .show_ui(ui, |ui| {
                        for template in self.templates {
                            ui.selectable_value(
                                &mut draft.template_id,
                                Some(template.id.clone()),
                                format!("{}  ·  {} cells", template.name, template.pane_count),
                            );
                        }
                    });
                template_combo.response.labelled_by(template_label.id);
                if ui.button("New layout…").clicked() {
                    actions.push(ViewAction::OpenLayoutEditor(
                        LayoutEditorOrigin::NewSession,
                    ));
                }
            });

            let name_label = ui.label(
                RichText::new("Session name (optional)")
                    .color(theme.text_dim)
                    .small(),
            );
            let name_field = ui
                .add(
                    egui::TextEdit::singleline(&mut draft.name)
                        .desired_width(f32::INFINITY)
                        .hint_text("Turn will choose a name if left blank"),
                )
                .labelled_by(name_label.id);
            if draft.request_name_focus {
                name_field.request_focus();
                draft.request_name_focus = false;
            }
            let submit_from_name = name_field.lost_focus()
                && ui.input(|input| input.key_pressed(Key::Enter));
            let task_label = ui.label(
                RichText::new("Task note (optional)")
                    .color(theme.text_dim)
                    .small(),
            );
            ui.add(
                egui::TextEdit::multiline(&mut draft.task)
                    .desired_width(f32::INFINITY)
                    .desired_rows(2),
            )
            .labelled_by(task_label.id);

            if let Some(template) = draft
                .template_id
                .as_ref()
                .and_then(|id| self.templates.iter().find(|template| &template.id == id))
            {
                let commands = if template.commands.is_empty() {
                    format!("{} default shell cell(s)", template.pane_count)
                } else {
                    template.commands.join(" · ")
                };
                ui.label(
                    RichText::new(format!("Will start: {commands}"))
                        .monospace()
                        .color(theme.text_faint)
                        .small(),
                );
            }
            ui.label(
                RichText::new(
                    "Uses the main checkout. If it already has a writer, Turn will offer focus, read-only or an isolated worktree.",
                )
                .color(theme.text_faint)
                .small(),
            );

            let connected = matches!(self.connection, Some(ConnectionState::Connected { .. }));
            if !connected {
                ui.label(
                    RichText::new("Waiting for the Turn daemon — this request will not be queued.")
                        .color(theme.failure)
                        .small(),
                );
            }
            if let Some(error) = &draft.error {
                ui.label(RichText::new(error).color(theme.failure).small());
            }
            let valid = connected
                && !draft.submitting
                && draft.template_id.is_some()
                && self
                    .workspaces
                    .iter()
                    .any(|workspace| workspace.id == draft.workspace_id);
            let mut submit_clicked = false;
            ui.with_layout(egui::Layout::bottom_up(egui::Align::RIGHT), |ui| {
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!draft.submitting, egui::Button::new("Cancel"))
                        .clicked()
                    {
                        actions.push(ViewAction::CloseOverlay);
                    }
                    submit_clicked = ui
                        .add_enabled(
                            valid,
                            egui::Button::new(if draft.submitting {
                                "Creating…"
                            } else {
                                "Create session"
                            })
                            .fill(theme.running)
                            .stroke(Stroke::NONE),
                        )
                        .clicked();
                });
            });
            if valid && (submit_clicked || submit_from_name) {
                let template_id = draft.template_id.clone().expect("validated above");
                draft.submitting = true;
                draft.error = None;
                actions.push(ViewAction::CreateSessionFromTemplate {
                    workspace_id: draft.workspace_id.clone(),
                    template_id,
                    name: draft.name.trim().to_string(),
                    task: (!draft.task.trim().is_empty())
                        .then(|| draft.task.trim().to_string()),
                });
            }
        });
        actions
    }

    fn layout_editor_overlay(
        &self,
        ui: &mut Ui,
        theme: &Theme,
        state: &mut ViewState,
        full: Rect,
    ) -> Vec<ViewAction> {
        let mut actions = Vec::new();
        let panel = Rect::from_center_size(
            full.center(),
            Vec2::new(
                860.0_f32.min((full.width() - 32.0).max(320.0)),
                650.0_f32.min((full.height() - 32.0).max(420.0)),
            ),
        );
        ui.painter()
            .rect_filled(full, 0.0, Color32::from_black_alpha(165));
        ui.painter().rect_filled(panel, 10.0, theme.panel);
        ui.painter().rect_stroke(
            panel,
            10.0,
            Stroke::new(1.0, theme.border),
            egui::StrokeKind::Outside,
        );

        ui.scope_builder(region(panel.shrink(20.0), "layout-editor"), |ui| {
            ui.ctx().accesskit_node_builder(ui.id(), |node| {
                node.set_role(egui::accesskit::Role::Dialog);
                node.set_label("Layout editor");
                node.set_modal();
            });
            let Some(draft) = state.layout_draft.as_mut() else {
                return;
            };

            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(
                        RichText::new("Create layout preset")
                            .size(21.0)
                            .color(theme.text)
                            .strong(),
                    );
                    ui.label(
                        RichText::new(
                            "Add rows or columns, choose a program per cell, drag cells to swap, and drag dividers to resize.",
                        )
                        .color(theme.text_dim)
                        .small(),
                    );
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
                    if ui
                        .add_enabled(!draft.submitting, egui::Button::new("Back"))
                        .clicked()
                    {
                        actions.push(ViewAction::CloseLayoutEditor);
                    }
                });
            });
            ui.add_space(12.0);

            ui.horizontal(|ui| {
                if ui.button("+ Column").clicked() {
                    draft.split_selected(Direction::Horizontal);
                }
                if ui.button("+ Row").clicked() {
                    draft.split_selected(Direction::Vertical);
                }
                if ui.button("Balance").clicked() {
                    draft.layout.apply_preset(LayoutPreset::Balanced);
                }
                if ui
                    .add_enabled(draft.layout.pane_count() > 1, egui::Button::new("Remove cell"))
                    .clicked()
                {
                    draft.remove_selected();
                }
                ui.separator();
                ui.label(
                    RichText::new(format!("{} cells", draft.layout.pane_count()))
                        .color(theme.text_faint)
                        .small(),
                );
            });
            ui.add_space(8.0);

            let canvas_height = (panel.height() * 0.42).clamp(210.0, 290.0);
            let (canvas, _) = ui.allocate_exact_size(
                Vec2::new(ui.available_width(), canvas_height),
                Sense::hover(),
            );
            ui.painter().rect_filled(canvas, 8.0, theme.background);
            ui.painter().rect_stroke(
                canvas,
                8.0,
                Stroke::new(1.0, theme.border),
                egui::StrokeKind::Inside,
            );
            let arrangement = panes::arrange(&draft.layout, canvas.shrink(8.0));
            for placed in &arrangement.panes {
                let selected = draft.selected == placed.pane_id;
                let response = ui.interact(
                    placed.rect,
                    ui.id().with(("layout-cell", placed.pane_id.as_str())),
                    Sense::click_and_drag(),
                );
                ui.painter().rect_filled(
                    placed.rect,
                    6.0,
                    if selected {
                        theme.selection
                    } else if response.hovered() {
                        theme.raised
                    } else {
                        theme.panel
                    },
                );
                ui.painter().rect_stroke(
                    placed.rect,
                    6.0,
                    Stroke::new(
                        if selected { 2.0 } else { 1.0 },
                        if selected { theme.running } else { theme.border },
                    ),
                    egui::StrokeKind::Inside,
                );
                ui.painter().text(
                    placed.rect.center() - Vec2::new(0.0, 8.0),
                    Align2::CENTER_CENTER,
                    draft.cell_label(&placed.pane_id),
                    theme.ui_font.clone(),
                    theme.text,
                );
                ui.painter().text(
                    placed.rect.center() + Vec2::new(0.0, 11.0),
                    Align2::CENTER_CENTER,
                    "drag to move · select to configure",
                    FontId::new(10.0, egui::FontFamily::Proportional),
                    theme.text_faint,
                );
                if response.clicked() {
                    draft.selected = placed.pane_id.clone();
                    draft.layout.active = Some(placed.pane_id.clone());
                }
                if response.drag_started() {
                    draft.dragged_pane = Some(placed.pane_id.clone());
                }
            }

            let released = ui.input(|input| input.pointer.any_released());
            if released {
                let target = ui
                    .input(|input| input.pointer.interact_pos())
                    .and_then(|position| arrangement.pane_at(position))
                    .map(|pane| pane.pane_id.clone());
                if let (Some(source), Some(target)) = (draft.dragged_pane.take(), target) {
                    if source != target && draft.layout.swap(&source, &target) {
                        draft.selected = source;
                    }
                }
            }

            for divider in &arrangement.dividers {
                let id = ui.id().with((
                    "layout-editor-divider",
                    divider.before.as_str(),
                    divider.after.as_str(),
                ));
                let response = ui.interact(divider.grab_rect(), id, Sense::click_and_drag());
                let active = response.hovered() || response.dragged();
                ui.painter().rect_filled(
                    divider.rect,
                    1.0,
                    if active { theme.running } else { theme.border },
                );
                if active {
                    ui.ctx().set_cursor_icon(match divider.direction {
                        Direction::Horizontal => egui::CursorIcon::ResizeHorizontal,
                        Direction::Vertical => egui::CursorIcon::ResizeVertical,
                    });
                }
                if response.double_clicked() {
                    draft
                        .layout
                        .equalize_divider(&divider.before, &divider.after);
                } else if response.dragged() {
                    if let Some(delta) = divider.fraction_for_drag(response.drag_delta()) {
                        draft
                            .layout
                            .resize_divider(&divider.before, &divider.after, delta);
                    }
                }
            }

            ui.add_space(12.0);
            ui.label(
                RichText::new(format!(
                    "Selected cell · {}",
                    draft.cell_label(&draft.selected)
                ))
                .color(theme.text)
                .strong(),
            );
            if let Some(command) = draft.cells.get_mut(&draft.selected) {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Quick choice").color(theme.text_dim).small());
                    for (label, program) in [
                        ("Shell", ""),
                        ("Claude", "claude"),
                        ("Codex", "codex"),
                        ("Gemini", "gemini"),
                    ] {
                        if ui.selectable_label(command.program == program, label).clicked() {
                            command.program = program.to_string();
                            command.arguments.clear();
                        }
                    }
                });
                ui.columns(2, |columns| {
                    columns[0].label(
                        RichText::new("Program (blank = workspace shell)")
                            .color(theme.text_dim)
                            .small(),
                    );
                    columns[0].add(
                        egui::TextEdit::singleline(&mut command.program)
                            .desired_width(f32::INFINITY)
                            .hint_text("npm"),
                    );
                    columns[1].label(
                        RichText::new("Arguments")
                            .color(theme.text_dim)
                            .small(),
                    );
                    columns[1].add(
                        egui::TextEdit::singleline(&mut command.arguments)
                            .desired_width(f32::INFINITY)
                            .hint_text("run dev -- --watch"),
                    );
                });
                ui.label(
                    RichText::new(
                        "Arguments support quotes and become argv. Turn never evaluates them as an implicit shell script.",
                    )
                    .color(theme.text_faint)
                    .small(),
                );
            }

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut draft.name)
                        .desired_width((ui.available_width() - 150.0).max(160.0))
                        .hint_text("Layout name"),
                );
                let connected = matches!(self.connection, Some(ConnectionState::Connected { .. }));
                let valid = connected && !draft.submitting && !draft.name.trim().is_empty();
                if ui
                    .add_enabled(
                        valid,
                        egui::Button::new(if draft.submitting {
                            "Saving…"
                        } else {
                            "Save layout"
                        })
                        .fill(theme.running)
                        .stroke(Stroke::NONE),
                    )
                    .clicked()
                {
                    match draft.materialized_layout() {
                        Ok(layout) => {
                            draft.submitting = true;
                            draft.error = None;
                            actions.push(ViewAction::CreateLayoutTemplate {
                                name: draft.name.trim().to_string(),
                                layout,
                            });
                        }
                        Err(error) => draft.error = Some(error),
                    }
                }
            });
            if let Some(error) = &draft.error {
                ui.label(RichText::new(error).color(theme.failure).small());
            } else if !matches!(self.connection, Some(ConnectionState::Connected { .. })) {
                ui.label(
                    RichText::new("Waiting for the Turn daemon before this layout can be saved.")
                        .color(theme.failure)
                        .small(),
                );
            }
        });
        actions
    }

    /// A bounded, explicit queue overlay. The hierarchy remains the sole persistent
    /// navigator; this view exists only while the user is triaging demands.
    fn attention_queue_overlay(&self, ui: &mut Ui, theme: &Theme, full: Rect) -> Vec<ViewAction> {
        const SNOOZE_MS: i64 = 10 * 60 * 1_000;
        const MUTE_MS: i64 = 60 * 60 * 1_000;

        let mut actions = Vec::new();
        let width = 680.0_f32.min((full.width() - 40.0).max(240.0));
        let content_height = 128.0 + self.queue.len().min(4) as f32 * 88.0;
        let height = content_height
            .clamp(220.0, 520.0)
            .min((full.height() - 80.0).max(220.0));
        let panel = Rect::from_center_size(full.center(), Vec2::new(width, height));
        ui.painter()
            .rect_filled(full, 0.0, Color32::from_black_alpha(150));
        ui.painter().rect_filled(panel, 0.0, theme.panel);
        ui.painter().rect_stroke(
            panel,
            0.0,
            Stroke::new(1.0, theme.border),
            egui::StrokeKind::Outside,
        );

        ui.scope_builder(region(panel.shrink(14.0), "attention-queue"), |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("ATTENTION QUEUE").color(theme.text).strong());
                ui.label(
                    RichText::new(format!("{} pending", self.queue.len()))
                        .color(theme.attention)
                        .small(),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Close").clicked() {
                        actions.push(ViewAction::CloseOverlay);
                    }
                });
            });
            ui.label(
                RichText::new(
                    "Demands stay unresolved until the agent resumes or you dismiss them explicitly.",
                )
                .color(theme.text_faint)
                .small(),
            );
            ui.add_space(8.0);

            if self.queue.is_empty() {
                ui.vertical_centered(|ui| {
                    ui.add_space(36.0);
                    ui.label(RichText::new("Nothing needs you").color(theme.text_dim));
                });
                return;
            }

            egui::ScrollArea::vertical()
                .id_salt("attention-queue-rows")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for item in &self.queue {
                        ui.group(|ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(if item.actionable { "◆" } else { "○" })
                                        .monospace()
                                        .color(if item.actionable {
                                            theme.attention
                                        } else {
                                            theme.text_faint
                                        }),
                                );
                                ui.label(
                                    RichText::new(&item.session_name)
                                        .color(theme.text)
                                        .strong(),
                                );
                                ui.label(
                                    RichText::new(item.reason_label().to_uppercase())
                                        .color(if item.actionable {
                                            theme.attention
                                        } else {
                                            theme.text_faint
                                        })
                                        .small(),
                                );
                                if item.provisional {
                                    ui.label(
                                        RichText::new("inferred")
                                            .color(theme.provisional)
                                            .small(),
                                    );
                                }
                                if !item.actionable {
                                    ui.label(
                                        RichText::new("snoozed")
                                            .color(theme.text_faint)
                                            .small(),
                                    );
                                }
                            });
                            if let Some(summary) = &item.summary {
                                ui.label(RichText::new(summary).color(theme.text_dim));
                            }
                            ui.horizontal(|ui| {
                                if ui
                                    .add_enabled(item.actionable, egui::Button::new("Open"))
                                    .clicked()
                                {
                                    actions.push(ViewAction::GotoAttention(
                                        item.attention_id.clone(),
                                    ));
                                }
                                if ui
                                    .add_enabled(
                                        item.actionable,
                                        egui::Button::new("Snooze 10 min"),
                                    )
                                    .clicked()
                                {
                                    actions.push(ViewAction::SnoozeAttention {
                                        attention_id: item.attention_id.clone(),
                                        until_ms: self.now_ms.saturating_add(SNOOZE_MS),
                                    });
                                }
                                if ui.button("Mute session 1h").clicked() {
                                    actions.push(ViewAction::MuteAttentionSession {
                                        session_id: item.session_id.clone(),
                                        until_ms: Some(self.now_ms.saturating_add(MUTE_MS)),
                                    });
                                }
                                if ui.button("Dismiss").clicked() {
                                    actions.push(ViewAction::DismissAttention(
                                        item.attention_id.clone(),
                                    ));
                                }
                            });
                        });
                        ui.add_space(6.0);
                    }
                });
        });
        actions
    }

    fn pane_status_bar(&self, ui: &mut Ui, theme: &Theme, session: Option<&SessionTreeView>) {
        let area = ui.available_rect_before_wrap();
        ui.painter().rect_filled(area, 0.0, theme.panel);
        ui.painter()
            .hline(area.x_range(), area.min.y, Stroke::new(1.0, theme.border));
        let focused = self
            .panes
            .iter()
            .find(|pane| pane.focused)
            .map(|pane| pane.title.as_str())
            .unwrap_or("no pane focused");
        ui.painter().text(
            area.left_center() + Vec2::new(9.0, 0.0),
            Align2::LEFT_CENTER,
            format!("FOCUS · {focused}"),
            FontId::new(10.0, egui::FontFamily::Monospace),
            if self.panes.iter().any(|pane| pane.focused) {
                theme.running
            } else {
                theme.text_faint
            },
        );
        if let Some(session) = session {
            ui.painter().text(
                area.right_center() + Vec2::new(-9.0, 0.0),
                Align2::RIGHT_CENTER,
                format!(
                    "{} · {} running · {} panes",
                    session.session.mode.label(),
                    session.session.running_count,
                    session.session.pane_count
                ),
                FontId::new(10.0, egui::FontFamily::Monospace),
                theme.text_faint,
            );
        }
    }

    /// The panes of the selected session, with their dividers.
    fn pane_area(
        &self,
        ui: &mut Ui,
        theme: &Theme,
        keymap: &Keymap,
        state: &mut ViewState,
        hierarchy: Option<&HierarchySnapshot>,
    ) -> Vec<ViewAction> {
        let mut actions = Vec::new();
        let area = ui.available_rect_before_wrap();

        let Some(layout) = &self.layout else {
            // The chord, spelled out. "Press the palette shortcut" is useless to somebody
            // who does not know it, and the keymap is right here — including a chord the
            // user chose themselves.
            let content = Rect::from_center_size(
                area.center(),
                Vec2::new(440.0_f32.min(area.width()), 150.0_f32.min(area.height())),
            );
            ui.scope_builder(region(content, "empty-primary-action"), |ui| {
                ui.vertical_centered(|ui| {
                    if self.workspaces.is_empty() {
                        ui.heading(RichText::new("Start with a workspace").color(theme.text));
                        ui.label(
                            RichText::new("Choose the project directory Turn will supervise.")
                                .color(theme.text_dim),
                        );
                        ui.add_space(10.0);
                        if ui.button("Create workspace").clicked() {
                            state.workspace_draft = Some(WorkspaceDraft::new(true));
                        }
                    } else if self.sessions.is_empty() {
                        ui.heading(RichText::new("Create the first session").color(theme.text));
                        let shortcut = keymap
                            .chord_for(Command::NewSession)
                            .map(|chord| chord.describe(keymap.platform()))
                            .unwrap_or_else(|| "the New Session command".to_string());
                        ui.label(
                            RichText::new(format!(
                                "Pick a task and template here, or press {shortcut}."
                            ))
                            .color(theme.text_dim),
                        );
                        ui.add_space(10.0);
                        if ui.button("New session").clicked() {
                            if let Some(snapshot) = hierarchy {
                                state.session_draft = self.new_session_draft(snapshot, state);
                            }
                        }
                    } else {
                        ui.label(
                            RichText::new("Select a session in the workspace tree")
                                .color(theme.text_dim),
                        );
                    }
                });
            });
            return actions;
        };

        let arrangement = panes::arrange(layout, area);
        // Persisted pane focus and the window's keyboard lease are different things.
        // Keep the visual focus in place behind a sheet, but never let that sheet's
        // Text/Paste/Key events reach a PTY.
        let accepts_terminal_input = !state.is_sensitive() && self.write_conflict.is_none();
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
                        accepts_input: focused && accepts_terminal_input,
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

    /// Draws the one surface-scoped temporary Pane over the explicit Layout. Its
    /// geometry is intentionally absent from `Layout`; replacing or closing it cannot
    /// move a divider or stop the underlying Agent.
    fn temporary_pane_overlay(
        &self,
        ui: &mut Ui,
        theme: &Theme,
        state: &mut ViewState,
        temporary: &TemporaryPaneContent<'_>,
    ) -> Vec<ViewAction> {
        let mut actions = Vec::new();
        let area = ui.available_rect_before_wrap();
        let width = 620.0_f32.min((area.width() * 0.68).max(320.0));
        let panel = Rect::from_min_size(
            area.right_top() - Vec2::new(width, 0.0),
            Vec2::new(width.min(area.width()), area.height()),
        );
        ui.painter().rect_filled(panel, 0.0, theme.background);
        ui.painter().rect_stroke(
            panel,
            0.0,
            Stroke::new(1.0, theme.attention),
            egui::StrokeKind::Inside,
        );

        let header = Rect::from_min_size(panel.min, Vec2::new(panel.width(), 32.0));
        ui.painter().rect_filled(header, 0.0, theme.raised);
        ui.painter().hline(
            header.x_range(),
            header.max.y,
            Stroke::new(1.0, theme.border),
        );
        let title = temporary
            .node
            .map(process_title)
            .unwrap_or("Process preview");
        ui.painter().text(
            header.left_center() + Vec2::new(10.0, 0.0),
            Align2::LEFT_CENTER,
            format!("TEMPORARY PANE  ·  {title}"),
            FontId::new(11.0, egui::FontFamily::Monospace),
            theme.text,
        );
        let close_rect = Rect::from_min_size(
            header.right_top() + Vec2::new(-58.0, 4.0),
            Vec2::new(52.0, 24.0),
        );
        if ui.put(close_rect, egui::Button::new("Close")).clicked() {
            actions.push(ViewAction::CloseTemporaryPane {
                session_id: temporary.pane.binding.session_id.clone(),
                pane_id: temporary.pane.binding.pane_id.clone(),
            });
        }

        let body = Rect::from_min_max(header.left_bottom(), panel.max).shrink(10.0);
        match (&temporary.pane.capability, temporary.grid) {
            (NodePaneCapability::Terminal { .. }, Some(grid)) => {
                let options = PaneOptions {
                    focused: true,
                    accepts_input: !state.is_sensitive() && self.write_conflict.is_none(),
                    now_ms: self.now_ms,
                    scrolled: false,
                    history_complete: true,
                };
                let pane_id = temporary.pane.binding.pane_id.clone();
                let interaction = state.pane(&pane_id);
                let id = ui.id().with(("temporary-terminal", pane_id.as_str()));
                for action in terminal::show(ui, theme, body, grid, interaction, options, id) {
                    actions.push(ViewAction::Pane {
                        pane_id: pane_id.clone(),
                        action,
                    });
                }
            }
            _ => {
                ui.scope_builder(region(body, "temporary-preview-details"), |ui| {
                    if let Some(node) = temporary.node {
                        let (colour, glyph) = theme.state_marker(node.display_state);
                        ui.horizontal_wrapped(|ui| {
                            ui.label(
                                RichText::new(format!("{glyph} {}", node.state_label))
                                    .monospace()
                                    .color(colour),
                            );
                            ui.label(
                                RichText::new(node_kind_label(node.kind))
                                    .monospace()
                                    .color(theme.text_dim),
                            );
                            if node.relationship_is_provisional {
                                ui.label(
                                    RichText::new("relationship inferred")
                                        .monospace()
                                        .color(theme.provisional),
                                );
                            }
                        });
                        if let Some(task) = node
                            .agent
                            .as_ref()
                            .and_then(|agent| agent.current_task.as_deref())
                        {
                            ui.label(RichText::new(task).color(theme.text));
                        }
                        ui.label(
                            RichText::new(format!(
                                "last activity {} · Esc closes this view",
                                format_duration(node.runtime_ms)
                            ))
                            .monospace()
                            .color(theme.text_faint)
                            .small(),
                        );
                    }
                    ui.separator();
                    ui.label(
                        RichText::new("STABLE ACTIVITY")
                            .monospace()
                            .color(theme.text_dim)
                            .small(),
                    );
                    egui::ScrollArea::vertical()
                        .id_salt("temporary-preview-history")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            if temporary.previews.is_empty() {
                                if let Some(preview) = temporary.node.and_then(visible_preview) {
                                    ui.label(
                                        RichText::new(&preview.normalized_text).color(theme.text),
                                    );
                                } else {
                                    ui.label(
                                        RichText::new("No stable, safe activity yet")
                                            .color(theme.text_faint),
                                    );
                                }
                            }
                            for preview in temporary.previews {
                                ui.group(|ui| {
                                    ui.label(
                                        RichText::new(&preview.normalized_text).color(theme.text),
                                    );
                                    ui.label(
                                        RichText::new(format!(
                                            "{} · {}{}",
                                            preview_source_label(preview.source),
                                            preview.confidence.label(),
                                            if preview.redacted { " · redacted" } else { "" }
                                        ))
                                        .monospace()
                                        .color(theme.text_faint)
                                        .small(),
                                    );
                                });
                            }
                        });
                });
            }
        }

        let id = ui.id().with("temporary-pane-accessibility");
        ui.ctx().accesskit_node_builder(id, |node| {
            node.set_role(egui::accesskit::Role::Group);
            node.set_label(format!(
                "Temporary pane for {title}; closing keeps the process alive"
            ));
        });
        actions
    }

    fn process_inspector(
        &self,
        ui: &mut Ui,
        theme: &Theme,
        node: &TreeNodeView,
        state: &mut ViewState,
    ) {
        let area = ui.available_rect_before_wrap();
        ui.painter().rect_filled(area, 0.0, theme.panel);
        ui.add_space(7.0);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(
                RichText::new("PROCESS DETAILS")
                    .color(theme.text_dim)
                    .small(),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("Close").clicked() {
                    state.inspector_open = false;
                }
            });
        });
        ui.separator();

        egui::ScrollArea::vertical()
            .id_salt("process-inspector-scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add_space(4.0);
                ui.label(
                    RichText::new(process_title(node))
                        .color(theme.text)
                        .strong(),
                );
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new(node_kind_label(node.kind))
                            .monospace()
                            .color(theme.text_dim)
                            .small(),
                    );
                    let (state_colour, glyph) = theme.state_marker(node.display_state);
                    ui.label(
                        RichText::new(format!("{glyph} {}", node.state_label))
                            .monospace()
                            .color(state_colour)
                            .small(),
                    );
                    if node.relationship_is_provisional {
                        ui.label(
                            RichText::new("inferred")
                                .monospace()
                                .color(theme.provisional)
                                .small(),
                        );
                    }
                });

                inspector_section(ui, theme, "ACTIVITY");
                match visible_preview(node) {
                    Some(preview) => {
                        ui.label(RichText::new(&preview.normalized_text).color(theme.text));
                        ui.label(
                            RichText::new(format!(
                                "{} · {} · {}",
                                preview_source_label(preview.source),
                                preview.confidence.label(),
                                if preview.stable { "stable" } else { "updating" }
                            ))
                            .monospace()
                            .color(theme.text_faint)
                            .small(),
                        );
                    }
                    None => {
                        ui.label(
                            RichText::new("No safe activity preview")
                                .color(theme.text_faint)
                                .small(),
                        );
                    }
                }
                let (preview_label, visibility) =
                    if matches!(node.preview_visibility, PreviewVisibility::Hide) {
                        ("Show activity preview", PreviewVisibility::Inherit)
                    } else {
                        ("Hide activity preview", PreviewVisibility::Hide)
                    };
                if ui.small_button(preview_label).clicked() {
                    if matches!(visibility, PreviewVisibility::Hide) {
                        state.quick_preview = None;
                    }
                    state.push_hierarchy_action(HierarchyAction::SetPreviewVisibility {
                        session_id: node.session_id.clone(),
                        node_id: node.node_id.clone(),
                        visibility,
                    });
                }

                inspector_section(ui, theme, "RELATIONSHIP");
                ui.label(
                    RichText::new(format!(
                        "{} · {}",
                        relationship_label(node.relationship.kind),
                        node.relationship.confidence.label()
                    ))
                    .monospace()
                    .color(if node.relationship_is_provisional {
                        theme.provisional
                    } else {
                        theme.text_dim
                    }),
                );
                if let Some(parent) = &node.parent {
                    ui.label(
                        RichText::new(format!("parent {}", parent.as_str()))
                            .monospace()
                            .color(theme.text_faint)
                            .small(),
                    );
                }

                inspector_section(ui, theme, "PROCESS");
                inspector_value(ui, theme, "command", &node.command);
                inspector_value(ui, theme, "cwd", &node.cwd);
                inspector_value(ui, theme, "lifecycle", lifecycle_label(&node.lifecycle));
                if let Some(pid) = node.pid {
                    inspector_value(ui, theme, "pid", &pid.to_string());
                }
                if let Some(ppid) = node.ppid {
                    inspector_value(ui, theme, "ppid", &ppid.to_string());
                }
                inspector_value(ui, theme, "runtime", &format_duration(node.runtime_ms));
                if let Some(code) = node.exit_code {
                    inspector_value(ui, theme, "exit", &code.to_string());
                }

                if let Some(agent) = &node.agent {
                    inspector_section(ui, theme, "AGENT");
                    if let Some(declared) = &agent.name.declared_name {
                        inspector_value(ui, theme, "declared", declared);
                    }
                    if let Some(tool) = &agent.agent.tool {
                        inspector_value(ui, theme, "tool", tool);
                    }
                    if let Some(model) = &agent.agent.model {
                        inspector_value(ui, theme, "model", model);
                    }
                    if let Some(branch) = &agent.git_branch {
                        inspector_value(ui, theme, "branch", branch);
                    }
                    if let Some(task) = &agent.current_task {
                        inspector_value(ui, theme, "task", task);
                    }
                }

                inspector_section(ui, theme, "PANE");
                if node.pane_bindings.is_empty() {
                    ui.label(
                        RichText::new("Not open in a pane")
                            .color(theme.text_faint)
                            .small(),
                    );
                } else {
                    for binding in &node.pane_bindings {
                        inspector_value(
                            ui,
                            theme,
                            if binding.temporary {
                                "temporary"
                            } else {
                                "layout"
                            },
                            binding.pane_id.as_str(),
                        );
                    }
                }
                ui.label(
                    RichText::new(pane_capability_label(&node.pane_capability))
                        .color(theme.text_dim)
                        .small(),
                );
            });
    }

    fn inspector_overlay(
        &self,
        ui: &mut Ui,
        theme: &Theme,
        node: &TreeNodeView,
        state: &mut ViewState,
        body: Rect,
    ) {
        let width = INSPECTOR_WIDTH.min((body.width() - 20.0).max(0.0));
        let panel = Rect::from_min_size(
            body.right_top() + Vec2::new(-width - 10.0, 10.0),
            Vec2::new(width, (body.height() - 20.0).max(0.0)),
        );
        ui.painter().rect_filled(panel, 0.0, theme.panel);
        ui.painter().rect_stroke(
            panel,
            0.0,
            Stroke::new(1.0, theme.border),
            egui::StrokeKind::Outside,
        );
        ui.scope_builder(region(panel.shrink(1.0), "inspector-overlay"), |ui| {
            self.process_inspector(ui, theme, node, state);
        });
    }

    fn quick_preview_overlay(
        &self,
        ui: &mut Ui,
        theme: &Theme,
        snapshot: &HierarchySnapshot,
        node: &TreeNodeView,
        state: &mut ViewState,
        full: Rect,
    ) {
        let history = quick_preview_history(
            state
                .preview_history
                .get(&node.node_id)
                .map(Vec::as_slice)
                .unwrap_or_default(),
        );
        let width = 600.0_f32.min((full.width() - 36.0).max(0.0));
        let height = 260.0_f32.min((full.height() - 36.0).max(0.0));
        let panel = Rect::from_center_size(full.center(), Vec2::new(width, height));
        ui.painter()
            .rect_filled(full, 0.0, Color32::from_black_alpha(115));
        ui.painter().rect_filled(panel, 0.0, theme.panel);
        ui.painter().rect_stroke(
            panel,
            0.0,
            Stroke::new(1.0, theme.border),
            egui::StrokeKind::Outside,
        );

        ui.scope_builder(region(panel.shrink(14.0), "quick-preview"), |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("QUICK PREVIEW").color(theme.text_dim).small());
                ui.label(
                    RichText::new(process_title(node))
                        .color(theme.text)
                        .strong(),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Close").clicked() {
                        state.quick_preview = None;
                    }
                });
            });
            ui.separator();

            match (history.as_slice(), visible_preview(node)) {
                ([_, ..], _) => {
                    ui.add_space(8.0);
                    for (index, preview) in history.iter().enumerate() {
                        ui.label(
                            RichText::new(&preview.normalized_text)
                                .color(if index == 0 {
                                    theme.text
                                } else {
                                    theme.text_dim
                                })
                                .size(if index == 0 { 16.0 } else { 13.0 }),
                        );
                    }
                    let latest = &history[0];
                    ui.label(
                        RichText::new(format!(
                            "{} · {} · stable{}",
                            preview_source_label(latest.source),
                            latest.confidence.label(),
                            if latest.redacted { " · redacted" } else { "" }
                        ))
                        .monospace()
                        .color(theme.text_faint),
                    );
                }
                ([], Some(preview)) => {
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(&preview.normalized_text)
                            .color(theme.text)
                            .size(16.0),
                    );
                }
                ([], None) => {
                    ui.add_space(18.0);
                    ui.label(
                        RichText::new("No stable, safe activity preview is available yet.")
                            .color(theme.text_faint),
                    );
                }
            }

            ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                ui.horizontal(|ui| {
                    if ui.button("Open temporary pane  ⌘⏎").clicked() {
                        state.push_hierarchy_action(HierarchyAction::OpenTemporaryPane {
                            surface_id: snapshot.tree_state.surface_id.clone(),
                            session_id: node.session_id.clone(),
                            node_id: node.node_id.clone(),
                        });
                    }
                    if ui.button("Show details").clicked() {
                        state.inspector_open = true;
                        state.quick_preview = None;
                    }
                    ui.label(
                        RichText::new("Esc closes · layout and pane focus stay unchanged")
                            .color(theme.text_faint)
                            .small(),
                    );
                });
            });
        });
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

    fn settings_sheet(
        &self,
        ui: &mut Ui,
        theme: &Theme,
        _state: &mut ViewState,
        full: Rect,
    ) -> Vec<ViewAction> {
        let mut actions = Vec::new();
        let panel = Rect::from_center_size(
            full.center(),
            Vec2::new(
                760.0_f32.min((full.width() - 40.0).max(320.0)),
                560.0_f32.min((full.height() - 40.0).max(360.0)),
            ),
        );
        ui.painter()
            .rect_filled(full, 0.0, Color32::from_black_alpha(150));
        ui.painter().rect_filled(panel, 10.0, theme.panel);
        ui.painter().rect_stroke(
            panel,
            10.0,
            Stroke::new(1.0, theme.border),
            egui::StrokeKind::Outside,
        );

        ui.scope_builder(region(panel.shrink(20.0), "settings"), |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("Settings")
                        .size(21.0)
                        .color(theme.text)
                        .strong(),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Close").clicked() {
                        actions.push(ViewAction::CloseOverlay);
                    }
                });
            });
            ui.add_space(14.0);
            ui.vertical(|ui| {
                ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.label(
                                RichText::new("Layout presets")
                                    .size(17.0)
                                    .color(theme.text)
                                    .strong(),
                            );
                            ui.label(
                                RichText::new(
                                    "Reusable rows, columns and commands offered when a Session is created.",
                                )
                                .color(theme.text_dim)
                                .small(),
                            );
                        });
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::TOP),
                            |ui| {
                                if ui
                                    .add(
                                        egui::Button::new("New layout…")
                                            .fill(theme.running)
                                            .stroke(Stroke::NONE),
                                    )
                                    .clicked()
                                {
                                    actions.push(ViewAction::OpenLayoutEditor(
                                        LayoutEditorOrigin::Settings,
                                    ));
                                }
                            },
                        );
                    });
                    ui.add_space(10.0);
                    egui::ScrollArea::vertical()
                        .id_salt("settings-layout-presets")
                        .max_height(260.0)
                        .show(ui, |ui| {
                            for template in self.templates {
                                egui::Frame::new()
                                    .fill(theme.raised)
                                    .stroke(Stroke::new(1.0, theme.border))
                                    .corner_radius(6.0)
                                    .inner_margin(egui::Margin::symmetric(12, 9))
                                    .show(ui, |ui| {
                                        ui.horizontal(|ui| {
                                            ui.vertical(|ui| {
                                                ui.label(
                                                    RichText::new(&template.name)
                                                        .color(theme.text)
                                                        .strong(),
                                                );
                                                let command_summary = if template.commands.is_empty()
                                                {
                                                    format!(
                                                        "{} default shell cell(s)",
                                                        template.pane_count
                                                    )
                                                } else {
                                                    template.commands.join(" · ")
                                                };
                                                ui.label(
                                                    RichText::new(command_summary)
                                                        .monospace()
                                                        .color(theme.text_dim)
                                                        .small(),
                                                );
                                            });
                                            ui.with_layout(
                                                egui::Layout::right_to_left(egui::Align::Center),
                                                |ui| {
                                                    ui.label(
                                                        RichText::new(if template.built_in {
                                                            "Built in"
                                                        } else {
                                                            "Custom"
                                                        })
                                                        .color(theme.text_faint)
                                                        .small(),
                                                    );
                                                },
                                            );
                                        });
                                    });
                                ui.add_space(6.0);
                            }
                        });
                    ui.add_space(12.0);
                    ui.separator();
                    ui.label(
                        RichText::new("Current Session attention")
                            .color(theme.text)
                            .strong(),
                    );
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
        });
        actions
    }
}

fn hierarchy_row(
    ui: &mut Ui,
    theme: &Theme,
    row: HierarchyRow<'_>,
    selected: bool,
    expanded: bool,
    focused_pane: bool,
    active_session: bool,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), row.height()),
        Sense::click(),
    );
    if selected {
        ui.painter().rect_filled(rect, 0.0, theme.selection);
    } else if response.hovered() {
        ui.painter().rect_filled(rect, 0.0, theme.raised);
    }

    let needs_user = match row {
        HierarchyRow::Workspace(workspace) => workspace.workspace.sessions_needing_user > 0,
        HierarchyRow::Session { session, .. } => {
            session.session.needs_user || session.session.badge_count > 0
        }
        HierarchyRow::Process { node, .. } => node.needs_user,
    };
    if focused_pane || active_session {
        ui.painter().rect_filled(
            Rect::from_min_size(rect.min, Vec2::new(3.0, rect.height())),
            0.0,
            theme.running,
        );
    }
    if needs_user {
        ui.painter().rect_filled(
            Rect::from_min_size(
                rect.right_top() - Vec2::new(3.0, 0.0),
                Vec2::new(3.0, rect.height()),
            ),
            0.0,
            theme.attention,
        );
    }

    let indent = 9.0 + row.depth() as f32 * 14.0;
    let caret = if row.child_count() == 0 {
        "·"
    } else if expanded {
        "▾"
    } else {
        "▸"
    };
    ui.painter().text(
        rect.min + Vec2::new(indent, 7.0),
        Align2::LEFT_TOP,
        caret,
        FontId::new(11.0, egui::FontFamily::Monospace),
        if row.child_count() == 0 {
            theme.text_faint
        } else {
            theme.text_dim
        },
    );

    let text_x = rect.min.x + indent + 15.0;
    let right_tag = match row {
        HierarchyRow::Workspace(workspace) if workspace.workspace.sessions_needing_user > 0 => {
            Some(format!(
                "{} NEED YOU",
                workspace.workspace.sessions_needing_user
            ))
        }
        HierarchyRow::Session { session, .. } if session.session.badge_count > 0 => {
            Some(if session.session.badge_count > 1 {
                format!("YOUR TURN · {}", session.session.badge_count)
            } else {
                "YOUR TURN".into()
            })
        }
        HierarchyRow::Session { .. } if active_session => Some("ACTIVE".into()),
        HierarchyRow::Process { .. } if focused_pane => Some("FOCUSED".into()),
        HierarchyRow::Process { node, .. } if !node.pane_bindings.is_empty() => Some(
            if node.pane_bindings.iter().any(|binding| binding.temporary) {
                "TEMP PANE".into()
            } else {
                "PANE OPEN".into()
            },
        ),
        _ => None,
    };
    let right_width = if right_tag.is_some() { 94.0 } else { 10.0 };
    let clip = Rect::from_min_max(
        egui::pos2(text_x, rect.min.y),
        egui::pos2((rect.max.x - right_width).max(text_x), rect.max.y),
    );
    let painter = ui.painter().with_clip_rect(clip);

    match row {
        HierarchyRow::Workspace(workspace) => {
            painter.text(
                egui::pos2(text_x, rect.min.y + 4.0),
                Align2::LEFT_TOP,
                &workspace.workspace.name,
                theme.ui_font.clone(),
                theme.text,
            );
            let mut detail = format!("WORKSPACE · {} sessions", workspace.workspace.session_count);
            if workspace.workspace.lease_reconciliation_required {
                detail.push_str(" · LEASE CHECK");
            }
            painter.text(
                egui::pos2(text_x, rect.min.y + 22.0),
                Align2::LEFT_TOP,
                detail,
                FontId::new(10.0, egui::FontFamily::Monospace),
                if workspace.workspace.lease_reconciliation_required {
                    theme.attention
                } else {
                    theme.text_faint
                },
            );
        }
        HierarchyRow::Session { session, .. } => {
            let summary = &session.session;
            painter.text(
                egui::pos2(text_x, rect.min.y + 4.0),
                Align2::LEFT_TOP,
                &summary.name,
                theme.ui_font.clone(),
                theme.text,
            );
            let (colour, glyph) = if summary.needs_user || summary.badge_count > 0 {
                (theme.attention, "◆")
            } else {
                theme.state_marker(summary.display_state)
            };
            let mut detail = format!(
                "{} · {glyph} {} · {} running",
                summary.mode.label(),
                summary.state_label,
                summary.running_count
            );
            if summary.read_only_enforced {
                detail.push_str(" · enforced");
            }
            if summary.muted {
                detail.push_str(" · muted");
            }
            painter.text(
                egui::pos2(text_x, rect.min.y + 23.0),
                Align2::LEFT_TOP,
                detail,
                FontId::new(10.0, egui::FontFamily::Monospace),
                colour,
            );
        }
        HierarchyRow::Process { node, .. } => {
            painter.text(
                egui::pos2(text_x, rect.min.y + 3.0),
                Align2::LEFT_TOP,
                process_title(node),
                theme.ui_font.clone(),
                theme.text,
            );
            let (colour, glyph) = theme.state_marker(node.display_state);
            let relation = if node.relationship_is_provisional {
                " · inferred"
            } else {
                ""
            };
            painter.text(
                egui::pos2(text_x, rect.min.y + 21.0),
                Align2::LEFT_TOP,
                format!(
                    "{} · {glyph} {}{relation}",
                    node_kind_label(node.kind),
                    node.state_label
                ),
                FontId::new(10.0, egui::FontFamily::Monospace),
                if node.relationship_is_provisional {
                    theme.provisional
                } else {
                    colour
                },
            );
            painter.text(
                egui::pos2(text_x, rect.min.y + 39.0),
                Align2::LEFT_TOP,
                visible_preview(node)
                    .map(|preview| preview.normalized_text.as_str())
                    .unwrap_or("no activity preview"),
                FontId::new(10.0, egui::FontFamily::Proportional),
                theme.text_faint,
            );
        }
    }

    if let Some(tag) = right_tag {
        ui.painter().text(
            rect.right_top() + Vec2::new(-9.0, 6.0),
            Align2::RIGHT_TOP,
            tag,
            FontId::new(9.0, egui::FontFamily::Monospace),
            if needs_user {
                theme.attention
            } else {
                theme.running
            },
        );
    }

    let accessible_name = row.accessible_name(focused_pane);
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &accessible_name)
    });
    response.ctx.accesskit_node_builder(response.id, |node| {
        node.set_role(egui::accesskit::Role::TreeItem);
        node.set_label(accessible_name);
        node.set_level(row.depth().saturating_add(1));
        node.set_selected(selected);
        if row.child_count() > 0 {
            node.set_expanded(expanded);
            node.add_action(if expanded {
                egui::accesskit::Action::Collapse
            } else {
                egui::accesskit::Action::Expand
            });
        }
        node.add_action(egui::accesskit::Action::Click);
        node.add_action(egui::accesskit::Action::Focus);
    });
    response
}

fn set_hierarchy_selection(state: &mut ViewState, snapshot: &HierarchySnapshot, key: HierarchyKey) {
    if state.selected_tree.as_ref() == Some(&key)
        || (state.selected_tree.is_none() && snapshot.tree_state.selected.as_ref() == Some(&key))
    {
        return;
    }
    state.selected_tree = Some(key.clone());
    state.scroll_tree_to = Some(key.clone());
    state.push_hierarchy_action(HierarchyAction::Select {
        surface_id: snapshot.tree_state.surface_id.clone(),
        key,
    });
}

fn set_hierarchy_expanded(
    state: &mut ViewState,
    snapshot: &HierarchySnapshot,
    key: HierarchyKey,
    expanded: bool,
) {
    if row_is_expanded(snapshot, state, &key) == expanded {
        return;
    }
    state.tree_expansion.insert(key.clone(), expanded);
    state.push_hierarchy_action(HierarchyAction::SetExpanded {
        surface_id: snapshot.tree_state.surface_id.clone(),
        key,
        expanded,
    });
}

fn activate_hierarchy_row(
    state: &mut ViewState,
    snapshot: &HierarchySnapshot,
    row: HierarchyRow<'_>,
) -> Vec<ViewAction> {
    match row {
        HierarchyRow::Workspace(_) => {
            let key = row.key();
            let expanded = row_is_expanded(snapshot, state, &key);
            set_hierarchy_expanded(state, snapshot, key, !expanded);
            Vec::new()
        }
        HierarchyRow::Session { session, .. } => {
            vec![ViewAction::SelectSession(session.session.id.clone())]
        }
        HierarchyRow::Process { node, .. } => {
            state.push_hierarchy_action(HierarchyAction::FocusPaneForNode {
                surface_id: snapshot.tree_state.surface_id.clone(),
                session_id: node.session_id.clone(),
                node_id: node.node_id.clone(),
            });
            Vec::new()
        }
    }
}

/// Double-click is the explicit mouse equivalent of "open or focus". A background
/// Agent with no Pane gets a temporary one; an Agent already represented in the
/// layout keeps that stable layout and merely focuses its existing Pane.
fn open_or_focus_hierarchy_row(
    state: &mut ViewState,
    snapshot: &HierarchySnapshot,
    row: HierarchyRow<'_>,
) -> Vec<ViewAction> {
    let HierarchyRow::Process { node, .. } = row else {
        return activate_hierarchy_row(state, snapshot, row);
    };
    let action = if node.pane_bindings.is_empty() {
        HierarchyAction::OpenTemporaryPane {
            surface_id: snapshot.tree_state.surface_id.clone(),
            session_id: node.session_id.clone(),
            node_id: node.node_id.clone(),
        }
    } else {
        HierarchyAction::FocusPaneForNode {
            surface_id: snapshot.tree_state.surface_id.clone(),
            session_id: node.session_id.clone(),
            node_id: node.node_id.clone(),
        }
    };
    state.push_hierarchy_action(action);
    Vec::new()
}

#[derive(Clone, Copy)]
enum HierarchyKeypress {
    Up,
    Down,
    Left,
    Right,
    Preview,
    TemporaryPane,
    Activate,
    Blur,
}

fn handle_hierarchy_keyboard(
    ui: &mut Ui,
    snapshot: &HierarchySnapshot,
    state: &mut ViewState,
    rows: &[HierarchyRow<'_>],
) -> Vec<ViewAction> {
    let keypress = ui.input_mut(|input| {
        if input.consume_key(Modifiers::COMMAND, Key::Enter) {
            Some(HierarchyKeypress::TemporaryPane)
        } else if input.consume_key(Modifiers::NONE, Key::Escape) {
            Some(HierarchyKeypress::Blur)
        } else if input.consume_key(Modifiers::NONE, Key::ArrowUp) {
            Some(HierarchyKeypress::Up)
        } else if input.consume_key(Modifiers::NONE, Key::ArrowDown) {
            Some(HierarchyKeypress::Down)
        } else if input.consume_key(Modifiers::NONE, Key::ArrowLeft) {
            Some(HierarchyKeypress::Left)
        } else if input.consume_key(Modifiers::NONE, Key::ArrowRight) {
            Some(HierarchyKeypress::Right)
        } else if input.consume_key(Modifiers::NONE, Key::Space) {
            Some(HierarchyKeypress::Preview)
        } else if input.consume_key(Modifiers::NONE, Key::Enter) {
            Some(HierarchyKeypress::Activate)
        } else {
            None
        }
    });
    let Some(keypress) = keypress else {
        return Vec::new();
    };
    if matches!(keypress, HierarchyKeypress::Blur) {
        state.tree_has_focus = false;
        return Vec::new();
    }
    let Some(first) = rows.first() else {
        return Vec::new();
    };
    let selection = effective_selection(snapshot, state);
    let Some(index) = selection
        .as_ref()
        .and_then(|selected| rows.iter().position(|row| row.key() == *selected))
    else {
        set_hierarchy_selection(state, snapshot, first.key());
        return Vec::new();
    };
    let row = rows[index];
    match keypress {
        HierarchyKeypress::Up => {
            let target = index.saturating_sub(1);
            set_hierarchy_selection(state, snapshot, rows[target].key());
        }
        HierarchyKeypress::Down => {
            let target = (index + 1).min(rows.len() - 1);
            set_hierarchy_selection(state, snapshot, rows[target].key());
        }
        HierarchyKeypress::Left => {
            let key = row.key();
            if row.child_count() > 0 && row_is_expanded(snapshot, state, &key) {
                set_hierarchy_expanded(state, snapshot, key, false);
            } else if let Some(parent) = row.parent_key() {
                set_hierarchy_selection(state, snapshot, parent);
            }
        }
        HierarchyKeypress::Right => {
            let key = row.key();
            if row.child_count() > 0 && !row_is_expanded(snapshot, state, &key) {
                set_hierarchy_expanded(state, snapshot, key, true);
            } else if rows
                .get(index + 1)
                .is_some_and(|next| next.depth() > row.depth())
            {
                set_hierarchy_selection(state, snapshot, rows[index + 1].key());
            }
        }
        HierarchyKeypress::Preview => {
            if let HierarchyRow::Process { node, .. } = row {
                let key = row.key();
                state.quick_preview = Some(key);
                state.push_hierarchy_action(HierarchyAction::QuickPreview {
                    surface_id: snapshot.tree_state.surface_id.clone(),
                    session_id: node.session_id.clone(),
                    node_id: node.node_id.clone(),
                });
            }
        }
        HierarchyKeypress::TemporaryPane => {
            if let HierarchyRow::Process { node, .. } = row {
                state.push_hierarchy_action(HierarchyAction::OpenTemporaryPane {
                    surface_id: snapshot.tree_state.surface_id.clone(),
                    session_id: node.session_id.clone(),
                    node_id: node.node_id.clone(),
                });
            }
        }
        HierarchyKeypress::Activate => return activate_hierarchy_row(state, snapshot, row),
        HierarchyKeypress::Blur => {}
    }
    Vec::new()
}

fn relationship_label(kind: RelationshipKind) -> &'static str {
    match kind {
        RelationshipKind::SpawnedBy => "spawned by parent",
        RelationshipKind::OwnsProcess => "owned by parent",
        RelationshipKind::Related => "related to parent",
        RelationshipKind::Unknown => "relationship unknown",
    }
}

fn lifecycle_label(lifecycle: &turn_core::state::Lifecycle) -> &'static str {
    use turn_core::state::Lifecycle;
    match lifecycle {
        Lifecycle::Spawning => "starting",
        Lifecycle::Alive => "alive",
        Lifecycle::Exited { .. } => "exited",
        Lifecycle::Signaled { .. } => "signaled",
        Lifecycle::Stopped { .. } => "stopped by user",
        Lifecycle::Orphaned => "orphaned",
        Lifecycle::Reconnected => "reconnected",
        Lifecycle::Lost => "lost",
    }
}

fn preview_source_label(source: turn_core::model::PreviewSource) -> &'static str {
    use turn_core::model::PreviewSource;
    match source {
        PreviewSource::SemanticEvent => "semantic event",
        PreviewSource::AdapterState => "adapter state",
        PreviewSource::RelevantAction => "relevant action",
        PreviewSource::StableScreenLine => "stable screen line",
        PreviewSource::ProcessFallback => "process fallback",
    }
}

fn pane_capability_label(capability: &NodePaneCapability) -> String {
    match capability {
        NodePaneCapability::PreviewDetails => "Preview/details only".into(),
        NodePaneCapability::Terminal { streams } => {
            format!("Attachable terminal · {} stream(s)", streams.len())
        }
    }
}

fn format_duration(milliseconds: i64) -> String {
    let seconds = milliseconds.max(0) / 1_000;
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3_600 {
        format!("{}m {}s", seconds / 60, seconds % 60)
    } else {
        format!("{}h {}m", seconds / 3_600, (seconds % 3_600) / 60)
    }
}

fn inspector_section(ui: &mut Ui, theme: &Theme, title: &str) {
    ui.add_space(10.0);
    ui.label(
        RichText::new(title)
            .monospace()
            .color(theme.text_faint)
            .small(),
    );
}

fn inspector_value(ui: &mut Ui, theme: &Theme, label: &str, value: &str) {
    ui.horizontal_wrapped(|ui| {
        ui.label(
            RichText::new(format!("{label}:"))
                .monospace()
                .color(theme.text_faint)
                .small(),
        );
        ui.label(RichText::new(value).color(theme.text_dim).small());
    });
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

/// A divider the user can drag.
///
/// The drag is turned into a fraction of the parent split, which is what `resize_pane`
/// takes, and it is sent as it happens rather than on release: a divider that only moved
/// when let go would feel broken. In egui 0.35 `drag_delta()` is already the movement
/// since the previous frame; `total_drag_delta()` is the cumulative alternative.
fn draggable_divider(ui: &mut Ui, theme: &Theme, divider: &Divider) -> Vec<ViewAction> {
    let mut actions = Vec::new();
    let id = ui
        .id()
        .with(("divider", divider.before.as_str(), divider.after.as_str()));
    let response = ui.interact(divider.grab_rect(), id, Sense::click_and_drag());
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
    if response.double_clicked() {
        actions.push(ViewAction::EqualizeDivider {
            before: divider.before.clone(),
            after: divider.after.clone(),
        });
    } else if response.dragged() {
        if let Some(fraction) = divider.fraction_for_drag(response.drag_delta()) {
            actions.push(ViewAction::ResizeDivider {
                before: divider.before.clone(),
                after: divider.after.clone(),
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
    use turn_core::event::Confidence;
    use turn_core::model::{
        ActivityPreview, Pane, PaneKind, PreviewSource, ProcessNode, Relation, Session,
        SessionMode, Workspace,
    };
    use turn_core::state::{Lifecycle, Turn};
    use turn_proto::{SessionSummary, TreeSurfaceState, WorkspaceSummary};

    const T0: i64 = 1_700_000_000_000;

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

    fn hierarchy_fixture() -> (HierarchySnapshot, NodeId, NodeId, SessionId) {
        let workspace = Workspace::new("turn", "/repo/turn", T0);
        let mut session = Session::new(
            workspace.id.clone(),
            "Unify navigation",
            "/repo/turn",
            Layout::single(Pane::new(PaneKind::Agent).with_command("claude")),
            T0,
        );
        session.mode = SessionMode::ReadOnly;
        session.read_only_enforced = true;

        let mut root = ProcessNode::agent(session.id.clone(), "claude", "/repo/turn", T0);
        root.lifecycle = Lifecycle::Alive;
        root.turn = Some(Turn::Active);
        root.activity_preview = Some(ActivityPreview {
            node_id: root.id.clone(),
            raw_source_sequence: Some(4),
            normalized_text: "Reviewing the navigation projection".into(),
            source: PreviewSource::SemanticEvent,
            confidence: Confidence::Explicit,
            stable: true,
            contains_sensitive_data: false,
            redacted: false,
            updated_ms: T0 + 4,
        });
        let root_id = session.tree.insert(root);

        let mut child = ProcessNode::agent(
            session.id.clone(),
            "claude --subagent",
            "/repo/turn",
            T0 + 1,
        );
        child.kind = NodeKind::Subagent;
        child.lifecycle = Lifecycle::Alive;
        child.turn = Some(Turn::Active);
        child.link_to(root_id.clone(), Relation::Inferred);
        let child_id = session.tree.insert(child);

        let session_id = session.id.clone();
        let summary = SessionSummary::from_session(&session, 1, false, T0 + 10);
        let workspace_summary =
            WorkspaceSummary::from_workspace(&workspace, std::slice::from_ref(&summary));
        let nodes = TreeNodeView::for_session(&session, T0 + 10);
        let snapshot = HierarchySnapshot {
            revision: 7,
            tree_state: TreeSurfaceState {
                surface_id: "window-test".into(),
                selected: Some(HierarchyKey::process(root_id.clone())),
                expanded: vec![
                    HierarchyKey::workspace(workspace.id.clone()),
                    HierarchyKey::session(session.id),
                ],
            },
            workspaces: vec![WorkspaceTreeView {
                workspace: workspace_summary,
                checkouts: Vec::new(),
                write_lease: None,
                sessions: vec![SessionTreeView {
                    session: summary,
                    nodes,
                }],
            }],
        };
        (snapshot, root_id, child_id, session_id)
    }

    #[test]
    fn selecting_a_project_folder_derives_an_editable_workspace_name() {
        let mut draft = WorkspaceDraft::new(true);
        draft
            .select_directory(std::path::Path::new("/Users/x/projects/space-troopers"))
            .unwrap();

        assert_eq!(draft.root, "/Users/x/projects/space-troopers");
        assert_eq!(draft.name, "space-troopers");
        assert!(draft.name_is_derived);

        draft.name = "Space Troopers".into();
        draft.name_is_derived = false;
        draft.root = "/Users/x/projects/space-troopers-v2".into();
        draft.refresh_derived_name();
        assert_eq!(draft.name, "Space Troopers");
    }

    #[test]
    fn choosing_another_folder_replaces_the_suggestion_but_not_the_ability_to_edit_it() {
        let mut draft = WorkspaceDraft::new(true);
        draft.name = "A custom label".into();
        draft.name_is_derived = false;

        draft
            .select_directory(std::path::Path::new("/Users/x/projects/alternative"))
            .unwrap();

        assert_eq!(draft.root, "/Users/x/projects/alternative");
        assert_eq!(draft.name, "alternative");
        assert!(draft.name_is_derived);
        assert!(draft.request_name_focus);
    }

    #[test]
    fn the_unified_tree_respects_each_expansion_level() {
        let (snapshot, root_id, child_id, _) = hierarchy_fixture();
        let mut state = ViewState::default();
        let collapsed = visible_hierarchy_rows(&snapshot, &state);
        assert_eq!(collapsed.len(), 3, "workspace, session, collapsed agent");
        assert_eq!(
            collapsed.iter().map(|row| row.depth()).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert!(!collapsed
            .iter()
            .any(|row| row.key() == HierarchyKey::process(child_id.clone())));

        set_hierarchy_expanded(&mut state, &snapshot, HierarchyKey::process(root_id), true);
        let expanded = visible_hierarchy_rows(&snapshot, &state);
        assert_eq!(expanded.len(), 4);
        assert_eq!(expanded.last().map(|row| row.depth()), Some(3));
        assert!(expanded
            .iter()
            .any(|row| row.key() == HierarchyKey::process(child_id.clone())));
    }

    #[test]
    fn selection_expansion_and_focus_are_different_typed_actions() {
        let (snapshot, root_id, _, _) = hierarchy_fixture();
        let mut state = ViewState::default();
        let key = HierarchyKey::process(root_id.clone());

        set_hierarchy_expanded(&mut state, &snapshot, key.clone(), true);
        let process = visible_hierarchy_rows(&snapshot, &state)
            .into_iter()
            .find(|row| row.key() == key)
            .expect("root agent is visible");
        set_hierarchy_selection(
            &mut state,
            &snapshot,
            HierarchyKey::session(process_session(process)),
        );
        assert!(activate_hierarchy_row(&mut state, &snapshot, process).is_empty());

        let actions = state.take_hierarchy_actions();
        assert!(actions.iter().any(|action| matches!(
            action,
            HierarchyAction::SetExpanded { key: changed, expanded: true, .. } if changed == &key
        )));
        assert!(actions.iter().any(|action| matches!(
            action,
            HierarchyAction::Select {
                key: HierarchyKey::Session { .. },
                ..
            }
        )));
        assert!(actions.iter().any(|action| matches!(
            action,
            HierarchyAction::FocusPaneForNode { node_id, .. } if node_id == &root_id
        )));
    }

    #[test]
    fn node_less_attention_is_visible_without_relabelling_the_running_agent() {
        let (snapshot, root_id, _, _) = hierarchy_fixture();
        let workspace = HierarchyRow::Workspace(&snapshot.workspaces[0]);
        assert!(workspace
            .accessible_name(false)
            .contains("1 sessions need attention"));

        let session = &snapshot.workspaces[0].sessions[0];
        let root = session
            .nodes
            .iter()
            .find(|node| node.node_id == root_id)
            .expect("running parent agent");
        assert_eq!(root.display_state, DisplayState::Running);
        assert!(!root.needs_user);
        assert_eq!(session.session.badge_count, 1);
        assert_eq!(session.session.state_label, "YOUR TURN");
        assert!(!session.session.needs_user);
        assert!(HierarchyRow::Session {
            workspace: &snapshot.workspaces[0],
            session,
        }
        .accessible_name(false)
        .contains("1 attention demand"));
    }

    #[test]
    fn double_click_opens_a_background_subagent_without_changing_the_saved_layout() {
        let (snapshot, root_id, child_id, session_id) = hierarchy_fixture();
        let session = &snapshot.workspaces[0].sessions[0];
        let child = session
            .nodes
            .iter()
            .find(|node| node.node_id == child_id)
            .expect("Reviewer");
        assert!(child.pane_bindings.is_empty());

        let mut state = ViewState::default();
        open_or_focus_hierarchy_row(
            &mut state,
            &snapshot,
            HierarchyRow::Process {
                session,
                node: child,
            },
        );
        let actions = state.take_hierarchy_actions();
        assert!(actions.iter().any(|action| matches!(
            action,
            HierarchyAction::OpenTemporaryPane {
                session_id: opened_session,
                node_id,
                ..
            } if opened_session == &session_id && node_id == &child_id
        )));
        assert!(!actions.iter().any(|action| matches!(
            action,
            HierarchyAction::FocusPaneForNode { node_id, .. } if node_id == &root_id
        )));
    }

    fn process_session(row: HierarchyRow<'_>) -> SessionId {
        match row {
            HierarchyRow::Process { node, .. } => node.session_id.clone(),
            _ => unreachable!("called only with a process row"),
        }
    }

    #[test]
    fn an_unredacted_sensitive_preview_never_reaches_navigation() {
        let (mut snapshot, root_id, _, _) = hierarchy_fixture();
        let node = snapshot.workspaces[0].sessions[0]
            .nodes
            .iter_mut()
            .find(|node| node.node_id == root_id)
            .unwrap();
        let preview = node.activity_preview.as_mut().unwrap();
        preview.contains_sensitive_data = true;
        preview.redacted = false;
        assert!(visible_preview(node).is_none());
    }

    #[test]
    fn quick_preview_shows_and_highlights_the_four_newest_entries() {
        let (snapshot, root_id, _, _) = hierarchy_fixture();
        let base = snapshot.workspaces[0].sessions[0]
            .nodes
            .iter()
            .find(|node| node.node_id == root_id)
            .and_then(|node| node.activity_preview.clone())
            .unwrap();
        let newest_first: Vec<_> = (1..=6_u64)
            .rev()
            .map(|sequence| ActivityPreview {
                raw_source_sequence: Some(sequence),
                normalized_text: sequence.to_string(),
                updated_ms: T0 + sequence as i64,
                ..base.clone()
            })
            .collect();

        let visible = quick_preview_history(&newest_first);
        assert_eq!(
            visible
                .iter()
                .map(|preview| preview.normalized_text.as_str())
                .collect::<Vec<_>>(),
            ["6", "5", "4", "3"]
        );
        assert_eq!(visible[0].normalized_text, "6", "index zero is highlighted");
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
        state.settings_open = false;
        state.attention_panel_open = true;
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

    #[test]
    fn the_layout_editor_starts_as_two_portable_shell_columns() {
        let draft = LayoutTemplateDraft::two_shells(LayoutEditorOrigin::NewSession);
        assert_eq!(draft.layout.pane_count(), 2);
        assert!(draft.layout.sizes_are_normalised());
        assert!(draft.layout.panes().iter().all(|pane| {
            pane.kind == PaneKind::Shell && pane.command.is_none() && pane.args.is_empty()
        }));
        let turn_core::model::LayoutNode::Split(split) = &draft.layout.root else {
            panic!("the starter must be a split");
        };
        assert_eq!(split.direction, Direction::Horizontal);
        assert!(split
            .children
            .iter()
            .all(|child| (child.size - 0.5).abs() < 0.001));
    }

    #[test]
    fn the_layout_editor_separates_program_and_quoted_arguments_without_a_shell() {
        let mut draft = LayoutTemplateDraft::two_shells(LayoutEditorOrigin::Settings);
        let selected = draft.selected.clone();
        draft.cells.insert(
            selected.clone(),
            CellCommandDraft {
                program: "npm".into(),
                arguments: "run test -- --name 'wall to ceiling'".into(),
            },
        );

        let layout = draft.materialized_layout().unwrap();
        let pane = layout.get(&selected).unwrap();
        assert_eq!(pane.command.as_deref(), Some("npm"));
        assert_eq!(
            pane.args,
            ["run", "test", "--", "--name", "wall to ceiling"]
        );
        assert_ne!(pane.command.as_deref(), Some("/bin/sh"));
    }
}
