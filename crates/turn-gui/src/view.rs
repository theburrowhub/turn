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

use std::collections::{BTreeSet, HashMap, HashSet};

use egui::{
    Align2, Color32, FontId, Key, Modifiers, Rect, Response, RichText, Sense, Stroke, Ui, Vec2,
};
use turn_core::attention::AttentionPolicy;
use turn_core::event::Risk;
use turn_core::ids::{
    AttentionId, HandoffId, LeaseId, NodeId, PaneId, SessionId, TemplateId, WorkspaceId,
};
use turn_core::model::{
    ActivityPreview, Direction, DropZone, Layout, LayoutPreset, LeaseState, NodeKind, Pane,
    PaneGeometry, PaneKind, PanePlacement, PreviewVisibility, RelationshipKind, RestoreBehaviour,
    RestoreState, SessionMode, SessionStatus, TreeFilter, TreeVisibilityMode, WorkspaceWriteLease,
};
use turn_core::state::{AwaitingReason, DisplayState, Lifecycle, Turn};
use turn_proto::cells::Grid;
use turn_proto::{
    CloseDisposition, ContextHandoffMode, ContextHandoffView, HierarchyKey, HierarchySnapshot,
    NewPane, NodePaneCapability, NodePaneView, PaneRestoreOutcome, ProtoErrorContext,
    SessionConflictAlternative, SessionSummary, SessionTreeView, TemplateSummary, TreeNodeView,
    TreeSurfaceState, WorkspaceSummary, WorkspaceTreeView,
};

use crate::icons;
use crate::keymap::{Command, Keymap};
use crate::palette::{self, Palette};
use crate::panes::{self, Arrangement, Divider, DropTarget, Side};
use crate::terminal::{self, PaneAction, PaneInteraction, PaneOptions};
use crate::theme::Theme;
use crate::transport::ConnectionState;

pub const OPEN_PANE_PLACEMENT_KEY: &str = "layout.open_pane_placement";

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
    pub agent: String,
    pub process: String,
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
    pub priority_boost: i16,
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

/// A restart left durable layout behind but no attachable process for one or more panes.
///
/// This is deliberately not a generic error string. Each outcome is an explicit offer
/// the user may accept; merely drawing it can never start a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRestoreView {
    pub session_id: SessionId,
    pub state: RestoreState,
    pub panes: Vec<PaneRestoreOutcome>,
}

/// A destructive lifecycle choice awaiting a second, explicit click.
///
/// Every field here exists to be *said* in the dialog. Nothing in Turn stops a process
/// without one of these on screen first, so a confirmation that could not name what it
/// was about to terminate would be the whole guarantee reduced to a shrug.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LifecycleConfirmation {
    EndSession {
        session_id: SessionId,
        name: String,
        running_count: usize,
        /// Of those, how many Turn cannot stop because they survived a previous daemon.
        ///
        /// The act goes ahead with them — a Session the user has finished with is not kept
        /// alive because a process escaped the daemon — so the honest place to say so is
        /// here, before the click, rather than in a status line afterwards.
        escaped_count: usize,
    },
    StopWorkspace {
        workspace_id: WorkspaceId,
        name: String,
        escaped_count: usize,
        /// How many Sessions the Workspace holds, and how many of them have something
        /// running. Closing a Workspace reaches every one of them, and a user who is
        /// only shown the Workspace's name cannot know how much that is.
        session_count: usize,
        running_sessions: usize,
        running_processes: usize,
    },
    /// The one that does not come back.
    ///
    /// A separate variant from `EndSession` rather than a flag on it, because the two ask
    /// different questions and the answer to one is not the answer to the other. Ending is
    /// about the work; deleting is about the record.
    DeleteSession {
        session_id: SessionId,
        name: String,
        running_count: usize,
        escaped_count: usize,
    },
    DeleteWorkspace {
        workspace_id: WorkspaceId,
        name: String,
        session_count: usize,
        running_processes: usize,
        escaped_count: usize,
        /// The directory the Workspace points at, shown verbatim.
        ///
        /// The one thing a person needs to see before deleting a Workspace is the path that is
        /// *not* being deleted. Naming it — rather than promising in the abstract that files
        /// are safe — is what makes the promise checkable by the person reading it.
        root: String,
    },
}

/// Window-local review state. The body itself comes from the daemon and cannot be
/// edited after review; changing source instruction or destination creates a new draft.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextHandoffDraft {
    pub session_id: SessionId,
    pub source_node_id: NodeId,
    pub target_node_id: Option<NodeId>,
    pub mode: ContextHandoffMode,
    pub instruction: String,
    pub prepared: Option<ContextHandoffView>,
    pub preparing: bool,
    pub delivering: bool,
    pub delivered: bool,
    pub error: Option<String>,
}

impl ContextHandoffDraft {
    fn new(session: &SessionTreeView, source: &TreeNodeView) -> Self {
        let target_node_id = session
            .nodes
            .iter()
            .find(|candidate| {
                candidate.is_agentic
                    && candidate.node_id != source.node_id
                    && context_target_unavailable_reason(candidate).is_none()
            })
            .or_else(|| {
                session
                    .nodes
                    .iter()
                    .find(|candidate| candidate.is_agentic && candidate.node_id != source.node_id)
            })
            .map(|candidate| candidate.node_id.clone());
        Self {
            session_id: session.session.id.clone(),
            source_node_id: source.node_id.clone(),
            target_node_id,
            mode: ContextHandoffMode::ContinueWith,
            instruction: String::new(),
            prepared: None,
            preparing: false,
            delivering: false,
            delivered: false,
            error: None,
        }
    }

    fn invalidate_review(&mut self) {
        self.prepared = None;
        self.delivered = false;
        self.error = None;
    }

    /// Resolves the exact selected Agent and its owning Session without borrowing
    /// pane focus or the active Session as a substitute.
    pub(crate) fn from_selection(
        snapshot: &HierarchySnapshot,
        selected: Option<&HierarchyKey>,
    ) -> Option<Self> {
        let HierarchyKey::Process { node_id } = selected? else {
            return None;
        };
        snapshot
            .workspaces
            .iter()
            .flat_map(|workspace| &workspace.sessions)
            .find_map(|session| {
                let source = session
                    .nodes
                    .iter()
                    .find(|node| &node.node_id == node_id && node.is_agentic)?;
                session
                    .nodes
                    .iter()
                    .any(|candidate| candidate.is_agentic && candidate.node_id != source.node_id)
                    .then(|| Self::new(session, source))
            })
    }
}

impl LifecycleConfirmation {
    /// The confirmation for ending one Session, from the daemon's own summary of it.
    pub fn end_session(session: &SessionSummary) -> Self {
        LifecycleConfirmation::EndSession {
            session_id: session.id.clone(),
            name: session.name.clone(),
            running_count: session.running_count,
            escaped_count: session.orphaned_count,
        }
    }

    /// The confirmation for deleting one Session, from the daemon's own summary of it.
    pub fn delete_session(session: &SessionSummary) -> Self {
        LifecycleConfirmation::DeleteSession {
            session_id: session.id.clone(),
            name: session.name.clone(),
            running_count: session.running_count,
            escaped_count: session.orphaned_count,
        }
    }

    /// The confirmation for deleting a whole Workspace, counted from the tree branch.
    ///
    /// The root path comes from the Workspace's own summary rather than from anything the
    /// caller assembles: the dialog's promise is about that exact directory, and a path the
    /// window guessed would be a promise about the wrong one.
    pub fn delete_workspace(workspace: &WorkspaceTreeView) -> Self {
        LifecycleConfirmation::DeleteWorkspace {
            workspace_id: workspace.workspace.id.clone(),
            name: workspace.workspace.name.clone(),
            session_count: workspace.sessions.len(),
            running_processes: workspace
                .sessions
                .iter()
                .map(|session| session.session.running_count)
                .sum(),
            escaped_count: workspace
                .sessions
                .iter()
                .map(|session| session.session.orphaned_count)
                .sum(),
            root: workspace.workspace.root.clone(),
        }
    }

    /// The confirmation for stopping a whole Workspace, counted from the tree branch.
    ///
    /// A constructor rather than three call sites adding up `running_count` themselves:
    /// the row control, the context menu and the keyboard command must all put the same
    /// number in front of the user, and a number the dialog computed differently from the
    /// row would be worse than no number at all.
    pub fn stop_workspace(workspace: &WorkspaceTreeView) -> Self {
        LifecycleConfirmation::StopWorkspace {
            workspace_id: workspace.workspace.id.clone(),
            name: workspace.workspace.name.clone(),
            session_count: workspace.sessions.len(),
            running_sessions: workspace
                .sessions
                .iter()
                .filter(|session| session.session.running_count > 0)
                .count(),
            running_processes: workspace
                .sessions
                .iter()
                .map(|session| session.session.running_count)
                .sum(),
            escaped_count: workspace
                .sessions
                .iter()
                .map(|session| session.session.orphaned_count)
                .sum(),
        }
    }
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
    /// Recovery state for the selected Session. Safe panes relaunch automatically in the Desk;
    /// this remains here to explain any safety gate while that happens.
    pub restore: Option<&'a SessionRestoreView>,
    /// A previous daemon owned this Session's checkout. Starting anything remains
    /// blocked until the user explicitly confirms a new fenced lease.
    pub recovery_lease: Option<&'a WorkspaceWriteLease>,
    /// Processes from the previous daemon that are still alive but cannot be controlled.
    /// They block recovery/relaunch so Turn never creates a second writer beside them.
    pub unreachable_processes: usize,
    /// Nodes whose relaunch request is currently in flight, normally from automatic recovery.
    pub relaunching: Vec<NodeId>,
    pub reclaiming_workspaces: Vec<WorkspaceId>,
    pub reclaiming_write_access: bool,
    pub permission: Option<PendingPermission>,
    /// The daemon's ordered queue still backs the global Next Attention action, but is
    /// not rendered as a second permanent navigation panel.
    pub queue: Vec<QueueItem>,
    pub connection: Option<ConnectionState>,
    /// True only while the user has explicitly opened the Archived filter.
    pub include_archived: bool,
    /// A failure worth showing, from a request that did not work.
    pub notice: Option<String>,
    /// Typed checkout conflict, rendered as a recovery flow rather than parsed text.
    pub write_conflict: Option<&'a ProtoErrorContext>,
    /// A link from a pane that must be confirmed before Turn hands it to the desktop.
    ///
    /// Only ever set for a link whose visible text names a different target than the one it
    /// would open — `links` decides that, and an ordinary link never arrives here.
    pub link_confirmation: Option<&'a terminal::links::LinkRequest>,
    /// The preferences in force, resolved by the daemon. `None` before the first answer.
    pub settings: Option<&'a turn_proto::SettingsView>,
    /// The attention policy in force, for the settings sheet.
    pub policy: Option<AttentionPolicy>,
    pub now_ms: i64,
}

/// The window's own mutable state: what is typed in the palette, and what is selected
/// in each pane.
#[derive(Default)]
struct LogoTexture(Option<egui::TextureHandle>);

impl std::fmt::Debug for LogoTexture {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("LogoTexture")
            .field(&self.0.as_ref().map(egui::TextureHandle::id))
            .finish()
    }
}

#[derive(Debug, Default)]
pub struct ViewState {
    pub palette: Palette,
    pub panes: HashMap<PaneId, PaneInteraction>,
    /// The checked-in product mark, decoded and uploaded once for the lifetime of the window.
    /// Keeping the handle here avoids allocating a GPU texture on every immediate-mode frame.
    logo_texture: LogoTexture,
    /// The pane whose header is being dragged, if any.
    ///
    /// The gesture itself belongs to `egui`, which also abandons it when Escape is
    /// pressed — so cancelling needs no bookkeeping here. What this is for is knowing
    /// that the Escape *belonged* to the drag, so the same press is not also spent on
    /// closing whatever is open behind it. Where the pane would land is deliberately not
    /// stored: it is recomputed from the pointer every frame, so a layout arriving from
    /// the daemon mid-drag cannot leave a landing spot on screen that no longer exists.
    pub dragged_pane: Option<PaneId>,
    /// Last floating rectangle already sent to the daemon, preventing one write
    /// per immediate-mode frame while a window is stationary.
    pub floating_geometry: HashMap<PaneId, PaneGeometry>,
    /// Which command sheet is open, if any.
    pub shortcuts_open: bool,
    pub settings_open: bool,
    /// Explicit, temporary view of the daemon-owned Attention Queue. It is an
    /// overlay, never a second persistent navigator beside the hierarchy.
    pub attention_panel_open: bool,
    pub write_conflict_open: bool,
    /// Which level the settings sheet writes to.
    ///
    /// Window-local, and remembered across openings: a user adjusting their Workspace does
    /// several in a row, and a selector that reset to Global between each would be a way to
    /// write the third one to the wrong place. Defaults to the narrowest level that exists,
    /// because that is the one a change is least likely to surprise somebody else with.
    pub settings_level: Option<turn_core::settings::Scope>,
    /// The chord being typed into one command's field, keyed by the command id.
    pub shortcut_drafts: std::collections::BTreeMap<String, String>,
    /// The text being typed into one preference's field, keyed by its key.
    ///
    /// Held while editing rather than written per keystroke: a `set_setting` per character
    /// would be a round trip per character, and every intermediate value would be refused as
    /// out of range on the way to a valid one.
    pub settings_drafts: std::collections::BTreeMap<String, String>,
    /// Replacement text for secret settings. Its `Debug` implementation reports only a
    /// count, so a diagnostic dump of window state cannot become a credential leak.
    secret_settings_drafts: SecretSettingsDrafts,
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
    /// Search is deliberately window-ephemeral: unlike the typed filter vocabulary it may
    /// contain repository names, tasks or preview text and therefore never reaches SQLite.
    pub tree_query: String,
    /// Optimistic copies of daemon-owned presentation state, for immediate controls.
    pub tree_filters: BTreeSet<TreeFilter>,
    pub tree_visibility: TreeVisibilityMode,
    pub tree_scroll_anchor: Option<HierarchyKey>,
    pub tree_manual_order: Vec<HierarchyKey>,
    /// Per-row expansion overrides awaiting acknowledgement from the daemon.
    pub tree_expansion: HashMap<HierarchyKey, bool>,
    /// Whether navigation keys belong to the hierarchy rather than the terminal.
    pub tree_has_focus: bool,
    /// A read-only overlay. Opening it never changes the pane layout or terminal focus.
    pub quick_preview: Option<HierarchyKey>,
    /// Confirmation for stopping a whole Session or Workspace. Process-row stop remains
    /// a separate, narrowly scoped action.
    pub lifecycle_confirmation: Option<LifecycleConfirmation>,
    /// Explicit source → destination context review. It is an overlay and never a Pane.
    pub context_handoff: Option<ContextHandoffDraft>,
    /// Explicit editor for the two audited Agent corrections.
    pub node_edit: Option<NodeEditDraft>,
    /// One placement decision shared by opening a Process and promoting its
    /// temporary view. It is local until Open is pressed.
    pub pane_placement: Option<PanePlacementDraft>,
    /// Explicit command/kind form for a new permanent Pane.
    pub new_pane: Option<NewPaneDraft>,
    /// Bounded, stable/redacted semantic history fetched on demand for Quick Preview.
    pub preview_history: HashMap<NodeId, Vec<ActivityPreview>>,
    /// The inspector is contextual and only occupies space for a selected Process.
    pub inspector_open: bool,
    hierarchy_actions: Vec<HierarchyAction>,
    observed_tree_state: Option<TreeSurfaceState>,
    observed_temporary_pane: Option<PaneId>,
}

#[derive(Default)]
struct SecretSettingsDrafts(std::collections::BTreeMap<String, String>);

impl std::fmt::Debug for SecretSettingsDrafts {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SecretSettingsDrafts")
            .field("entries", &self.0.len())
            .finish()
    }
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
            || self.lifecycle_confirmation.is_some()
            || self.context_handoff.is_some()
            || self.node_edit.is_some()
            || self.pane_placement.is_some()
            || self.new_pane.is_some()
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
pub enum NodeEditDraft {
    Rename {
        session_id: SessionId,
        node_id: NodeId,
        name: String,
    },
    Relationship {
        session_id: SessionId,
        node_id: NodeId,
        parent_node_id: Option<NodeId>,
        relationship_kind: RelationshipKind,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PanePlacementSource {
    Node {
        surface_id: String,
        session_id: SessionId,
        node_id: NodeId,
    },
    Temporary {
        surface_id: String,
        session_id: SessionId,
        pane_id: PaneId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanePlacementDraft {
    pub source: PanePlacementSource,
    pub target_pane_id: PaneId,
    pub placement: PanePlacement,
    pub remember: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NewPaneDraft {
    pub target_pane_id: PaneId,
    pub kind: PaneKind,
    pub title: String,
    pub program: String,
    pub arguments: String,
    pub cwd: String,
    pub placement: PanePlacement,
    pub error: Option<String>,
}

impl NewPaneDraft {
    fn new(target_pane_id: PaneId, placement: PanePlacement) -> Self {
        Self {
            target_pane_id,
            kind: PaneKind::Shell,
            title: String::new(),
            program: String::new(),
            arguments: String::new(),
            cwd: String::new(),
            placement: match placement {
                PanePlacement::Temporary => PanePlacement::SplitRight,
                placement => placement,
            },
            error: None,
        }
    }
}

impl NodeEditDraft {
    fn rename(node: &TreeNodeView) -> Self {
        Self::Rename {
            session_id: node.session_id.clone(),
            node_id: node.node_id.clone(),
            name: node.title.clone(),
        }
    }

    fn relationship(node: &TreeNodeView) -> Self {
        Self::Relationship {
            session_id: node.session_id.clone(),
            node_id: node.node_id.clone(),
            parent_node_id: node.parent.clone(),
            relationship_kind: node.relationship.kind,
        }
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
    SetExpandedAll {
        surface_id: String,
        expanded: bool,
    },
    SetPresentation {
        surface_id: String,
        filters: Vec<TreeFilter>,
        visibility_mode: TreeVisibilityMode,
        scroll_anchor: Option<HierarchyKey>,
    },
    Move {
        surface_id: String,
        key: HierarchyKey,
        before: Option<HierarchyKey>,
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
    OpenPane {
        surface_id: String,
        session_id: SessionId,
        node_id: NodeId,
        target_pane_id: PaneId,
        placement: PanePlacement,
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
    SetAttentionPriority {
        attention_id: AttentionId,
        priority_boost: i16,
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
    RenameNode {
        session_id: SessionId,
        node_id: NodeId,
        name: String,
    },
    CorrectRelationship {
        session_id: SessionId,
        node_id: NodeId,
        parent_node_id: Option<NodeId>,
        relationship_kind: RelationshipKind,
    },
    CloseSession {
        session_id: SessionId,
        disposition: CloseDisposition,
    },
    CloseWorkspace {
        workspace_id: WorkspaceId,
        disposition: CloseDisposition,
    },
    /// Maximises a pane, or puts the layout back when it is already the maximised one.
    ///
    /// The daemon's `zoom_pane` *toggles*, which is right for the keyboard chord and wrong for
    /// the tree: clicking two different subagents that share a pane would maximise and then
    /// un-maximise it. So the window sends this only when the toggle would land on the state it
    /// wants, worked out from the layout the daemon last gave it.
    ZoomPane {
        session_id: SessionId,
        pane_id: PaneId,
    },
    /// Removes a Session from Turn for good. Only ever produced by the confirmation dialog.
    DeleteSession {
        session_id: SessionId,
        disposition: CloseDisposition,
    },
    /// Removes a Workspace and its Sessions for good. Only ever produced by the dialog.
    DeleteWorkspace {
        workspace_id: WorkspaceId,
        disposition: CloseDisposition,
    },
    RelaunchNode {
        session_id: SessionId,
        node_id: NodeId,
        resume: bool,
    },
    SetArchivedVisibility {
        include: bool,
    },
    ArchiveSession {
        session_id: SessionId,
        archived: bool,
    },
    ArchiveWorkspace {
        workspace_id: WorkspaceId,
        archived: bool,
    },
    ReclaimWorkspaceWriteLease {
        workspace_id: WorkspaceId,
        session_id: SessionId,
        checkout_id: turn_core::ids::CheckoutId,
    },
    PrepareContextHandoff {
        session_id: SessionId,
        source_node_id: NodeId,
        target_node_id: NodeId,
        mode: ContextHandoffMode,
        instruction: Option<String>,
    },
    CreateContextHandoffTarget {
        session_id: SessionId,
        pane_id: PaneId,
    },
    DeliverContextHandoff {
        session_id: SessionId,
        handoff_id: HandoffId,
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
    /// Close one named pane — the one whose header control was used, never "whichever
    /// is active". The process it was showing keeps running; the control that produces
    /// this says so in as many words.
    ClosePane {
        pane_id: PaneId,
    },
    /// Move one pane so it sits beside another, from a header drag or from a
    /// `MovePane…` command. `zone` is which of the target's five regions it lands in,
    /// so the same action expresses "left of", "above" and "exchange with".
    ///
    /// The daemon owns the tree: the window asks and draws whatever comes back, and
    /// never rearranges its own copy. A window that moved the pane itself and then
    /// received a different layout would flicker, which is the failure this whole
    /// architecture exists to avoid.
    RelocatePane {
        moved: PaneId,
        target: PaneId,
        zone: DropZone,
    },
    OpenNodePane {
        surface_id: String,
        session_id: SessionId,
        node_id: NodeId,
        target_pane_id: PaneId,
        placement: PanePlacement,
        remember: bool,
    },
    PromoteTemporaryPane {
        surface_id: String,
        session_id: SessionId,
        pane_id: PaneId,
        target_pane_id: PaneId,
        placement: PanePlacement,
        remember: bool,
    },
    CreatePane {
        target_pane_id: PaneId,
        placement: PanePlacement,
        pane: NewPane,
    },
    DuplicatePane {
        pane_id: PaneId,
    },
    ChangePaneKind {
        pane_id: PaneId,
        kind: PaneKind,
    },
    FloatPane {
        pane_id: PaneId,
        geometry: PaneGeometry,
    },
    DockPane {
        pane_id: PaneId,
    },
    SetFloatingPaneGeometry {
        pane_id: PaneId,
        geometry: PaneGeometry,
    },
    /// Closing a temporary view always keeps the Agent/Process alive.
    CloseTemporaryPane {
        session_id: SessionId,
        pane_id: PaneId,
    },
    /// Close a sheet.
    CloseOverlay,

    /// Forget the scrollback Turn kept for one pane, from that pane's own menu.
    ///
    /// Turn's record only. The screen belongs to the program in the pane, and clearing that
    /// would mean typing into whatever is running.
    ClearPaneHistory {
        pane_id: PaneId,
    },
    /// Open a link found in a pane's output.
    ///
    /// Carries the whole [`terminal::links::LinkRequest`] rather than a URL, because whether
    /// this needs asking about first is a property of the link — a target whose visible text
    /// names a different host arrives with that warning attached — and the window is the
    /// thing that can ask.
    FollowLink(terminal::links::LinkRequest),
    /// Turn saying something in its own voice, from a pane that declined part of what the
    /// user asked for.
    Notice(String),
    /// The user said yes to the link they were asked about.
    ConfirmLink,
    /// The user said no, or pressed Escape.
    DismissLink,

    /// Record one preference at one level.
    ///
    /// The level is explicit and comes from the control the user used, never from what is
    /// selected: "set the font size" is four different acts, and a window that guessed would
    /// be the one that silently edited the wrong one.
    SetSetting {
        scope: turn_core::settings::Scope,
        owner_id: String,
        key: String,
        value: serde_json::Value,
    },
    /// Bind, unbind or reset one command's chord.
    ///
    /// The chord arrives as the text the user typed, unparsed. Parsing it here would put a
    /// second reader of the chord grammar in the window; `Overrides::from_settings` already
    /// has one, and it reports what it could not read rather than dropping it.
    RebindCommand {
        command: String,
        /// The chord as written, `""` to unbind, or [`DEFAULT_CHORD`] to go back to Turn's own.
        chord: String,
    },
    /// Remove one level's opinion, so the level below is in force again.
    ResetSetting {
        scope: turn_core::settings::Scope,
        owner_id: String,
        key: String,
    },
}

/// What one toolbar button does when it is pressed.
///
/// Two of them open a draft in [`ViewState`] rather than sending a command, because
/// creating a Workspace or a Session is a form the user fills in and not a request the
/// window can make on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolbarIntent {
    Run(Command),
    NewWorkspace,
    /// The layout presets, as a menu: there are five of them and a toolbar button
    /// cannot mean five things.
    LayoutMenu,
}

/// One button of the top bar's toolbar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolbarButton {
    pub icon: &'static str,
    /// The words. An icon on its own would convey the action by appearance, which this
    /// project does not allow: this is the accessible name and the tooltip.
    pub label: &'static str,
    pub intent: ToolbarIntent,
}

/// The toolbar, in the order the buttons are drawn and the reverse of the order they
/// are dropped when the window is too narrow for all of them.
///
/// Everything here already exists as a command or as a visible control somewhere else.
/// The toolbar is not new capability; it is the capability Turn accumulated, in one
/// place a person can find without reading documentation.
pub const TOOLBAR: &[ToolbarButton] = &[
    ToolbarButton {
        icon: icons::PLUS_SQUARE,
        label: "New pane",
        intent: ToolbarIntent::Run(Command::SplitHorizontal),
    },
    ToolbarButton {
        icon: icons::LAYOUT,
        label: "Layout",
        intent: ToolbarIntent::LayoutMenu,
    },
    ToolbarButton {
        icon: icons::FOLDER_PLUS,
        label: "New workspace",
        intent: ToolbarIntent::NewWorkspace,
    },
    ToolbarButton {
        icon: icons::COMMAND,
        label: "Command palette",
        intent: ToolbarIntent::Run(Command::OpenPalette),
    },
    ToolbarButton {
        icon: icons::BELL,
        label: "Attention queue",
        intent: ToolbarIntent::Run(Command::ToggleAttentionPanel),
    },
    ToolbarButton {
        icon: icons::KEYBOARD,
        label: "Keyboard shortcuts",
        intent: ToolbarIntent::Run(Command::ShowKeyboardShortcuts),
    },
    ToolbarButton {
        icon: icons::GEAR,
        label: "Settings",
        intent: ToolbarIntent::Run(Command::OpenSettings),
    },
];

/// How many toolbar buttons fit in `width`.
///
/// Worked out before anything is drawn, and the reason is a failure two earlier
/// snapshots caught: egui draws a widget wherever the cursor has reached, including past
/// the edge of the region it was given, so a row that ran out of room overlapped the
/// text beside it instead of stopping. Buttons are dropped from the end, which is why
/// [`TOOLBAR`] is ordered by how much the action is worth.
pub fn toolbar_capacity(width: f32) -> usize {
    if width < icons::SIZE.x {
        return 0;
    }
    // The last button needs its own width but not the gap that would follow it.
    let fits = ((width + (icons::PITCH - icons::SIZE.x)) / icons::PITCH).floor();
    (fits.max(0.0) as usize).min(TOOLBAR.len())
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

/// The same thing for a region that exists once per pane or per workspace.
///
/// The key has to be in the salt: three pane headers each holding one close button would
/// otherwise share that button's id, and with it its hover and press state — closing the
/// first pane would highlight the third.
fn keyed_region(rect: Rect, name: &'static str, key: &str) -> egui::UiBuilder {
    egui::UiBuilder::new().max_rect(rect).id_salt((name, key))
}

const SIDEBAR_WIDTH: f32 = 344.0;
const INSPECTOR_WIDTH: f32 = 264.0;
/// The top bar: the connection, the toolbar of actions, and the version.
///
/// Taller than the 26 points it used to be because it now carries real controls rather
/// than only text. A 26-point bar with 22-point buttons in it leaves one point of
/// padding, and the buttons touch the border.
const STATUS_HEIGHT: f32 = 32.0;
/// The bar along the bottom of the window.
const WINDOW_STATUS_HEIGHT: f32 = 26.0;
const ROW_HEIGHT: f32 = 40.0;
const PANE_HEADER: f32 = 22.0;
/// The room the close control needs at the right of a pane header.
const PANE_CLOSE_WIDTH: f32 = 18.0;
/// The gap between a row's last control and the right edge of the tree.
const ROW_ACTION_MARGIN: f32 = 6.0;
/// How far below the top of a row its controls sit.
///
/// On the *first* line, deliberately, and that is what lets a row carry three controls
/// without narrowing the second line at all: the name and the status tag share the first
/// line with the buttons and have to make room for them, while the detail line underneath
/// keeps the full width of the tree.
const ROW_ACTION_TOP: f32 = 3.0;

/// How many controls a row of the tree carries.
///
/// A Workspace row carries three — new session, archive, close — and a Session row two.
/// The count is a function of the row rather than of its state: a control that vanished
/// when it did not apply would teach nothing about why, so an inapplicable one is drawn
/// disabled with the reason in its tooltip, and the room it needs is reserved either way.
fn row_action_count(row: HierarchyRow<'_>) -> usize {
    match row {
        HierarchyRow::Workspace(_) => 3,
        HierarchyRow::Session { .. } => 2,
        // A worker an agent is managing carries one: the way to stop it.
        //
        // Every other Process row carries none, and that difference is the point rather than an
        // inconsistency. An agent can be managing a dozen subagents and processes at once, and
        // the row is where the user finds out one of them has gone quiet — so it is also where
        // they should be able to do something about it, without a right-click and a menu. A shell
        // or the agent itself is not in that position: stopping either is a decision about the
        // pane, and it stays in the context menu where it always was.
        //
        // Decided by kind rather than by state: the control is there whether the worker is busy
        // or idle, because a control that appeared only when Turn thought something was wrong
        // would make its absence a claim Turn cannot support.
        HierarchyRow::Process { session, node } => {
            usize::from(crate::spotlight::is_managed(session, node))
        }
    }
}

/// The widest a row's status tag gets, for deciding beforehand whether the row can afford
/// its controls.
///
/// The tag itself is measured when it is painted — this is the allowance the *decision* is
/// made against, so a Session's controls do not appear and disappear as its `YOUR TURN`
/// comes and goes.
const TAG_COLUMN: f32 = 94.0;
/// The least room a row's name may be left with before its controls give way.
///
/// Enough for a dozen characters and the ellipsis, which is the difference between a name a
/// person can recognise and one they cannot.
const ROW_MIN_TITLE: f32 = 96.0;

/// Where a row's text starts, measured from the left of the row.
fn row_text_x(row: HierarchyRow<'_>) -> f32 {
    // The caret sits in the indent; the text begins after it.
    9.0 + row.depth() as f32 * 14.0 + 15.0
}

/// The room a row's controls need at its right-hand end, or nothing when the row is too
/// narrow to afford them.
///
/// Reserved inside [`hierarchy_row`] rather than left to the buttons, because the row's
/// name and its status tag are painted, not laid out, and painted text does not move
/// aside for a widget drawn on top of it.
///
/// A tree narrow enough that its rows would be left with no room for a name gives the
/// controls up instead of the name: `Fix cli…` identifies nothing, while every one of
/// these acts is also on the row's context menu and on the keyboard. Worked out from the
/// row's width before anything is drawn, for the same reason `toolbar_capacity` is — and
/// from the width the *tag* case needs, so the controls do not appear and disappear as a
/// Session's `YOUR TURN` comes and goes.
fn row_action_width(row: HierarchyRow<'_>, row_width: f32) -> f32 {
    let wanted = match row_action_count(row) {
        0 => return 0.0,
        count => count as f32 * icons::ROW_PITCH + ROW_ACTION_MARGIN,
    };
    if row_width - row_text_x(row) - TAG_COLUMN - wanted >= ROW_MIN_TITLE {
        wanted
    } else {
        0.0
    }
}

/// Paints one line of text, abbreviated with an ellipsis when it does not fit.
///
/// The rows of the tree are painted rather than laid out, and a clip rectangle cuts at
/// whatever pixel it reaches: `Fix climbing bugs` becomes `Fix climbing bug`, which is a
/// different name and says nothing about being shortened. An ellipsis says so. This is the
/// failure two rounds of recorded screenshots caught, and the rows have less room now that
/// they carry controls, so it is worth the galley.
fn paint_line(
    painter: &egui::Painter,
    at: egui::Pos2,
    max_width: f32,
    text: &str,
    font: FontId,
    colour: Color32,
) {
    let mut job = egui::text::LayoutJob::single_section(
        text.to_string(),
        egui::TextFormat {
            font_id: font,
            color: colour,
            ..Default::default()
        },
    );
    job.wrap = egui::text::TextWrapping {
        max_width,
        max_rows: 1,
        // Anywhere rather than at a word boundary: a name is one long word as often as not,
        // and dropping the whole of it would be worse than abbreviating it.
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    painter.galley(at, painter.layout_job(job), colour);
}

/// Where the `index`-th control of a row goes, counted from the right-hand edge.
///
/// Counted from the right so the destructive one is furthest from the name and always in
/// the same place, whatever else the row happens to carry.
fn row_action_slot(row: Rect, index: usize) -> Rect {
    let right = row.max.x - ROW_ACTION_MARGIN - index as f32 * icons::ROW_PITCH;
    Rect::from_min_size(
        egui::pos2(right - icons::ROW_SIZE.x, row.min.y + ROW_ACTION_TOP),
        icons::ROW_SIZE,
    )
}

/// The window's own version, shown at the right of the top bar.
///
/// The window's, not the daemon's: the daemon's version and pid are already in the
/// connection sentence a few characters to the left, and the two disagreeing is exactly
/// the thing a user needs to be able to see.
const WINDOW_VERSION: &str = env!("CARGO_PKG_VERSION");
const TURN_LOGO_PNG: &[u8] = include_bytes!("../assets/turn-icon.png");

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

    fn height(self, visibility: TreeVisibilityMode) -> f32 {
        match self {
            Self::Workspace(_) => 34.0,
            Self::Session { .. } => 46.0,
            Self::Process { .. } => match visibility {
                TreeVisibilityMode::Normal => 40.0,
                TreeVisibilityMode::Expanded => 56.0,
                TreeVisibilityMode::Technical => 58.0,
            },
        }
    }

    fn accessible_name(self, focused_pane: bool, visibility: TreeVisibilityMode) -> String {
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
                if summary.archived {
                    name.push_str(" — archived");
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
                if summary.status == SessionStatus::Archived {
                    name.push_str(" — archived");
                }
                if let Some(guard) = read_only_guard_label(summary) {
                    name.push_str(&format!(" — {guard}"));
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
                // A title the process printed is announced as such. A screen-reader
                // user gets the same caveat a sighted one gets from the styling, and
                // the point is the same: this text is the program's word about itself.
                if node.title_is_provisional {
                    name.push_str(" — title set by the process");
                }
                if let Some(preview) = visible_preview(node) {
                    if visibility == TreeVisibilityMode::Expanded {
                        name.push_str(&format!(" — {}", preview.normalized_text));
                    }
                }
                if visibility == TreeVisibilityMode::Technical {
                    name.push_str(&format!(
                        " — pid {} — parent pid {} — command {}",
                        node.pid
                            .map_or_else(|| "unknown".into(), |pid| pid.to_string()),
                        node.ppid
                            .map_or_else(|| "unknown".into(), |pid| pid.to_string()),
                        node.command
                    ));
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

fn relationship_kind_label(kind: RelationshipKind) -> &'static str {
    match kind {
        RelationshipKind::SpawnedBy => "Spawned by",
        RelationshipKind::OwnsProcess => "Owns process",
        RelationshipKind::Related => "Related",
        RelationshipKind::Unknown => "Session root",
    }
}

fn relationship_would_cycle(
    nodes: &[TreeNodeView],
    child: &NodeId,
    candidate_parent: &NodeId,
) -> bool {
    let mut cursor = Some(candidate_parent);
    let mut visited = HashSet::new();
    while let Some(node_id) = cursor {
        if node_id == child {
            return true;
        }
        if !visited.insert(node_id.clone()) {
            return true;
        }
        cursor = nodes
            .iter()
            .find(|node| &node.node_id == node_id)
            .and_then(|node| node.parent.as_ref());
    }
    false
}

/// The node's title, as the daemon resolved it.
///
/// This used to pick between the agent's display name and the node title, which was
/// the UI applying its own precedence. `TreeNodeView::title` now arrives already
/// resolved — user name, then declared name, then process title, then command — so
/// there is one order and the window cannot disagree with the daemon about it.
fn read_only_guard_label(summary: &SessionSummary) -> Option<&'static str> {
    if summary.mode != SessionMode::ReadOnly {
        return None;
    }
    Some(if summary.read_only_enforced {
        "read-only guard enforced; checkout writes blocked"
    } else {
        "read-only guard unavailable; processes disabled"
    })
}

fn process_title(node: &TreeNodeView) -> &str {
    &node.title
}

/// A visual explanation only; the daemon repeats every check authoritatively at
/// preparation and delivery. In particular, a semantic subagent may be visible in the
/// tree without owning a PTY Turn can type into.
fn context_target_unavailable_reason(node: &TreeNodeView) -> Option<&'static str> {
    if !node.is_agentic {
        return Some("not an Agent");
    }
    if !matches!(node.lifecycle, Lifecycle::Alive | Lifecycle::Reconnected) {
        return Some("Agent is not running under Turn");
    }
    if !matches!(node.pane_capability, NodePaneCapability::Terminal { .. }) {
        return Some("Agent has no controllable PTY");
    }
    if node.interaction_pending
        || node.agent.as_ref().is_some_and(|agent| {
            agent.pending_permission.is_some() || agent.pending_question.is_some()
        })
    {
        return Some("resolve its pending response first");
    }
    if !matches!(
        node.turn.as_ref(),
        Some(Turn::Idle | Turn::Done | Turn::TaskDone)
    ) {
        return Some("wait for its current turn to finish");
    }
    None
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

/// The rows the tree shows, in order.
///
/// `include_archived` is the preference from Settings, and it is applied here as well as
/// in the request that fetched the snapshot. Both, deliberately: the request is what makes
/// the daemon send the archived rows at all, and this is what makes archiving *look* like
/// it worked. Without the local half, a row the user just archived would sit in the tree
/// until a snapshot happened to arrive, and an action whose effect you cannot see is an
/// action a user will try again — this time reaching for Close.
fn visible_hierarchy_rows<'a>(
    snapshot: &'a HierarchySnapshot,
    state: &ViewState,
    include_archived: bool,
) -> Vec<HierarchyRow<'a>> {
    let archived_filter = state.tree_filters.contains(&TreeFilter::Archived);
    let ordered = ordered_hierarchy_rows(
        snapshot,
        include_archived || archived_filter,
        effective_manual_order(snapshot, state),
    );
    let query = state.tree_query.trim().to_ascii_lowercase();
    let filtering = !query.is_empty() || !state.tree_filters.is_empty();

    // Search/filter results are a tree projection, not a flat list: keep every ancestor of
    // every result. This preserves both spatial context and complete keyboard traversal.
    let mut parents = HashMap::new();
    let mut retained = HashSet::new();
    for row in &ordered {
        let key = row.key();
        if let Some(parent) = row.parent_key() {
            parents.insert(key.clone(), parent);
        }
        let ephemeral_hidden = matches!(row, HierarchyRow::Process { node, .. } if node.ephemeral)
            && state.tree_visibility != TreeVisibilityMode::Technical
            && query.is_empty();
        if !ephemeral_hidden
            && row_matches_query(*row, &query)
            && row_matches_filters(*row, &state.tree_filters)
        {
            retained.insert(key);
        }
    }
    if filtering {
        let matches: Vec<_> = retained.iter().cloned().collect();
        for mut key in matches {
            while let Some(parent) = parents.get(&key).cloned() {
                retained.insert(parent.clone());
                key = parent;
            }
        }
    }

    let mut rows = Vec::new();
    let mut collapsed_depth: Option<usize> = None;
    let mut hidden_ephemeral_depth: Option<usize> = None;
    for row in ordered {
        let depth = row.depth();
        if let Some(hidden) = hidden_ephemeral_depth {
            if depth > hidden {
                continue;
            }
            hidden_ephemeral_depth = None;
        }
        if let HierarchyRow::Process { node, .. } = row {
            if node.ephemeral
                && state.tree_visibility != TreeVisibilityMode::Technical
                && query.is_empty()
            {
                hidden_ephemeral_depth = Some(depth);
                continue;
            }
        }
        if filtering {
            if retained.contains(&row.key()) {
                rows.push(row);
            }
            continue;
        }
        if let Some(collapsed) = collapsed_depth {
            if depth > collapsed {
                continue;
            }
            collapsed_depth = None;
        }
        let key = row.key();
        rows.push(row);
        if row.child_count() > 0 && !row_is_expanded(snapshot, state, &key) {
            collapsed_depth = Some(depth);
        }
    }
    rows
}

/// Full hierarchy order before expansion/filter projection. Every sibling family uses the
/// same persisted rank list; unknown/new rows stay in daemon order after ranked siblings.
fn ordered_hierarchy_rows<'a>(
    snapshot: &'a HierarchySnapshot,
    include_archived: bool,
    manual_order: &[HierarchyKey],
) -> Vec<HierarchyRow<'a>> {
    let rank: HashMap<HierarchyKey, usize> = manual_order
        .iter()
        .cloned()
        .enumerate()
        .map(|(index, key)| (key, index))
        .collect();
    let rank_of = |key: &HierarchyKey| rank.get(key).copied().unwrap_or(usize::MAX);

    let mut workspaces: Vec<_> = snapshot.workspaces.iter().enumerate().collect();
    workspaces.sort_by_key(|(index, workspace)| {
        (
            rank_of(&HierarchyKey::workspace(workspace.workspace.id.clone())),
            *index,
        )
    });
    let mut rows = Vec::new();
    for (_, workspace) in workspaces {
        if workspace.workspace.archived && !include_archived {
            continue;
        }
        rows.push(HierarchyRow::Workspace(workspace));
        let mut sessions: Vec<_> = workspace.sessions.iter().enumerate().collect();
        sessions.sort_by_key(|(index, session)| {
            (
                rank_of(&HierarchyKey::session(session.session.id.clone())),
                *index,
            )
        });
        for (_, session) in sessions {
            if session.session.status == SessionStatus::Archived && !include_archived {
                continue;
            }
            rows.push(HierarchyRow::Session { workspace, session });
            append_ordered_process_rows(session, &rank, &mut rows);
        }
    }
    rows
}

fn effective_manual_order<'a>(
    snapshot: &'a HierarchySnapshot,
    state: &'a ViewState,
) -> &'a [HierarchyKey] {
    if state.tree_manual_order.is_empty() {
        &snapshot.tree_state.manual_order
    } else {
        &state.tree_manual_order
    }
}

fn append_ordered_process_rows<'a>(
    session: &'a SessionTreeView,
    rank: &HashMap<HierarchyKey, usize>,
    rows: &mut Vec<HierarchyRow<'a>>,
) {
    let known: HashSet<_> = session
        .nodes
        .iter()
        .map(|node| node.node_id.clone())
        .collect();
    let mut children: HashMap<Option<NodeId>, Vec<(usize, &'a TreeNodeView)>> = HashMap::new();
    for (index, node) in session.nodes.iter().enumerate() {
        let parent = node.parent.clone().filter(|parent| known.contains(parent));
        children.entry(parent).or_default().push((index, node));
    }
    for siblings in children.values_mut() {
        siblings.sort_by_key(|(index, node)| {
            (
                rank.get(&HierarchyKey::process(node.node_id.clone()))
                    .copied()
                    .unwrap_or(usize::MAX),
                *index,
            )
        });
    }
    let mut stack: Vec<_> = children
        .get(&None)
        .into_iter()
        .flatten()
        .map(|(_, node)| *node)
        .rev()
        .collect();
    let mut visited = HashSet::new();
    while let Some(node) = stack.pop() {
        if !visited.insert(node.node_id.clone()) {
            continue;
        }
        rows.push(HierarchyRow::Process { session, node });
        if let Some(descendants) = children.get(&Some(node.node_id.clone())) {
            stack.extend(descendants.iter().rev().map(|(_, child)| *child));
        }
    }
    // Corrupt/cyclic input remains visible and bounded rather than disappearing.
    let mut leftovers: Vec<_> = session
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| !visited.contains(&node.node_id))
        .collect();
    leftovers.sort_by_key(|(index, node)| {
        (
            rank.get(&HierarchyKey::process(node.node_id.clone()))
                .copied()
                .unwrap_or(usize::MAX),
            *index,
        )
    });
    rows.extend(
        leftovers
            .into_iter()
            .map(|(_, node)| HierarchyRow::Process { session, node }),
    );
}

fn row_matches_query(row: HierarchyRow<'_>, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let contains = |value: &str| value.to_ascii_lowercase().contains(query);
    match row {
        HierarchyRow::Workspace(workspace) => {
            contains(&workspace.workspace.name)
                || contains(&workspace.workspace.root)
                || workspace
                    .workspace
                    .git_remote
                    .as_deref()
                    .is_some_and(contains)
        }
        HierarchyRow::Session { session, .. } => {
            let summary = &session.session;
            contains(&summary.name)
                || contains(&summary.cwd)
                || summary.note.as_deref().is_some_and(contains)
                || summary.git_branch.as_deref().is_some_and(contains)
                || summary.tags.iter().any(|tag| contains(tag))
        }
        HierarchyRow::Process { node, .. } => {
            contains(&node.title)
                || contains(&node.command)
                || contains(&node.cwd)
                || node.args.iter().any(|arg| contains(arg))
                || visible_preview(node).is_some_and(|preview| contains(&preview.normalized_text))
                || node.agent.as_ref().is_some_and(|agent| {
                    contains(&agent.name.display_name)
                        || agent.current_task.as_deref().is_some_and(contains)
                        || agent.last_message.as_deref().is_some_and(contains)
                })
        }
    }
}

fn row_matches_filters(row: HierarchyRow<'_>, filters: &BTreeSet<TreeFilter>) -> bool {
    if filters.is_empty() {
        return true;
    }
    let has_any = |family: &[TreeFilter]| family.iter().any(|filter| filters.contains(filter));
    let state_family = [
        TreeFilter::Attention,
        TreeFilter::Running,
        TreeFilter::Failed,
        TreeFilter::Idle,
        TreeFilter::Completed,
    ];
    let kind_family = [TreeFilter::Agents, TreeFilter::Tools];
    let mode_family = [TreeFilter::Main, TreeFilter::ReadOnly, TreeFilter::Worktree];

    let state_ok = !has_any(&state_family)
        || state_family
            .iter()
            .filter(|filter| filters.contains(filter))
            .any(|filter| row_matches_state(row, *filter));
    let kind_ok = !has_any(&kind_family)
        || kind_family
            .iter()
            .filter(|filter| filters.contains(filter))
            .any(|filter| row_matches_kind(row, *filter));
    let mode_ok = !has_any(&mode_family)
        || mode_family
            .iter()
            .filter(|filter| filters.contains(filter))
            .any(|filter| row_matches_mode(row, *filter));
    let archived_ok = !filters.contains(&TreeFilter::Archived)
        || match row {
            HierarchyRow::Workspace(workspace) => workspace.workspace.archived,
            HierarchyRow::Session { session, .. } | HierarchyRow::Process { session, .. } => {
                session.session.status == SessionStatus::Archived
            }
        };
    state_ok && kind_ok && mode_ok && archived_ok
}

fn row_matches_state(row: HierarchyRow<'_>, filter: TreeFilter) -> bool {
    match row {
        HierarchyRow::Workspace(workspace) => match filter {
            TreeFilter::Attention => workspace.workspace.sessions_needing_user > 0,
            TreeFilter::Running => workspace
                .sessions
                .iter()
                .any(|session| session.session.running_count > 0),
            TreeFilter::Failed => workspace
                .sessions
                .iter()
                .any(|session| session.session.display_state == DisplayState::Failed),
            TreeFilter::Idle => workspace
                .sessions
                .iter()
                .all(|session| session.session.running_count == 0),
            TreeFilter::Completed => false,
            _ => true,
        },
        HierarchyRow::Session { session, .. } => match filter {
            TreeFilter::Attention => session.session.needs_user || session.session.badge_count > 0,
            TreeFilter::Running => session.session.running_count > 0,
            TreeFilter::Failed => session.session.display_state == DisplayState::Failed,
            TreeFilter::Idle => session.session.display_state == DisplayState::Idle,
            TreeFilter::Completed => matches!(
                session.session.display_state,
                DisplayState::CompletedTurn | DisplayState::CompletedTask | DisplayState::Stopped
            ),
            _ => true,
        },
        HierarchyRow::Process { node, .. } => match filter {
            TreeFilter::Attention => node.needs_user,
            TreeFilter::Running => node.lifecycle.is_running(),
            TreeFilter::Failed => {
                node.display_state == DisplayState::Failed || node.lifecycle.is_failure()
            }
            TreeFilter::Idle => node.display_state == DisplayState::Idle,
            TreeFilter::Completed => matches!(
                node.display_state,
                DisplayState::CompletedTurn | DisplayState::CompletedTask | DisplayState::Stopped
            ),
            _ => true,
        },
    }
}

fn row_matches_kind(row: HierarchyRow<'_>, filter: TreeFilter) -> bool {
    match row {
        HierarchyRow::Process { node, .. } => match filter {
            TreeFilter::Agents => node.is_agentic,
            TreeFilter::Tools => !node.is_agentic,
            _ => true,
        },
        HierarchyRow::Workspace(_) | HierarchyRow::Session { .. } => false,
    }
}

fn row_matches_mode(row: HierarchyRow<'_>, filter: TreeFilter) -> bool {
    let mode = match row {
        HierarchyRow::Session { session, .. } | HierarchyRow::Process { session, .. } => {
            session.session.mode
        }
        HierarchyRow::Workspace(_) => return false,
    };
    matches!(
        (filter, mode),
        (TreeFilter::Main, SessionMode::MainCheckout)
            | (TreeFilter::ReadOnly, SessionMode::ReadOnly)
            | (TreeFilter::Worktree, SessionMode::IsolatedWorktree)
    )
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
    fn preferred_pane_placement(&self) -> PanePlacement {
        match self
            .settings
            .and_then(|settings| settings.entry(OPEN_PANE_PLACEMENT_KEY))
            .and_then(|entry| entry.resolution.value.as_str())
        {
            Some("replace_current") => PanePlacement::ReplaceCurrent,
            Some("split_below") => PanePlacement::SplitBelow,
            Some("temporary") => PanePlacement::Temporary,
            _ => PanePlacement::SplitRight,
        }
    }

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
        // Idempotent, and here rather than at startup so no caller of this view — the
        // application, a snapshot harness, a future second window — can forget it and
        // draw a toolbar of missing-glyph boxes.
        icons::install(ui.ctx());

        // Escape abandons a pane drag — `egui` has already dropped the gesture by the time
        // this runs — and the press is spent here rather than left to fall through to the
        // handlers below. Cancelling a rearrangement must not have the side effect of
        // closing a temporary pane the user was reading; a gesture people are afraid to
        // start is one they will not use. A second press, with no drag in progress, reaches
        // those handlers normally.
        if state.dragged_pane.is_some()
            && ui.input_mut(|input| input.consume_key(Modifiers::NONE, Key::Escape))
        {
            state.dragged_pane = None;
        }

        // Temporarily take the snapshot so the UI may update its local interaction
        // state without cloning the complete process tree every frame.
        let hierarchy = state.hierarchy.take();
        let incoming_tree_state = hierarchy
            .as_ref()
            .map(|snapshot| snapshot.tree_state.clone());
        if incoming_tree_state != state.observed_tree_state {
            let first_observation = state.observed_tree_state.is_none();
            let previous_selection = state
                .observed_tree_state
                .as_ref()
                .and_then(|tree| tree.selected.as_ref());
            let next_selection = incoming_tree_state
                .as_ref()
                .and_then(|tree| tree.selected.as_ref());
            if first_observation {
                state.scroll_tree_to = incoming_tree_state
                    .as_ref()
                    .and_then(|tree| tree.scroll_anchor.clone())
                    .or_else(|| next_selection.cloned());
            } else if previous_selection != next_selection {
                state.scroll_tree_to = next_selection.cloned();
            }
            state.selected_tree = None;
            state.tree_expansion.clear();
            if let Some(tree) = &incoming_tree_state {
                state.tree_filters = tree.filters.iter().copied().collect();
                state.tree_visibility = tree.visibility_mode;
                state.tree_scroll_anchor = tree.scroll_anchor.clone();
                state.tree_manual_order = tree.manual_order.clone();
            }
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

        actions.extend(self.status_bar(ui, theme, keymap, hierarchy.as_ref(), state));
        if let Some(permission) = &self.permission {
            actions.extend(self.permission_banner(ui, theme, permission));
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

        // The bottom bar is taken out of the body before anything else is placed, so it
        // is a bar the window has rather than a strip that pushes the panes around when
        // recovery appears.
        let remaining = ui.available_rect_before_wrap();
        let status_height = WINDOW_STATUS_HEIGHT.min(remaining.height());
        let bottom_status = Rect::from_min_size(
            remaining.left_bottom() - Vec2::new(0.0, status_height),
            Vec2::new(remaining.width(), status_height),
        );
        let body = Rect::from_min_max(remaining.min, bottom_status.right_top());
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
                    actions.extend(self.hierarchy_sidebar(ui, theme, keymap, snapshot, state));
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
        let context_rect =
            Rect::from_min_size(centre.min, Vec2::new(centre.width(), context_height));
        let pane_rect = Rect::from_min_max(centre.min + Vec2::new(0.0, context_height), centre.max);
        if let Some((workspace, session)) = active_context {
            ui.scope_builder(region(context_rect, "session-context"), |ui| {
                actions.extend(self.session_context_bar(ui, theme, workspace, session, state));
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
        actions.extend(self.floating_panes(ui, theme, keymap, state));
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
        if status_height > 0.0 {
            actions.extend(self.window_status_bar(
                ui,
                theme,
                bottom_status,
                active_context.map(|(_, session)| session),
            ));
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

        if state.pane_placement.is_some() {
            actions.extend(self.pane_placement_overlay(ui, theme, state, full));
        } else if state.new_pane.is_some() {
            actions.extend(self.new_pane_overlay(ui, theme, state, full));
        } else if state.node_edit.is_some() {
            if let Some(snapshot) = hierarchy.as_ref() {
                actions.extend(self.node_edit_overlay(ui, theme, snapshot, state, full));
            } else {
                state.node_edit = None;
            }
        } else if state.context_handoff.is_some() {
            if let Some(snapshot) = hierarchy.as_ref() {
                actions.extend(self.context_handoff_overlay(ui, theme, snapshot, state, full));
            } else {
                state.context_handoff = None;
            }
        } else if state.lifecycle_confirmation.is_some() {
            actions.extend(self.lifecycle_confirmation_overlay(ui, theme, state, full));
        } else if let Some(link) = self.link_confirmation {
            actions.extend(self.link_confirmation_overlay(ui, theme, link, full));
        } else if state.layout_draft.is_some() {
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
            actions.extend(self.shortcuts_sheet(ui, theme, keymap, state, full));
        } else if state.settings_open {
            actions.extend(self.settings_sheet(ui, theme, state, full));
        }
        state.hierarchy = hierarchy;
        actions
    }

    fn pane_placement_overlay(
        &self,
        ui: &mut Ui,
        theme: &Theme,
        state: &mut ViewState,
        full: Rect,
    ) -> Vec<ViewAction> {
        let Some(mut draft) = state.pane_placement.take() else {
            return Vec::new();
        };
        let mut cancel = ui.input(|input| input.key_pressed(Key::Escape));
        let mut submit = false;
        let panel = Rect::from_center_size(
            full.center(),
            Vec2::new(
                460.0_f32.min((full.width() - 32.0).max(300.0)),
                292.0_f32.min((full.height() - 32.0).max(220.0)),
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
        ui.scope_builder(region(panel.shrink(20.0), "pane-placement"), |ui| {
            let promotion = matches!(draft.source, PanePlacementSource::Temporary { .. });
            ui.heading(if promotion {
                "Keep this Pane in the layout"
            } else {
                "Open as Pane"
            });
            ui.label(
                RichText::new(
                    "Choose the placement once. Turn will reuse it next time unless you change it here.",
                )
                .color(theme.text_dim),
            );
            ui.add_space(10.0);
            for (label, placement) in [
                ("Replace current", PanePlacement::ReplaceCurrent),
                ("Split right", PanePlacement::SplitRight),
                ("Split below", PanePlacement::SplitBelow),
            ] {
                ui.radio_value(&mut draft.placement, placement, label);
            }
            if !promotion {
                ui.radio_value(
                    &mut draft.placement,
                    PanePlacement::Temporary,
                    "Temporary — do not change the saved layout",
                );
            }
            ui.add_space(6.0);
            ui.checkbox(&mut draft.remember, "Remember this placement");
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui.button(if promotion { "Keep Pane" } else { "Open" }).clicked() {
                    submit = true;
                }
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
            });
        });

        if submit {
            return vec![match draft.source {
                PanePlacementSource::Node {
                    surface_id,
                    session_id,
                    node_id,
                } => ViewAction::OpenNodePane {
                    surface_id,
                    session_id,
                    node_id,
                    target_pane_id: draft.target_pane_id,
                    placement: draft.placement,
                    remember: draft.remember,
                },
                PanePlacementSource::Temporary {
                    surface_id,
                    session_id,
                    pane_id,
                } => ViewAction::PromoteTemporaryPane {
                    surface_id,
                    session_id,
                    pane_id,
                    target_pane_id: draft.target_pane_id,
                    placement: draft.placement,
                    remember: draft.remember,
                },
            }];
        }
        if !cancel {
            state.pane_placement = Some(draft);
        }
        Vec::new()
    }

    fn new_pane_overlay(
        &self,
        ui: &mut Ui,
        theme: &Theme,
        state: &mut ViewState,
        full: Rect,
    ) -> Vec<ViewAction> {
        let Some(mut draft) = state.new_pane.take() else {
            return Vec::new();
        };
        let mut cancel = ui.input(|input| input.key_pressed(Key::Escape));
        let mut submit = false;
        let panel = Rect::from_center_size(
            full.center(),
            Vec2::new(
                560.0_f32.min((full.width() - 32.0).max(320.0)),
                440.0_f32.min((full.height() - 32.0).max(300.0)),
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
        ui.scope_builder(region(panel.shrink(20.0), "new-pane"), |ui| {
            ui.heading("New Pane");
            ui.label(
                RichText::new("Run any executable directly; arguments are parsed without a shell.")
                    .color(theme.text_dim),
            );
            ui.add_space(8.0);
            egui::ComboBox::from_label("View type")
                .selected_text(format!("{:?}", draft.kind))
                .show_ui(ui, |ui| {
                    for (label, kind) in pane_kind_choices() {
                        ui.selectable_value(&mut draft.kind, kind, label);
                    }
                });
            ui.horizontal(|ui| {
                ui.label("Title");
                ui.text_edit_singleline(&mut draft.title);
            });
            ui.horizontal(|ui| {
                ui.label("Program");
                ui.text_edit_singleline(&mut draft.program);
            });
            ui.horizontal(|ui| {
                ui.label("Arguments");
                ui.text_edit_singleline(&mut draft.arguments);
            });
            ui.horizontal(|ui| {
                ui.label("Working directory");
                ui.text_edit_singleline(&mut draft.cwd);
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.radio_value(
                    &mut draft.placement,
                    PanePlacement::ReplaceCurrent,
                    "Replace current",
                );
                ui.radio_value(
                    &mut draft.placement,
                    PanePlacement::SplitRight,
                    "Split right",
                );
                ui.radio_value(
                    &mut draft.placement,
                    PanePlacement::SplitBelow,
                    "Split below",
                );
            });
            if let Some(error) = &draft.error {
                ui.label(RichText::new(error).color(theme.failure));
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button("Create Pane").clicked() {
                    submit = true;
                }
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
            });
        });

        if submit {
            match shell_words::split(draft.arguments.trim()) {
                Ok(args) => {
                    let mut pane = NewPane::new(draft.kind);
                    pane.title =
                        (!draft.title.trim().is_empty()).then(|| draft.title.trim().to_string());
                    pane.command = (!draft.program.trim().is_empty())
                        .then(|| draft.program.trim().to_string());
                    pane.args = args;
                    pane.cwd = (!draft.cwd.trim().is_empty()).then(|| draft.cwd.trim().to_string());
                    return vec![ViewAction::CreatePane {
                        target_pane_id: draft.target_pane_id,
                        placement: draft.placement,
                        pane,
                    }];
                }
                Err(error) => draft.error = Some(format!("Arguments: {error}")),
            }
        }
        if !cancel {
            state.new_pane = Some(draft);
        }
        Vec::new()
    }

    fn node_edit_overlay(
        &self,
        ui: &mut Ui,
        theme: &Theme,
        snapshot: &HierarchySnapshot,
        state: &mut ViewState,
        full: Rect,
    ) -> Vec<ViewAction> {
        let Some(mut draft) = state.node_edit.take() else {
            return Vec::new();
        };
        let mut cancel = ui.input(|input| input.key_pressed(Key::Escape));
        let mut submit = false;
        let panel = Rect::from_center_size(
            full.center(),
            Vec2::new(
                480.0_f32.min((full.width() - 32.0).max(300.0)),
                286.0_f32.min((full.height() - 32.0).max(220.0)),
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
        ui.scope_builder(region(panel.shrink(20.0), "agent-node-editor"), |ui| {
            ui.ctx().accesskit_node_builder(ui.id(), |node| {
                node.set_role(egui::accesskit::Role::Dialog);
                node.set_modal();
                node.set_label(match &draft {
                    NodeEditDraft::Rename { .. } => "Rename Agent",
                    NodeEditDraft::Relationship { .. } => "Correct Agent relationship",
                });
            });
            ui.heading(match &draft {
                NodeEditDraft::Rename { .. } => "Rename Agent",
                NodeEditDraft::Relationship { .. } => "Correct relationship",
            });
            ui.label(
                RichText::new(
                    "This is a durable user correction. Turn keeps the original integration fact in the audit log.",
                )
                .color(theme.text_dim),
            );
            ui.add_space(12.0);
            match &mut draft {
                NodeEditDraft::Rename { name, .. } => {
                    ui.label("Display name");
                    let response = ui.add(
                        egui::TextEdit::singleline(name)
                            .desired_width(f32::INFINITY)
                            .hint_text("Agent name"),
                    );
                    response.request_focus();
                    submit = !name.trim().is_empty()
                        && response.has_focus()
                        && ui.input(|input| input.key_pressed(Key::Enter));
                }
                NodeEditDraft::Relationship {
                    session_id,
                    node_id,
                    parent_node_id,
                    relationship_kind,
                } => {
                    let candidates = snapshot
                        .workspaces
                        .iter()
                        .flat_map(|workspace| &workspace.sessions)
                        .find(|session| &session.session.id == session_id)
                        .map(|session| &session.nodes[..])
                        .unwrap_or_default();
                    ui.label("Parent Agent");
                    egui::ComboBox::from_id_salt("correct-relationship-parent")
                        .selected_text(
                            parent_node_id
                                .as_ref()
                                .and_then(|parent| {
                                    candidates.iter().find(|node| &node.node_id == parent)
                                })
                                .map(|node| node.title.clone())
                                .unwrap_or_else(|| "Session root".into()),
                        )
                        .show_ui(ui, |ui| {
                            if ui
                                .selectable_label(parent_node_id.is_none(), "Session root")
                                .clicked()
                            {
                                *parent_node_id = None;
                                *relationship_kind = RelationshipKind::Unknown;
                            }
                            for candidate in candidates.iter().filter(|candidate| {
                                candidate.is_agentic
                                    && &candidate.node_id != node_id
                                    && !relationship_would_cycle(candidates, node_id, &candidate.node_id)
                            }) {
                                if ui
                                    .selectable_label(
                                        parent_node_id.as_ref() == Some(&candidate.node_id),
                                        &candidate.title,
                                    )
                                    .clicked()
                                {
                                    *parent_node_id = Some(candidate.node_id.clone());
                                    if *relationship_kind == RelationshipKind::Unknown {
                                        *relationship_kind = RelationshipKind::SpawnedBy;
                                    }
                                }
                            }
                        });
                    ui.label("Relationship");
                    ui.add_enabled_ui(parent_node_id.is_some(), |ui| {
                        egui::ComboBox::from_id_salt("correct-relationship-kind")
                            .selected_text(relationship_kind_label(*relationship_kind))
                            .show_ui(ui, |ui| {
                                for kind in [
                                    RelationshipKind::SpawnedBy,
                                    RelationshipKind::OwnsProcess,
                                    RelationshipKind::Related,
                                ] {
                                    ui.selectable_value(
                                        relationship_kind,
                                        kind,
                                        relationship_kind_label(kind),
                                    );
                                }
                            });
                    });
                }
            }
            let can_submit = match &draft {
                NodeEditDraft::Rename { name, .. } => !name.trim().is_empty(),
                NodeEditDraft::Relationship {
                    parent_node_id,
                    relationship_kind,
                    ..
                } => match parent_node_id {
                    Some(_) => *relationship_kind != RelationshipKind::Unknown,
                    None => *relationship_kind == RelationshipKind::Unknown,
                },
            };
            ui.with_layout(egui::Layout::bottom_up(egui::Align::RIGHT), |ui| {
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                    if ui
                        .add_enabled(can_submit, egui::Button::new("Save correction"))
                        .clicked()
                    {
                        submit = true;
                    }
                });
            });
        });

        if cancel {
            return Vec::new();
        }
        if submit {
            return vec![match draft {
                NodeEditDraft::Rename {
                    session_id,
                    node_id,
                    name,
                } => ViewAction::RenameNode {
                    session_id,
                    node_id,
                    name: name.trim().to_string(),
                },
                NodeEditDraft::Relationship {
                    session_id,
                    node_id,
                    parent_node_id,
                    relationship_kind,
                } => ViewAction::CorrectRelationship {
                    session_id,
                    node_id,
                    parent_node_id,
                    relationship_kind,
                },
            }];
        }
        state.node_edit = Some(draft);
        Vec::new()
    }

    /// The top bar: what Turn is talking to, what the user can do, and which build this
    /// is.
    ///
    /// Laid out in explicit zones rather than as one `horizontal`, and that is the whole
    /// reason this function is longer than it looks like it should be. A `horizontal`
    /// gives its first label as much room as the label wants, so a long disconnection
    /// sentence used to push the attention button off the right-hand edge — the failure
    /// mode this bar has had twice. Every zone here is measured, clipped and allowed to
    /// disappear, in the order of what matters least.
    fn status_bar(
        &self,
        ui: &mut Ui,
        theme: &Theme,
        keymap: &Keymap,
        hierarchy: Option<&HierarchySnapshot>,
        state: &mut ViewState,
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

        let (version_value, connection_detail) = match &connection {
            ConnectionState::Connected {
                daemon_version,
                daemon_pid,
                ..
            } => (daemon_version.as_str(), format!("pid {daemon_pid}")),
            _ => (WINDOW_VERSION, connection.detail()),
        };
        let version = format!("v{version_value}");

        // Identity and controls are one left-aligned cluster. The product mark is the same
        // checked-in artwork used by the Dock/window icon, so the app does not acquire a second
        // improvised logo in its own chrome.
        let texture_id = state
            .logo_texture
            .0
            .get_or_insert_with(|| {
                let icon = eframe::icon_data::from_png_bytes(TURN_LOGO_PNG)
                    .expect("the embedded Turn logo must be a valid PNG");
                let image = egui::ColorImage::from_rgba_unmultiplied(
                    [icon.width as usize, icon.height as usize],
                    &icon.rgba,
                );
                ui.ctx()
                    .load_texture("turn-product-mark", image, egui::TextureOptions::LINEAR)
            })
            .id();
        let logo = Rect::from_center_size(
            egui::pos2(rect.min.x + 19.0, rect.center().y),
            Vec2::splat(22.0),
        );
        ui.painter().image(
            texture_id,
            logo,
            Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
            Color32::WHITE,
        );
        let brand = ui.painter().text(
            egui::pos2(logo.max.x + 6.0, rect.center().y),
            Align2::LEFT_CENTER,
            "TURN",
            FontId::new(12.0, egui::FontFamily::Monospace),
            theme.text,
        );

        // Connection, process identity, attention and the single visible version are metadata,
        // so they live together at the right edge. The daemon's version is the visible version
        // while connected; showing the identical window version again conveyed no information.
        let metadata_wanted: f32 = if waiting > 0 { 450.0 } else { 330.0 };
        let metadata_width = metadata_wanted.min((rect.width() - 92.0).max(0.0));
        let metadata = Rect::from_min_max(
            egui::pos2(rect.max.x - metadata_width, rect.min.y + 4.0),
            egui::pos2(rect.max.x - 8.0, rect.max.y - 4.0),
        );
        if metadata.width() > 40.0 {
            ui.scope_builder(region(metadata, "status-metadata"), |ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        RichText::new(&version)
                            .monospace()
                            .color(theme.text_faint)
                            .small(),
                    );
                    ui.label(
                        RichText::new(&connection_detail)
                            .color(theme.text_faint)
                            .small(),
                    );
                    ui.label(
                        RichText::new(format!("{glyph} {}", connection.word()))
                            .color(colour)
                            .small(),
                    );
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
                    } else {
                        ui.label(
                            RichText::new("nothing waiting")
                                .color(theme.text_faint)
                                .small(),
                        );
                    }
                });
            });
        }

        // Every global control begins immediately after the mark and product name. Contextual
        // Session creation deliberately does not live here; each Workspace row owns that action.
        let toolbar_left = brand.max.x + 12.0;
        let toolbar = Rect::from_min_max(
            egui::pos2(toolbar_left, rect.min.y + 5.0),
            egui::pos2(metadata.min.x - 10.0, rect.max.y - 5.0),
        );
        let capacity = if toolbar.width() > 0.0 {
            toolbar_capacity(toolbar.width())
        } else {
            0
        };
        if capacity > 0 {
            ui.scope_builder(region(toolbar, "status-toolbar"), |ui| {
                ui.spacing_mut().item_spacing.x = icons::PITCH - icons::SIZE.x;
                ui.horizontal(|ui| {
                    for button in &TOOLBAR[..capacity] {
                        actions.extend(self.toolbar_button(ui, keymap, state, *button));
                    }
                });
            });
        }

        // One node for the whole bar, so a screen reader hears the sentence rather than
        // four unrelated fragments — and hears the version, which is painted.
        let status_id = ui.id().with("window-top-status");
        let announced = format!(
            "Turn {version_value}, {}, {}, {}",
            connection.word(),
            connection_detail,
            if waiting == 1 {
                "1 session needs you".to_string()
            } else if waiting > 0 {
                format!("{waiting} sessions need you")
            } else {
                "nothing waiting".to_string()
            }
        );
        ui.ctx().accesskit_node_builder(status_id, |node| {
            node.set_role(egui::accesskit::Role::Group);
            node.set_label(announced);
        });
        ui.advance_cursor_after_rect(rect);
        actions
    }

    /// One toolbar button, with its words attached.
    fn toolbar_button(
        &self,
        ui: &mut Ui,
        keymap: &Keymap,
        state: &mut ViewState,
        button: ToolbarButton,
    ) -> Vec<ViewAction> {
        let mut actions = Vec::new();
        let has_session = self.selected.is_some() && self.layout.is_some();
        match button.intent {
            ToolbarIntent::Run(command) => {
                let enabled = match command {
                    Command::SplitHorizontal => has_session,
                    _ => true,
                };
                let shortcut = keymap
                    .chord_for(command)
                    .map(|chord| chord.describe(keymap.platform()));
                if icons::icon_button(ui, button.icon, button.label, shortcut.as_deref(), enabled)
                    .clicked()
                {
                    actions.push(ViewAction::Run(command));
                }
            }
            ToolbarIntent::NewWorkspace => {
                let shortcut = keymap
                    .chord_for(Command::NewWorkspace)
                    .map(|chord| chord.describe(keymap.platform()));
                if icons::icon_button(ui, button.icon, button.label, shortcut.as_deref(), true)
                    .clicked()
                {
                    state.workspace_draft = Some(WorkspaceDraft::new(false));
                }
            }
            ToolbarIntent::LayoutMenu => {
                let menu = egui::containers::menu::MenuButton::from_button(
                    egui::Button::new(RichText::new(button.icon).font(icons::font(15.0)))
                        .min_size(icons::SIZE),
                );
                let (response, _) = menu.ui(ui, |ui| {
                    for (label, preset) in [
                        ("Balance current splits", LayoutPreset::Balanced),
                        ("Equal columns", LayoutPreset::Columns),
                        ("Equal rows", LayoutPreset::Rows),
                        ("Main pane left", LayoutPreset::MainLeft),
                        ("Grid", LayoutPreset::Grid),
                    ] {
                        if ui
                            .add_enabled(has_session, egui::Button::new(label))
                            .clicked()
                        {
                            actions.push(ViewAction::ApplyLayoutPreset(preset));
                            ui.close();
                        }
                    }
                    ui.separator();
                    if ui
                        .add_enabled(
                            has_session,
                            egui::Button::new("Save current layout as preset"),
                        )
                        .clicked()
                    {
                        actions.push(ViewAction::Run(Command::SaveLayoutAsTemplate));
                        ui.close();
                    }
                    if ui.button("New layout preset…").clicked() {
                        actions.push(ViewAction::OpenLayoutEditor(LayoutEditorOrigin::Settings));
                        ui.close();
                    }
                });
                icons::describe(&response, button.label);
                response.on_hover_text("Layout — balance, columns, rows, grid or a saved preset");
            }
        }
        actions
    }

    /// The bar along the bottom of the window.
    ///
    /// This is where the `RESTORED SAFELY` strip went. It used to be a band pushed in
    /// between the top bar and the panes, which moved everything down whenever recovery
    /// was pending; a status bar is a place the window always has, so the same sentence
    /// can appear and disappear without the layout jumping.
    ///
    /// The recovery decision stays a decision. Confirming write access to a main checkout
    /// is an authority the user grants, so it remains a real, named, focusable button
    /// here — never a passive label, and never something the window does on its own.
    fn window_status_bar(
        &self,
        ui: &mut Ui,
        theme: &Theme,
        rect: Rect,
        session: Option<&SessionTreeView>,
    ) -> Vec<ViewAction> {
        let mut actions = Vec::new();
        let restore = self.restore;
        let available = restore
            .into_iter()
            .flat_map(|restore| &restore.panes)
            .filter(|pane| pane.can_relaunch)
            .count();
        let needs_write_confirmation =
            self.recovery_lease.is_some() || self.reclaiming_write_access;
        let recovering =
            restore.is_some() || needs_write_confirmation || self.unreachable_processes > 0;
        let status_alert = recovering || self.notice.is_some();

        ui.painter().rect_filled(
            rect,
            0.0,
            if status_alert {
                theme.raised
            } else {
                theme.panel
            },
        );
        ui.painter()
            .hline(rect.x_range(), rect.min.y, Stroke::new(1.0, theme.border));

        // The right-hand end first: its controls are laid out as widgets, and the
        // sentence on the left has to be clipped to whatever is left over rather than
        // painted straight through them.
        let recovery_has_controls = self.recovery_lease.is_some() || self.reclaiming_write_access;
        let controls_width = if recovery_has_controls { 250.0 } else { 0.0 };
        let controls = Rect::from_min_max(
            egui::pos2(
                (rect.max.x - controls_width).max(rect.min.x),
                rect.min.y + 2.0,
            ),
            egui::pos2(rect.max.x - 8.0, rect.max.y - 2.0),
        );
        if recovery_has_controls && controls.width() > 60.0 {
            ui.scope_builder(region(controls, "recovery-actions"), |ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if let Some(lease) = self.recovery_lease {
                        let pending = self.reclaiming_workspaces.contains(&lease.workspace_id);
                        if ui
                            .add_enabled(
                                !pending,
                                egui::Button::new(if pending {
                                    "Confirming…"
                                } else if self.unreachable_processes > 0 {
                                    "Check & confirm access"
                                } else {
                                    "Confirm write access"
                                })
                                .small(),
                            )
                            .on_hover_text(
                                "Grant this Session write access to the main checkout again",
                            )
                            .clicked()
                        {
                            actions.push(ViewAction::ReclaimWorkspaceWriteLease {
                                workspace_id: lease.workspace_id.clone(),
                                session_id: lease.session_id.clone(),
                                checkout_id: lease.checkout_id.clone(),
                            });
                        }
                    } else if self.reclaiming_write_access {
                        ui.add_enabled(false, egui::Button::new("Confirming…").small());
                    }
                });
            });
        }

        let focused = self.panes.iter().find(|pane| pane.focused);
        let (sentence, colour) =
            if let Some(notice) = self.notice.as_deref() {
                (notice.to_string(), theme.failure)
            } else if self.unreachable_processes > 0 {
                (
                    format!(
                    "RESTORED SAFELY · {} surviving process{} cannot be controlled by this daemon",
                    self.unreachable_processes,
                    if self.unreachable_processes == 1 { "" } else { "es" }
                ),
                    theme.attention,
                )
            } else if needs_write_confirmation {
                (
                    // Not "before starting panes": a pane that only opens a terminal
                    // writes nothing to the shared checkout and starts without this, so
                    // saying otherwise would ask the user for something they do not need.
                    "RESTORED SAFELY · confirm main-checkout write access to start agents \
                     and commands"
                        .to_string(),
                    theme.attention,
                )
            } else if restore.is_some() {
                (
                    format!(
                        "RESTORED SAFELY · {available} pane{} stopped · no command was restarted",
                        if available == 1 { "" } else { "s" }
                    ),
                    theme.attention,
                )
            } else {
                // Nothing to recover. The bar still earns its height: which pane has the
                // keyboard is the thing a terminal user asks of a status bar.
                match focused {
                    Some(pane) => (format!("FOCUS · {}", pane.title), theme.running),
                    None => ("FOCUS · no pane focused".to_string(), theme.text_faint),
                }
            };
        let sentence_limit = if recovery_has_controls {
            controls.min.x - 10.0
        } else if status_alert {
            rect.max.x - 10.0
        } else {
            rect.max.x - 220.0
        };
        ui.painter()
            .with_clip_rect(Rect::from_min_max(
                rect.min,
                egui::pos2(sentence_limit.max(rect.min.x), rect.max.y),
            ))
            .text(
                rect.left_center() + Vec2::new(10.0, 0.0),
                Align2::LEFT_CENTER,
                &sentence,
                FontId::new(11.0, egui::FontFamily::Proportional),
                colour,
            );

        // The session's own counts sit at the right when there is nothing to recover, so
        // the space the recovery controls need is never shared with them.
        let counts = session.filter(|_| !status_alert).map(|session| {
            let guard = read_only_guard_label(&session.session)
                .map(|guard| format!(" · {guard}"))
                .unwrap_or_default();
            format!(
                "{}{} · {} running · {} panes",
                session.session.mode.label(),
                guard,
                session.session.running_count,
                session.session.pane_count
            )
        });
        if let Some(counts) = &counts {
            ui.painter().text(
                rect.right_center() - Vec2::new(9.0, 0.0),
                Align2::RIGHT_CENTER,
                counts,
                FontId::new(10.0, egui::FontFamily::Monospace),
                theme.text_faint,
            );
        }

        let status_id = ui.id().with("window-bottom-status");
        let announced = match &counts {
            Some(counts) => format!("Status: {sentence} — {counts}"),
            None => format!("Status: {sentence}"),
        };
        ui.ctx().accesskit_node_builder(status_id, |node| {
            node.set_role(egui::accesskit::Role::Group);
            node.set_label(announced);
        });
        actions
    }

    /// What a click on a row does to the layout, as at most one request.
    ///
    /// Clicking a subagent — or a process an agent started — maximises the pane it is running
    /// inside; clicking the agent, the Session or the Workspace that owns it puts the layout
    /// back. Which pane, and which of the two, is [`spotlight`]'s decision; this turns it into a
    /// request, and its whole remaining job is *not sending one when nothing would change*.
    ///
    /// That matters because `zoom_pane` toggles. Asking to show a pane that is already maximised
    /// would un-maximise it, so clicking through four subagents that share a pane would flicker
    /// the layout in and out instead of leaving it maximised. The state the daemon last reported
    /// is what the comparison is against — the window does not keep its own idea of it.
    fn spotlight_for(&self, row: HierarchyRow<'_>) -> Vec<ViewAction> {
        let Some(layout) = self.layout.as_ref() else {
            return Vec::new();
        };
        let zoomed = layout.zoomed.clone();
        let restore = |zoomed: Option<PaneId>, session_id: SessionId| match zoomed {
            // Toggling the pane that *is* maximised is how the layout comes back.
            Some(pane) => vec![ViewAction::ZoomPane {
                session_id,
                pane_id: pane,
            }],
            None => Vec::new(),
        };
        match row {
            // A Session or a Workspace owns everything under it, so picking one is asking to see
            // the whole layout again.
            HierarchyRow::Session { session, .. } => restore(zoomed, session.session.id.clone()),
            HierarchyRow::Workspace(workspace) => workspace
                .sessions
                .iter()
                .find(|session| Some(&session.session.id) == self.selected.as_ref())
                .map(|session| restore(zoomed, session.session.id.clone()))
                .unwrap_or_default(),
            HierarchyRow::Process { session, node } => {
                match crate::spotlight::for_node(session, node) {
                    crate::spotlight::Spotlight::Show(pane) if zoomed.as_ref() != Some(&pane) => {
                        vec![ViewAction::ZoomPane {
                            session_id: session.session.id.clone(),
                            pane_id: pane,
                        }]
                    }
                    crate::spotlight::Spotlight::Show(_) => Vec::new(),
                    crate::spotlight::Spotlight::Restore => {
                        restore(zoomed, session.session.id.clone())
                    }
                    crate::spotlight::Spotlight::Leave => Vec::new(),
                }
            }
        }
    }

    fn lifecycle_confirmation_overlay(
        &self,
        ui: &mut Ui,
        theme: &Theme,
        state: &mut ViewState,
        full: Rect,
    ) -> Vec<ViewAction> {
        let mut actions = Vec::new();
        let Some(confirmation) = state.lifecycle_confirmation.clone() else {
            return actions;
        };
        // `full` is no longer used to place anything: the modal owns its own backdrop and
        // centres its own card, and it measures the card from what is put in it rather than
        // from a height guessed per variant — which is what the hand-painted panel had to do,
        // and got wrong every time a sentence was added.
        let _ = full;

        let (title, subject, detail, scope, terminating) = match &confirmation {
            LifecycleConfirmation::EndSession {
                name,
                running_count,
                ..
            } => (
                "End session?",
                name.as_str(),
                "Turn will politely stop every process in this Session, and its row leaves the \
                 tree. Its layout and history are kept — restoring it brings the Session back, \
                 stopped.",
                None,
                format!(
                    "{} running process{} will receive a termination request.",
                    running_count,
                    if *running_count == 1 { "" } else { "es" }
                ),
            ),
            LifecycleConfirmation::StopWorkspace {
                name,
                session_count,
                running_sessions,
                running_processes,
                ..
            } => (
                "Stop all sessions in this workspace?",
                name.as_str(),
                "Turn will politely stop processes in every Session, and the Workspace leaves \
                 the tree. The project directory and all files stay untouched, and restoring it \
                 brings the Workspace and its Sessions back, stopped.",
                // The blast radius, in numbers, because "this workspace" is not a
                // quantity and the user is about to stop everything in it.
                Some(format!(
                    "{} session{} in this Workspace · {} with something running.",
                    session_count,
                    if *session_count == 1 { "" } else { "s" },
                    running_sessions
                )),
                format!(
                    "{} running process{} will receive a termination request, including in archived Sessions.",
                    running_processes,
                    if *running_processes == 1 { "" } else { "es" }
                ),
            ),
            LifecycleConfirmation::DeleteSession {
                name,
                running_count,
                ..
            } => (
                "Delete this session?",
                name.as_str(),
                // What is deleted, then what is not, in that order. A person reading
                // "delete" is asking about their work, and the answer is in the second
                // half of the sentence.
                "Turn will forget this Session: its layout, its history and its record. \
                 Your files, branches and worktrees are not touched.",
                None,
                format!(
                    "This cannot be undone. {} running process{} will be stopped first.",
                    running_count,
                    if *running_count == 1 { "" } else { "es" }
                ),
            ),
            LifecycleConfirmation::DeleteWorkspace {
                name,
                session_count,
                running_processes,
                root,
                ..
            } => (
                "Delete this workspace?",
                name.as_str(),
                "Turn will forget this Workspace and every Session in it.",
                // The path, verbatim: the one thing worth reading twice is what stays.
                Some(format!("{root} stays exactly as it is.")),
                format!(
                    "This cannot be undone. {} session{} and {} running process{} will \
                     be stopped and deleted.",
                    session_count,
                    if *session_count == 1 { "" } else { "s" },
                    running_processes,
                    if *running_processes == 1 { "" } else { "es" }
                ),
            ),
        };
        // What the act will *not* achieve, said before the click rather than after it.
        //
        // Turn used to refuse the whole operation here — a Session with a process from a
        // previous daemon in it could not be ended at all, and the user was told to go and
        // stop it themselves first. That protected nothing: the survivor kept running either
        // way, and the only thing the refusal preserved was a row the user had finished with.
        // So the act goes ahead, and what is owed instead is this sentence. It is deliberately
        // not phrased as a warning to be dismissed: it says what stays running and leaves the
        // decision alone.
        let escaped_count = match &confirmation {
            LifecycleConfirmation::EndSession { escaped_count, .. }
            | LifecycleConfirmation::StopWorkspace { escaped_count, .. }
            | LifecycleConfirmation::DeleteSession { escaped_count, .. }
            | LifecycleConfirmation::DeleteWorkspace { escaped_count, .. } => *escaped_count,
        };
        let escaped = (escaped_count > 0).then(|| {
            format!(
                "{} of them survived a previous daemon and cannot be stopped by Turn. \
                 This will go ahead without {}; stop {} yourself if you need to.",
                escaped_count,
                if escaped_count == 1 { "it" } else { "them" },
                if escaped_count == 1 { "it" } else { "them" }
            )
        });
        // The other door, named on the way through this one. Somebody who only wants the row
        // gone must be able to see that stopping the work is not the price of a tidy tree.
        let alternative = match &confirmation {
            // Not "archive instead": ending *is* archiving now, with the work stopped first.
            // The alternative worth naming is the one that keeps the work running.
            LifecycleConfirmation::EndSession { .. } => {
                "Only clearing your screen? Detach its views instead — the panes close, the row stays and the work carries on."
            }
            LifecycleConfirmation::StopWorkspace { .. } => {
                "Only clearing your screen? Archive it instead — the Workspace leaves the tree and nothing stops."
            }
            LifecycleConfirmation::DeleteSession { .. } => {
                "Want it back later? Archive it instead — the row leaves the tree and the Session keeps everything."
            }
            LifecycleConfirmation::DeleteWorkspace { .. } => {
                "Want it back later? Archive it instead — the Workspace leaves the tree and keeps its Sessions."
            }
        };
        // Named for what it does, and the same word the control that opened it used.
        let confirm_label = match &confirmation {
            LifecycleConfirmation::EndSession { .. } => "End session",
            LifecycleConfirmation::StopWorkspace { .. } => "Stop all sessions",
            LifecycleConfirmation::DeleteSession { .. } => "Delete session",
            LifecycleConfirmation::DeleteWorkspace { .. } => "Delete workspace",
        };

        // Turn paints this itself. `egui-elegance` was tried for it and rejected: its symbols
        // font is inserted into the same private-use range as Turn's icon font at the same
        // priority, and it won — every archive drawer in the tree became a plus sign. What it
        // was worth having, though, was three properties this dialog did not have, and they are
        // implemented below rather than lost with it: the accessibility role of an *alert*, a
        // way out with the keyboard, and giving focus back to whatever had it.
        // Taller for the two that say one thing more — how many Sessions the act reaches, or
        // which directory it leaves alone. A panel sized for the longest of the four would leave
        // the shortest with a band of empty space under its buttons.
        let wanted_height = match &confirmation {
            LifecycleConfirmation::StopWorkspace { .. }
            | LifecycleConfirmation::DeleteWorkspace { .. } => 312.0_f32,
            _ => 284.0_f32,
        }
        // And taller again for the one that only appears when a process has escaped. Two
        // lines: it names a count and then what the user can do about it, and a card sized
        // without it clipped the buttons underneath.
        + if escaped.is_some() { 40.0 } else { 0.0 };
        let bounds = Rect::from_center_size(
            full.center(),
            Vec2::new(
                520.0_f32.min((full.width() - 32.0).max(300.0)),
                wanted_height.min((full.height() - 32.0).max(220.0)),
            ),
        );
        ui.painter()
            .rect_filled(full, 0.0, Color32::from_black_alpha(165));

        // Escape declines. A modal question with no keyboard way out is a trap for anyone not
        // using a pointer, and declining is always the safe answer — every one of these four
        // stops or destroys something.
        //
        // Dismissing on a click outside the panel was written and then removed. The click that
        // *opens* the dialog is in the same frame's input as the first frame it is drawn on, so
        // the question appeared and closed again in one frame — caught by the test that presses
        // the row's own control. Distinguishing the two would mean tracking which frame the
        // dialog appeared on, and the convention buys nothing here: Escape covers the keyboard,
        // Cancel covers the pointer, and neither can be triggered by the act of asking.
        let mut confirmed = false;
        let mut cancelled = ui.input(|input| input.key_pressed(egui::Key::Escape));
        ui.painter().rect_filled(bounds, 10.0, theme.panel);
        ui.painter().rect_stroke(
            bounds,
            10.0,
            Stroke::new(1.0, theme.border),
            egui::StrokeKind::Outside,
        );
        ui.scope_builder(
            region(bounds.shrink(20.0), "lifecycle-confirmation"),
            |ui| {
                ui.ctx().accesskit_node_builder(ui.id(), |node| {
                    // An *alert* dialog, not a plain one. Every one of these four questions is about
                    // stopping or destroying something, and a screen reader announces an alert
                    // before its content rather than waiting to be asked.
                    node.set_role(egui::accesskit::Role::AlertDialog);
                    node.set_modal();
                    node.set_label(title);
                });
                ui.label(RichText::new(title).size(21.0).color(theme.text).strong());
                ui.label(RichText::new(subject).color(theme.text_dim).strong());
                ui.add_space(10.0);
                ui.label(RichText::new(detail).color(theme.text_dim));
                if let Some(scope) = scope {
                    ui.label(RichText::new(scope).color(theme.text_dim));
                }
                // At body size, not small. This is the line that says how much of the world the
                // button reaches, and for a delete it is also the line that says it does not come
                // back — it must not be the same weight as the hint underneath it, which is the
                // way *out*.
                ui.label(RichText::new(terminating).color(theme.attention));
                if let Some(escaped) = escaped.as_deref() {
                    ui.label(RichText::new(escaped).color(theme.failure));
                }
                ui.label(RichText::new(alternative).color(theme.text_faint).small());
                ui.add_space(14.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add(
                            egui::Button::new(RichText::new(confirm_label).color(Color32::WHITE))
                                .fill(theme.failure),
                        )
                        .clicked()
                    {
                        confirmed = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancelled = true;
                    }
                });
            },
        );

        if confirmed {
            match confirmation {
                LifecycleConfirmation::EndSession { session_id, .. } => {
                    actions.push(ViewAction::CloseSession {
                        session_id,
                        disposition: CloseDisposition::Terminate,
                    });
                }
                LifecycleConfirmation::StopWorkspace { workspace_id, .. } => {
                    actions.push(ViewAction::CloseWorkspace {
                        workspace_id,
                        disposition: CloseDisposition::Terminate,
                    });
                }
                LifecycleConfirmation::DeleteSession { session_id, .. } => {
                    actions.push(ViewAction::DeleteSession {
                        session_id,
                        disposition: CloseDisposition::Terminate,
                    });
                }
                LifecycleConfirmation::DeleteWorkspace { workspace_id, .. } => {
                    actions.push(ViewAction::DeleteWorkspace {
                        workspace_id,
                        disposition: CloseDisposition::Terminate,
                    });
                }
            }
        }
        if confirmed || cancelled {
            state.lifecycle_confirmation = None;
        }
        actions
    }

    /// The question asked before Turn hands a suspicious link to the desktop.
    ///
    /// Only reached for a link `links` flagged: an ordinary hyperlink opens on the click, and
    /// a dialog in front of every link would train the user to dismiss this one. What makes
    /// it worth asking is that the program in the pane chose both halves — the text the user
    /// read and the target they did not — so the two are quoted separately and neither is
    /// paraphrased.
    ///
    /// The default is the safe answer: Escape declines, and "Open link" is the one that has to
    /// be aimed at.
    fn link_confirmation_overlay(
        &self,
        ui: &mut Ui,
        theme: &Theme,
        link: &terminal::links::LinkRequest,
        full: Rect,
    ) -> Vec<ViewAction> {
        let mut actions = Vec::new();
        let bounds = Rect::from_center_size(
            full.center(),
            Vec2::new(
                560.0_f32.min((full.width() - 32.0).max(300.0)),
                268.0_f32.min((full.height() - 32.0).max(220.0)),
            ),
        );
        ui.painter()
            .rect_filled(full, 0.0, Color32::from_black_alpha(165));
        ui.painter().rect_filled(bounds, 10.0, theme.panel);
        ui.painter().rect_stroke(
            bounds,
            10.0,
            Stroke::new(1.0, theme.border),
            egui::StrokeKind::Outside,
        );
        let mut confirmed = false;
        let mut declined = ui.input(|input| input.key_pressed(egui::Key::Escape));
        ui.scope_builder(region(bounds.shrink(20.0), "link-confirmation"), |ui| {
            ui.ctx().accesskit_node_builder(ui.id(), |node| {
                node.set_role(egui::accesskit::Role::AlertDialog);
                node.set_modal();
                node.set_label("Open this link?");
            });
            ui.label(
                RichText::new("Open this link?")
                    .size(21.0)
                    .color(theme.text)
                    .strong(),
            );
            ui.add_space(6.0);
            // Monospace, both of them. These are strings chosen by a program to be
            // mistaken for one another, and a proportional font is where a lookalike
            // character hides.
            ui.label(
                RichText::new(format!("shown as  {}", link.text))
                    .monospace()
                    .color(theme.text_dim),
            );
            ui.label(
                RichText::new(format!("goes to   {}", link.display))
                    .monospace()
                    .color(theme.text),
            );
            ui.add_space(10.0);
            if let Some(warning) = &link.warning {
                ui.label(RichText::new(warning.describe()).color(theme.failure));
            }
            ui.add_space(14.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add(
                        egui::Button::new(RichText::new("Open link").color(Color32::WHITE))
                            .fill(theme.failure),
                    )
                    .clicked()
                {
                    confirmed = true;
                }
                if ui.button("Cancel").clicked() {
                    declined = true;
                }
            });
        });
        if confirmed {
            actions.push(ViewAction::ConfirmLink);
        } else if declined {
            actions.push(ViewAction::DismissLink);
        }
        actions
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
        let height = 148.0;
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
                ui.label(
                    RichText::new(format!("Agent: {}   ·   Process: {}", p.agent, p.process))
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
        keymap: &Keymap,
        snapshot: &HierarchySnapshot,
        state: &mut ViewState,
    ) -> Vec<ViewAction> {
        let mut actions = Vec::new();
        let area = ui.available_rect_before_wrap();
        ui.painter().rect_filled(area, 0.0, theme.panel);

        let header = Rect::from_min_size(area.min, Vec2::new(area.width(), 116.0));
        ui.painter().rect_filled(header, 0.0, theme.panel);
        ui.painter().hline(
            header.x_range(),
            header.max.y,
            Stroke::new(1.0, theme.border),
        );
        let mut presentation_changed = false;
        ui.scope_builder(
            region(header.shrink2(Vec2::new(8.0, 4.0)), "hierarchy-header"),
            |ui| {
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new("WORKSPACES")
                                .monospace()
                                .size(11.0)
                                .color(theme.text_dim),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("+ Workspace").clicked() {
                                state.workspace_draft = Some(WorkspaceDraft::new(false));
                            }
                            if ui
                                .small_button("Collapse")
                                .on_hover_text("Collapse all")
                                .clicked()
                            {
                                set_hierarchy_expanded_all(state, snapshot, false);
                            }
                            if ui
                                .small_button("Expand")
                                .on_hover_text("Expand all")
                                .clicked()
                            {
                                set_hierarchy_expanded_all(state, snapshot, true);
                            }
                        });
                    });
                    let search = ui.add(
                        egui::TextEdit::singleline(&mut state.tree_query)
                            .desired_width(f32::INFINITY)
                            .hint_text("Search workspace, session, agent, process or preview"),
                    );
                    search.ctx.accesskit_node_builder(search.id, |node| {
                        node.set_role(egui::accesskit::Role::SearchInput);
                        node.set_label("Search workspace tree");
                    });
                    ui.horizontal(|ui| {
                        ui.menu_button(format!("Filters ({})", state.tree_filters.len()), |ui| {
                            for filter in TreeFilter::ALL {
                                let mut enabled = state.tree_filters.contains(filter);
                                if ui.checkbox(&mut enabled, filter.label()).changed() {
                                    if enabled {
                                        state.tree_filters.insert(*filter);
                                    } else {
                                        state.tree_filters.remove(filter);
                                    }
                                    presentation_changed = true;
                                }
                            }
                            if !state.tree_filters.is_empty()
                                && ui.button("Clear filters").clicked()
                            {
                                state.tree_filters.clear();
                                presentation_changed = true;
                            }
                        });
                        egui::ComboBox::from_id_salt("tree-visibility-mode")
                            .selected_text(state.tree_visibility.label())
                            .show_ui(ui, |ui| {
                                for mode in TreeVisibilityMode::ALL {
                                    if ui
                                        .selectable_value(
                                            &mut state.tree_visibility,
                                            *mode,
                                            mode.label(),
                                        )
                                        .changed()
                                    {
                                        presentation_changed = true;
                                    }
                                }
                            });
                        ui.label(
                            RichText::new(if keymap.platform().uses_command_key {
                                "Cmd+Opt+←/→ all"
                            } else {
                                "Ctrl+Alt+←/→ all"
                            })
                            .monospace()
                            .size(9.0)
                            .color(theme.text_faint),
                        );
                    });
                });
            },
        );
        if presentation_changed {
            push_tree_presentation(state, snapshot);
        }
        ui.advance_cursor_after_rect(header);

        let rows = visible_hierarchy_rows(snapshot, state, self.include_archived);
        let tree_id = ui.id().with("workspace-session-process-tree");
        ui.ctx().accesskit_node_builder(tree_id, |node| {
            node.set_role(egui::accesskit::Role::Tree);
            node.set_label(format!(
                "Workspaces, sessions and processes, {} rows",
                rows.len()
            ));
        });

        if rows.is_empty() {
            // An empty tree with archived Workspaces behind it is not the same thing as an
            // empty tree, and a blank sidebar would leave the user to guess which they are
            // looking at.
            let everything_is_archived = !snapshot.workspaces.is_empty();
            ui.vertical_centered(|ui| {
                ui.add_space(28.0);
                ui.label(
                    RichText::new(if everything_is_archived {
                        "Every Workspace is archived"
                    } else {
                        "No workspaces yet"
                    })
                    .color(theme.text_dim),
                );
                ui.label(
                    RichText::new(if everything_is_archived {
                        "Turn on Show archived Workspaces and Sessions in Settings to see them"
                    } else {
                        "Create a project root before starting a Session"
                    })
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
        let restoring_scroll = state.scroll_tree_to.is_some();
        let mut first_visible = None;
        egui::ScrollArea::vertical()
            .id_salt("hierarchy-rows")
            .auto_shrink([false, false])
            .show_viewport(ui, |ui, viewport| {
                ui.add_space(4.0);
                // One width for every row, measured before the first one is placed. Asking
                // for `available_width()` per row instead let each row inherit the previous
                // one's overhang — a control placed at the right-hand end expands the used
                // area, so the next row came out a few points wider and the one after that
                // wider again, which is how the lower rows' controls ended up drifting
                // under the divider and being clipped in half.
                let row_width = ui.available_width();
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
                        row_width,
                        state.tree_visibility,
                        RowState {
                            selected: is_selected,
                            expanded,
                            focused_pane,
                            active_session,
                            idle: match row {
                                HierarchyRow::Process { session, node } => {
                                    crate::spotlight::idleness(session, node, self.now_ms)
                                }
                                _ => None,
                            },
                        },
                    );
                    if first_visible.is_none()
                        && response.rect.max.y >= viewport.min.y
                        && response.rect.min.y <= viewport.max.y
                    {
                        first_visible = Some(key.clone());
                    }
                    if state.scroll_tree_to.as_ref() == Some(&key) {
                        response.scroll_to_me(Some(egui::Align::Center));
                        state.scroll_tree_to = None;
                    }
                    // The row's own controls: create, archive, close. Drawn after the row
                    // and therefore on top of it — egui gives the later widget the click,
                    // which is what keeps a control from also selecting the row underneath.
                    actions.extend(self.hierarchy_row_controls(
                        ui,
                        theme,
                        keymap,
                        *row,
                        response.rect,
                        state,
                    ));
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
                            actions.extend(select_hierarchy_row(state, snapshot, *row));
                            actions.extend(self.spotlight_for(*row));
                        }
                    }
                    if response.double_clicked() && !caret_clicked {
                        actions.extend(open_or_focus_hierarchy_row(state, snapshot, *row));
                    }
                    match row {
                        HierarchyRow::Workspace(workspace) => response.context_menu(|ui| {
                            hierarchy_reorder_menu(ui, state, snapshot, *row);
                            if workspace.workspace.archived {
                                if ui.button("Restore workspace").clicked() {
                                    actions.push(ViewAction::ArchiveWorkspace {
                                        workspace_id: workspace.workspace.id.clone(),
                                        archived: false,
                                    });
                                    ui.close();
                                }
                            } else {
                                if ui.button("New session…").clicked() {
                                    state.session_draft = Some(SessionDraft::new(
                                        workspace.workspace.id.clone(),
                                        self.preferred_template_id(&workspace.workspace.id),
                                    ));
                                    ui.close();
                                }
                                ui.separator();
                                if ui
                                    .button(
                                        RichText::new("Stop all sessions…").color(theme.failure),
                                    )
                                    .clicked()
                                {
                                    state.lifecycle_confirmation =
                                        Some(LifecycleConfirmation::stop_workspace(workspace));
                                    ui.close();
                                }
                                let running_count: usize = workspace
                                    .sessions
                                    .iter()
                                    .map(|session| session.session.running_count)
                                    .sum();
                                let can_archive =
                                    running_count == 0 && workspace.write_lease.is_none();
                                if ui
                                    .add_enabled(can_archive, egui::Button::new("Archive workspace"))
                                    .on_disabled_hover_text(
                                        "End its Sessions and release the write lease before archiving.",
                                    )
                                    .clicked()
                                {
                                    actions.push(ViewAction::ArchiveWorkspace {
                                        workspace_id: workspace.workspace.id.clone(),
                                        archived: true,
                                    });
                                    ui.close();
                                }
                            }
                            // Last, after a separator, and offered whether the Workspace is
                            // archived or not: an archived row is exactly what somebody
                            // tidying up wants to get rid of, and refusing there would leave
                            // no way to remove it at all.
                            ui.separator();
                            if ui
                                .button(RichText::new("Delete workspace…").color(theme.failure))
                                .clicked()
                            {
                                state.lifecycle_confirmation =
                                    Some(LifecycleConfirmation::delete_workspace(workspace));
                                ui.close();
                            }
                        }),
                        HierarchyRow::Session { workspace, session } => response.context_menu(|ui| {
                            hierarchy_reorder_menu(ui, state, snapshot, *row);
                            if session.session.status == SessionStatus::Archived {
                                if ui
                                    .add_enabled(
                                        !workspace.workspace.archived,
                                        egui::Button::new("Restore session"),
                                    )
                                    .on_disabled_hover_text(
                                        "Restore the Workspace before restoring this Session.",
                                    )
                                    .clicked()
                                {
                                    actions.push(ViewAction::ArchiveSession {
                                        session_id: session.session.id.clone(),
                                        archived: false,
                                    });
                                    ui.close();
                                }
                                ui.separator();
                                if ui
                                    .button(RichText::new("Delete session…").color(theme.failure))
                                    .clicked()
                                {
                                    state.lifecycle_confirmation = Some(
                                        LifecycleConfirmation::delete_session(&session.session),
                                    );
                                    ui.close();
                                }
                                return;
                            }
                            if self.selected.as_ref() != Some(&session.session.id)
                                && ui.button("Activate session").clicked()
                            {
                                actions.push(ViewAction::SelectSession(session.session.id.clone()));
                                ui.close();
                            }
                            if ui
                                .button("Detach all views · keep processes running")
                                .clicked()
                            {
                                actions.push(ViewAction::CloseSession {
                                    session_id: session.session.id.clone(),
                                    disposition: CloseDisposition::KeepProcesses,
                                });
                                ui.close();
                            }
                            ui.separator();
                            if ui
                                .button(RichText::new("End session…").color(theme.failure))
                                .clicked()
                            {
                                state.lifecycle_confirmation =
                                    Some(LifecycleConfirmation::end_session(&session.session));
                                ui.close();
                            }
                            let owns_lease = workspace
                                .write_lease
                                .as_ref()
                                .is_some_and(|lease| lease.session_id == session.session.id);
                            if let Some(lease) = workspace.write_lease.as_ref().filter(|lease| {
                                lease.session_id == session.session.id
                                    && session.session.running_count == 0
                            }) {
                                if ui.button("Release write lease").clicked() {
                                    actions.push(ViewAction::ReleaseWorkspaceLease {
                                        workspace_id: workspace.workspace.id.clone(),
                                        lease_id: lease.id.clone(),
                                        expected_generation: lease.generation,
                                    });
                                    ui.close();
                                }
                            }
                            if ui
                                .add_enabled(
                                    session.session.running_count == 0 && !owns_lease,
                                    egui::Button::new("Archive session"),
                                )
                                .on_disabled_hover_text(
                                    "End it and release its write lease before archiving.",
                                )
                                .clicked()
                            {
                                actions.push(ViewAction::ArchiveSession {
                                    session_id: session.session.id.clone(),
                                    archived: true,
                                });
                                ui.close();
                            }
                            ui.separator();
                            if ui
                                .button(RichText::new("Delete session…").color(theme.failure))
                                .clicked()
                            {
                                state.lifecycle_confirmation =
                                    Some(LifecycleConfirmation::delete_session(&session.session));
                                ui.close();
                            }
                        }),
                        HierarchyRow::Process { session, node } => response.context_menu(|ui| {
                            hierarchy_reorder_menu(ui, state, snapshot, *row);
                            let workspace = snapshot.workspaces.iter().find(|workspace| {
                                workspace.workspace.id == session.session.workspace_id
                            });
                            let launch_blocked = session.session.status == SessionStatus::Archived
                                || workspace.is_some_and(|workspace| {
                                    workspace.workspace.archived
                                        || self.reclaiming_workspaces.contains(
                                            &workspace.workspace.id,
                                        )
                                        || workspace.write_lease.as_ref().is_some_and(|lease| {
                                            lease.session_id == session.session.id
                                                && lease.state == LeaseState::RecoveryRequired
                                        })
                                })
                                || session
                                    .nodes
                                    .iter()
                                    .any(|candidate| candidate.lifecycle == Lifecycle::Orphaned);
                            let has_saved_pane =
                                node.pane_bindings.iter().any(|binding| !binding.temporary);
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
                            if node.is_agentic {
                                if ui.button("Rename Agent…").clicked() {
                                    state.node_edit = Some(NodeEditDraft::rename(node));
                                    ui.close();
                                }
                                if ui.button("Correct relationship…").clicked() {
                                    state.node_edit = Some(NodeEditDraft::relationship(node));
                                    ui.close();
                                }
                                let has_target = session.nodes.iter().any(|candidate| {
                                    candidate.is_agentic && candidate.node_id != node.node_id
                                });
                                if ui
                                    .add_enabled(
                                        has_target,
                                        egui::Button::new("Pass context to Agent…"),
                                    )
                                    .on_disabled_hover_text(
                                        "This Session has no other Agent to receive context.",
                                    )
                                    .clicked()
                                {
                                    state.context_handoff =
                                        Some(ContextHandoffDraft::new(session, node));
                                    ui.close();
                                }
                            }
                            if ui.button("Open temporary pane").clicked() {
                                state.push_hierarchy_action(HierarchyAction::OpenTemporaryPane {
                                    surface_id: snapshot.tree_state.surface_id.clone(),
                                    session_id: node.session_id.clone(),
                                    node_id: node.node_id.clone(),
                                });
                                ui.close();
                            }
                            if let Some(target_pane_id) = self
                                .layout
                                .as_ref()
                                .and_then(|layout| layout.active.clone())
                            {
                                if ui.button("Open as pane…").clicked() {
                                    state.pane_placement = Some(PanePlacementDraft {
                                        source: PanePlacementSource::Node {
                                            surface_id: snapshot.tree_state.surface_id.clone(),
                                            session_id: node.session_id.clone(),
                                            node_id: node.node_id.clone(),
                                        },
                                        target_pane_id,
                                        placement: self.preferred_pane_placement(),
                                        remember: true,
                                    });
                                    ui.close();
                                }
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
                                    .add_enabled(
                                        node.lifecycle != Lifecycle::Orphaned,
                                        egui::Button::new(
                                            RichText::new(label).color(theme.failure),
                                        ),
                                    )
                                    .on_disabled_hover_text(
                                        "This process survived the previous daemon and is not controllable. Stop it outside Turn, then confirm recovery.",
                                    )
                                    .clicked()
                                {
                                    actions.push(ViewAction::TerminateNode {
                                        session_id: node.session_id.clone(),
                                        node_id: node.node_id.clone(),
                                    });
                                    ui.close();
                                }
                            } else if has_saved_pane {
                                ui.separator();
                                if ui
                                    .add_enabled(
                                        !launch_blocked,
                                        egui::Button::new("Start again"),
                                    )
                                    .on_disabled_hover_text(
                                        "Restore the Workspace/Session and confirm recovery before starting this pane.",
                                    )
                                    .clicked()
                                {
                                    actions.push(ViewAction::RelaunchNode {
                                        session_id: node.session_id.clone(),
                                        node_id: node.node_id.clone(),
                                        resume: false,
                                    });
                                    ui.close();
                                }
                            }
                        }),
                    };
                    if response.secondary_clicked() {
                        state.tree_has_focus = true;
                        response.request_focus();
                        set_hierarchy_selection(state, snapshot, key);
                    }
                }
            });

        if !restoring_scroll && first_visible != state.tree_scroll_anchor {
            state.tree_scroll_anchor = first_visible;
            push_tree_presentation(state, snapshot);
        }

        if hierarchy_accepts_keyboard(state) {
            let updated_rows = visible_hierarchy_rows(snapshot, state, self.include_archived);
            actions.extend(handle_hierarchy_keyboard(
                ui,
                snapshot,
                state,
                &updated_rows,
            ));
        }
        actions
    }

    /// The controls that live on one row of the tree.
    ///
    /// Three acts, and the whole point of drawing them side by side is that a user can see
    /// they are not the same act:
    ///
    /// * **New session** creates.
    /// * **Archive** takes the row out of the tree. Nothing is stopped, nothing is lost,
    ///   and the same control brings it back. It is the answer to "get this out of my
    ///   way", and it is deliberately the reversible one.
    /// * **Close** stops processes. It cannot do so from here: it opens the confirmation,
    ///   which is the only place in Turn where anything is terminated, and which says how
    ///   much and offers archiving instead.
    ///
    /// Every control names its exact target — "Close session Fix climbing bugs" — so a
    /// screen reader announces which row it belongs to, and carries its chord so the tree
    /// teaches the keyboard rather than hiding it in a sheet.
    fn hierarchy_row_controls(
        &self,
        ui: &mut Ui,
        theme: &Theme,
        keymap: &Keymap,
        row: HierarchyRow<'_>,
        row_rect: Rect,
        state: &mut ViewState,
    ) -> Vec<ViewAction> {
        let mut actions = Vec::new();
        // The same answer the row used when it reserved its width, so a control is never
        // drawn over a name that was not asked to make room for it.
        if row_action_width(row, row_rect.width()) <= 0.0 {
            return actions;
        }
        let shortcut = |command: Command| {
            keymap
                .chord_for(command)
                .map(|chord| chord.describe(keymap.platform()))
        };
        match row {
            HierarchyRow::Workspace(workspace) => {
                let summary = &workspace.workspace;
                let key = summary.id.as_str();
                let running: usize = workspace
                    .sessions
                    .iter()
                    .map(|session| session.session.running_count)
                    .sum();

                let label = format!("New session in {}", summary.name);
                let slot = row_action_slot(row_rect, 2);
                ui.scope_builder(keyed_region(slot, "workspace-new-session", key), |ui| {
                    if icons::row_button(
                        ui,
                        slot,
                        icons::FILE_PLUS,
                        &label,
                        "its Workspace, layout preset and checkout are already chosen",
                        shortcut(Command::NewSession).as_deref(),
                        !summary.archived,
                    )
                    .on_disabled_hover_text(
                        "Restore this Workspace before creating a Session in it.",
                    )
                    .clicked()
                    {
                        state.session_draft = Some(SessionDraft::new(
                            summary.id.clone(),
                            self.preferred_template_id(&summary.id),
                        ));
                    }
                });

                let archived = summary.archived;
                let label = if archived {
                    format!("Restore workspace {}", summary.name)
                } else {
                    format!("Archive workspace {}", summary.name)
                };
                let can_archive = archived || (running == 0 && workspace.write_lease.is_none());
                let slot = row_action_slot(row_rect, 1);
                ui.scope_builder(keyed_region(slot, "workspace-archive", key), |ui| {
                    if icons::row_button(
                        ui,
                        slot,
                        if archived {
                            icons::UNARCHIVE
                        } else {
                            icons::ARCHIVE
                        },
                        &label,
                        if archived {
                            "brings it back into the tree"
                        } else {
                            "takes it out of the tree · stops nothing · reversible"
                        },
                        shortcut(Command::ArchiveWorkspace).as_deref(),
                        can_archive,
                    )
                    .on_disabled_hover_text(
                        "End its Sessions and release the write lease before archiving.",
                    )
                    .clicked()
                    {
                        actions.push(ViewAction::ArchiveWorkspace {
                            workspace_id: summary.id.clone(),
                            archived: !archived,
                        });
                    }
                });

                // "Stop all sessions", not "Close workspace". The control stops the *work*: it
                // ends every Session in the Workspace, and each ended Session's row leaves the
                // tree. The Workspace's own row stays, because a Workspace is a project rather
                // than a task. Calling it "close" promised the project would go, which it does
                // not — and the two controls that really do that are on this same row.
                let label = format!("Stop all sessions in {}", summary.name);
                let detail = format!(
                    "ends its {} session{} · the Workspace stays · asks first",
                    workspace.sessions.len(),
                    if workspace.sessions.len() == 1 {
                        ""
                    } else {
                        "s"
                    }
                );
                let slot = row_action_slot(row_rect, 0);
                ui.scope_builder(keyed_region(slot, "workspace-close", key), |ui| {
                    ui.visuals_mut().widgets.hovered.fg_stroke.color = theme.failure;
                    if icons::row_button(
                        ui,
                        slot,
                        icons::POWER,
                        &label,
                        &detail,
                        shortcut(Command::CloseWorkspace).as_deref(),
                        !archived,
                    )
                    .on_disabled_hover_text(
                        "An archived Workspace has nothing running. Restore it first.",
                    )
                    .clicked()
                    {
                        // The control opens the question; the dialog is the only thing
                        // that can answer it.
                        state.lifecycle_confirmation =
                            Some(LifecycleConfirmation::stop_workspace(workspace));
                    }
                });
            }
            HierarchyRow::Session { workspace, session } => {
                let summary = &session.session;
                let key = summary.id.as_str();
                let archived = summary.status == SessionStatus::Archived;
                let owns_lease = workspace
                    .write_lease
                    .as_ref()
                    .is_some_and(|lease| lease.session_id == summary.id);

                let label = if archived {
                    format!("Restore session {}", summary.name)
                } else {
                    format!("Archive session {}", summary.name)
                };
                let enabled = if archived {
                    !workspace.workspace.archived
                } else {
                    summary.running_count == 0 && !owns_lease
                };
                let slot = row_action_slot(row_rect, 1);
                ui.scope_builder(keyed_region(slot, "session-archive", key), |ui| {
                    if icons::row_button(
                        ui,
                        slot,
                        if archived {
                            icons::UNARCHIVE
                        } else {
                            icons::ARCHIVE
                        },
                        &label,
                        if archived {
                            "brings it back into the tree"
                        } else {
                            "takes it out of the tree · stops nothing · reversible"
                        },
                        shortcut(Command::ArchiveSession).as_deref(),
                        enabled,
                    )
                    .on_disabled_hover_text(if archived {
                        "Restore the Workspace before restoring this Session."
                    } else {
                        "End it and release its write lease before archiving."
                    })
                    .clicked()
                    {
                        actions.push(ViewAction::ArchiveSession {
                            session_id: summary.id.clone(),
                            archived: !archived,
                        });
                    }
                });

                let label = format!("Close session {}", summary.name);
                let detail = format!(
                    "stops its {} running process{} · asks first",
                    summary.running_count,
                    if summary.running_count == 1 { "" } else { "es" }
                );
                let slot = row_action_slot(row_rect, 0);
                ui.scope_builder(keyed_region(slot, "session-close", key), |ui| {
                    ui.visuals_mut().widgets.hovered.fg_stroke.color = theme.failure;
                    if icons::row_button(
                        ui,
                        slot,
                        icons::POWER,
                        &label,
                        &detail,
                        shortcut(Command::CloseSession).as_deref(),
                        !archived,
                    )
                    .on_disabled_hover_text(
                        "An archived Session has nothing running. Restore it first.",
                    )
                    .clicked()
                    {
                        state.lifecycle_confirmation =
                            Some(LifecycleConfirmation::end_session(summary));
                    }
                });
            }
            HierarchyRow::Process { session, node } => {
                // Only a worker an agent is managing, which is what reserved the room above.
                if !crate::spotlight::is_managed(session, node) || node.lifecycle.is_terminal() {
                    return actions;
                }
                let what = if node.is_agentic { "agent" } else { "process" };
                let label = format!("Stop {what} {}", process_title(node));
                let idle = crate::spotlight::idleness(session, node, self.now_ms)
                    .filter(|idle| idle.worth_saying);
                let detail = match &idle {
                    // The reason the control is being looked at, in the tooltip that names it.
                    Some(idle) => format!(
                        "it has said nothing for {} · asks nothing first",
                        crate::spotlight::describe_silence(idle.silent_ms)
                    ),
                    None => "signals it to stop · asks nothing first".to_string(),
                };
                let slot = row_action_slot(row_rect, 0);
                ui.scope_builder(
                    keyed_region(slot, "process-stop", node.node_id.as_str()),
                    |ui| {
                        ui.visuals_mut().widgets.hovered.fg_stroke.color = theme.failure;
                        if icons::row_button(
                            ui,
                            slot,
                            icons::POWER,
                            &label,
                            &detail,
                            None,
                            node.lifecycle != Lifecycle::Orphaned,
                        )
                        .on_disabled_hover_text(
                            "This process survived the previous daemon and is not controllable. \
                             Stop it outside Turn, then confirm recovery.",
                        )
                        .clicked()
                        {
                            actions.push(ViewAction::TerminateNode {
                                session_id: node.session_id.clone(),
                                node_id: node.node_id.clone(),
                            });
                        }
                    },
                );
            }
        }
        // Back to the bottom of the row. Placing a widget moves the parent's cursor to the
        // bottom of *that* widget, and these sit on the row's first line — so without this
        // the next row started a line and a half too high and was drawn straight over this
        // one's state text.
        ui.advance_cursor_after_rect(row_rect);
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
        state: &mut ViewState,
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
            egui::pos2((area.max.x - 310.0).max(area.min.x), area.max.y),
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
                "{}{} · {branch} · {glyph} {} · {}",
                summary.mode.label(),
                read_only_guard_label(summary)
                    .map(|guard| format!(" · {guard}"))
                    .unwrap_or_default(),
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
        let archived = workspace.workspace.archived || summary.status == SessionStatus::Archived;
        let launch_blocked = archived
            || self.recovery_lease.is_some()
            || self.reclaiming_write_access
            || self.unreachable_processes > 0;
        let toolbar = Rect::from_min_size(
            area.right_top() + Vec2::new(-298.0, 7.0),
            Vec2::new(288.0, 32.0),
        );
        ui.scope_builder(region(toolbar, "session-layout-toolbar"), |ui| {
            ui.horizontal(|ui| {
                ui.add_enabled_ui(!launch_blocked, |ui| {
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
                        ui.separator();
                        if ui.button("Custom command or view…").clicked() {
                            if let Some(target) = self
                                .layout
                                .as_ref()
                                .and_then(|layout| layout.active.clone())
                            {
                                state.new_pane = Some(NewPaneDraft::new(
                                    target,
                                    self.preferred_pane_placement(),
                                ));
                            }
                            ui.close();
                        }
                    });
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
                });
                ui.menu_button("Session", |ui| {
                    if archived {
                        if workspace.workspace.archived && ui.button("Restore workspace").clicked()
                        {
                            actions.push(ViewAction::ArchiveWorkspace {
                                workspace_id: workspace.workspace.id.clone(),
                                archived: false,
                            });
                            ui.close();
                        }
                        if summary.status == SessionStatus::Archived
                            && ui
                                .add_enabled(
                                    !workspace.workspace.archived,
                                    egui::Button::new("Restore session"),
                                )
                                .on_disabled_hover_text(
                                    "Restore the Workspace before restoring this Session.",
                                )
                                .clicked()
                        {
                            actions.push(ViewAction::ArchiveSession {
                                session_id: summary.id.clone(),
                                archived: false,
                            });
                            ui.close();
                        }
                    } else {
                        if summary.mode == SessionMode::ReadOnly {
                            let pending = self
                                .reclaiming_workspaces
                                .contains(&workspace.workspace.id);
                            // An unenforced read-only Session launches no processes, so
                            // explicit promotion is its safe recovery path rather than an
                            // action to hide. The daemon still performs the atomic mode and
                            // lease transition and rejects any live runtime.
                            let available = summary.running_count == 0
                                && workspace.write_lease.is_none()
                                && !pending;
                            let disabled_reason = if summary.running_count > 0 {
                                "End every read-only process before changing this Session to write mode."
                            } else if workspace.write_lease.is_some() {
                                "Another Session currently owns this checkout's write lease."
                            } else {
                                "The write-access request is already in progress."
                            };
                            if ui
                                .add_enabled(
                                    available,
                                    egui::Button::new(if pending {
                                        "Acquiring write access…"
                                    } else {
                                        "Acquire exclusive write access"
                                    }),
                                )
                                .on_disabled_hover_text(disabled_reason)
                                .on_hover_text(
                                    "Explicitly changes this read-only Session to main-checkout write mode and records the exclusive lease.",
                                )
                                .clicked()
                            {
                                actions.push(ViewAction::ReclaimWorkspaceWriteLease {
                                    workspace_id: workspace.workspace.id.clone(),
                                    session_id: summary.id.clone(),
                                    checkout_id: summary.checkout_id.clone(),
                                });
                                ui.close();
                            }
                            ui.separator();
                        }
                        if ui.button("Detach all views · keep running").clicked() {
                            actions.push(ViewAction::CloseSession {
                                session_id: summary.id.clone(),
                                disposition: CloseDisposition::KeepProcesses,
                            });
                            ui.close();
                        }
                        if let Some(lease) = owned_lease.filter(|_| summary.running_count == 0) {
                            if ui.button("Release write lease").clicked() {
                                actions.push(ViewAction::ReleaseWorkspaceLease {
                                    workspace_id: workspace.workspace.id.clone(),
                                    lease_id: lease.id.clone(),
                                    expected_generation: lease.generation,
                                });
                                ui.close();
                            }
                        }
                        if ui
                            .button(RichText::new("End session…").color(theme.failure))
                            .clicked()
                        {
                            state.lifecycle_confirmation =
                                Some(LifecycleConfirmation::end_session(summary));
                            ui.close();
                        }
                        if ui
                            .add_enabled(
                                summary.running_count == 0 && owned_lease.is_none(),
                                egui::Button::new("Archive session"),
                            )
                            .on_disabled_hover_text(
                                "End the Session and release its write lease before archiving.",
                            )
                            .clicked()
                        {
                            actions.push(ViewAction::ArchiveSession {
                                session_id: summary.id.clone(),
                                archived: true,
                            });
                            ui.close();
                        }
                        ui.separator();
                        if ui
                            .button(RichText::new("Stop all sessions…").color(theme.failure))
                            .clicked()
                        {
                            state.lifecycle_confirmation =
                                Some(LifecycleConfirmation::stop_workspace(workspace));
                            ui.close();
                        }
                        let workspace_running: usize = workspace
                            .sessions
                            .iter()
                            .map(|session| session.session.running_count)
                            .sum();
                        if ui
                            .add_enabled(
                                workspace_running == 0 && workspace.write_lease.is_none(),
                                egui::Button::new("Archive workspace"),
                            )
                            .on_disabled_hover_text(
                                "Stop its Sessions and release the write lease first.",
                            )
                            .clicked()
                        {
                            actions.push(ViewAction::ArchiveWorkspace {
                                workspace_id: workspace.workspace.id.clone(),
                                archived: true,
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
                            "Add rows or columns, choose a program per cell, drag a cell onto another one's edge to move it there or its middle to swap, and drag dividers to resize.",
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
                // Only as much of the hint as the cell can hold. A layout of six cells
                // makes each one narrow, and a caption wider than its cell would spill
                // across the cell next door and describe the wrong one.
                paint_widest_that_fits(
                    ui.painter(),
                    placed.rect.shrink(6.0),
                    placed.rect.center() + Vec2::new(0.0, 11.0),
                    &[
                        "drag onto an edge or a middle · select to configure",
                        "drag onto an edge or a middle",
                        "drag to move",
                    ],
                    FontId::new(10.0, egui::FontFamily::Proportional),
                    theme.text_faint,
                );
                if response.clicked() {
                    draft.selected = placed.pane_id.clone();
                    draft.layout.active = Some(placed.pane_id.clone());
                }
                if response.dragged() && draft.dragged_pane.is_none() {
                    draft.dragged_pane = Some(placed.pane_id.clone());
                }
            }

            // Escape abandons the drag rather than closing the sheet: the key belongs to
            // the gesture in progress, and consuming it here is what keeps a cancelled
            // move from also discarding the template the user is drawing.
            if draft.dragged_pane.is_some()
                && ui.input_mut(|input| input.consume_key(Modifiers::NONE, Key::Escape))
            {
                draft.dragged_pane = None;
            }

            // The same five zones as a session pane, because this is the editor where a
            // layout is designed and a template that could only ever be reshaped by
            // deleting cells would be the original complaint in a second place.
            let landing = draft
                .dragged_pane
                .as_ref()
                .zip(ui.input(|input| input.pointer.interact_pos()))
                .and_then(|(moved, pointer)| arrangement.drop_target_at(moved, pointer));
            if let Some(moved) = draft
                .dragged_pane
                .as_ref()
                .and_then(|moved| arrangement.pane(moved))
            {
                let title = landing
                    .as_ref()
                    .and_then(|landing| arrangement.pane(&landing.pane_id))
                    .map(|target| draft.cell_label(&target.pane_id))
                    .unwrap_or_default();
                paint_drop_preview(
                    ui,
                    theme,
                    moved.rect,
                    landing.as_ref(),
                    &title,
                    ui.id().with("layout-editor-move-hint"),
                );
            }
            if ui.input(|input| input.pointer.any_released()) {
                // A draft is the window's own, so this one is applied locally: no daemon
                // owns a template that has not been created yet.
                if let (Some(moved), Some(landing)) = (draft.dragged_pane.take(), landing) {
                    if draft.layout.relocate(&moved, &landing.pane_id, landing.zone) {
                        draft.selected = moved;
                        debug_assert!(
                            draft.layout.sizes_are_normalised(),
                            "a relocation left the draft's sibling sizes not summing to one"
                        );
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
                                if ui.button("Priority −").clicked() {
                                    actions.push(ViewAction::SetAttentionPriority {
                                        attention_id: item.attention_id.clone(),
                                        priority_boost: item.priority_boost.saturating_sub(10).max(-100),
                                    });
                                }
                                ui.label(
                                    RichText::new(format!("{:+}", item.priority_boost))
                                        .monospace()
                                        .color(theme.text_dim),
                                );
                                if ui.button("Priority +").clicked() {
                                    actions.push(ViewAction::SetAttentionPriority {
                                        attention_id: item.attention_id.clone(),
                                        priority_boost: item.priority_boost.saturating_add(10).min(100),
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

    /// The panes of the selected session, with their dividers.
    /// Turns what one pane did this frame into what the window will do about it.
    ///
    /// The two halves arrive together and are handled differently on purpose.
    /// [`PaneAction`]s are things that happen *to* the pane and are forwarded verbatim.
    /// [`PaneRequest`]s are things the pane cannot do itself, and two of them are answered
    /// here rather than passed on, because they are window-local state in the same way
    /// `settings_open` is: the find bar belongs to the pane's own interaction record, and
    /// nothing outside this window needs to know it opened.
    fn pane_outcome(
        &self,
        state: &mut ViewState,
        pane_id: &PaneId,
        outcome: terminal::PaneOutcome,
        panes_in_layout: usize,
        scrollback_offset: usize,
    ) -> Vec<ViewAction> {
        let mut actions: Vec<ViewAction> = outcome
            .actions
            .into_iter()
            .map(|action| ViewAction::Pane {
                pane_id: pane_id.clone(),
                action,
            })
            .collect();
        for request in outcome.requests {
            match request {
                terminal::PaneRequest::Search(text) => {
                    // The offset the user is looking at, so closing the search puts them back
                    // where they were rather than at the bottom of a pane they had scrolled up
                    // in to read the thing they are searching for.
                    state
                        .pane(pane_id)
                        .search
                        .open_with(text, scrollback_offset, self.now_ms);
                }
                terminal::PaneRequest::ClearHistory => {
                    actions.push(ViewAction::ClearPaneHistory {
                        pane_id: pane_id.clone(),
                    });
                }
                terminal::PaneRequest::FollowLink(link) => {
                    actions.push(ViewAction::FollowLink(link));
                }
                terminal::PaneRequest::Split(Direction::Horizontal) => {
                    actions.push(ViewAction::Run(Command::SplitHorizontal));
                }
                terminal::PaneRequest::Split(Direction::Vertical) => {
                    actions.push(ViewAction::Run(Command::SplitVertical));
                }
                // Refused here rather than only greyed out in the menu. The menu is built
                // from the same count, so this is unreachable through it — but a chord is
                // not the menu, and the last pane of a Session is the one whose closure
                // would leave a Session with nowhere to type.
                terminal::PaneRequest::Close if panes_in_layout <= 1 => {
                    actions.push(ViewAction::Notice(
                        "this is the Session's only pane — end the Session to close it".into(),
                    ));
                }
                terminal::PaneRequest::Close => {
                    actions.push(ViewAction::ClosePane {
                        pane_id: pane_id.clone(),
                    });
                }
                terminal::PaneRequest::Notice(message) => {
                    actions.push(ViewAction::Notice(message));
                }
            }
        }
        actions
    }

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

        let selected_archived = self.selected.as_ref().is_some_and(|selected| {
            hierarchy.is_some_and(|snapshot| {
                snapshot.workspaces.iter().any(|workspace| {
                    (workspace.workspace.archived
                        || workspace.sessions.iter().any(|session| {
                            &session.session.id == selected
                                && session.session.status == SessionStatus::Archived
                        }))
                        && workspace
                            .sessions
                            .iter()
                            .any(|session| &session.session.id == selected)
                })
            })
        });

        let arrangement = panes::arrange(layout, area);
        // Persisted pane focus and the window's keyboard lease are different things.
        // Keep the visual focus in place behind a sheet, but never let that sheet's
        // Text/Paste/Key events reach a PTY.
        // A modal question holds the keyboard, and the link dialog is one: a keystroke aimed at
        // "Cancel" must not also be typed into whatever is running underneath it.
        let accepts_terminal_input = !state.is_sensitive()
            && self.write_conflict.is_none()
            && self.link_confirmation.is_none();

        // The pane menu's chords come from the same keymap the rest of the window uses, so a
        // user who rebound "close pane" sees their own chord in the menu rather than the
        // default it no longer is.
        let shortcuts = terminal::menu::PaneShortcuts::from_keymap(keymap);
        let panes_in_layout = arrangement.panes.len();
        // Why an item is greyed out, in the words the menu will show. Each reason is a
        // sentence rather than a bool: an item that is simply dim teaches the user nothing,
        // and "why can I not close this pane" is a question the menu can answer itself.
        let pane_context = terminal::menu::PaneContext {
            split_unavailable: (!accepts_terminal_input)
                .then(|| "Not while a sheet is open.".to_string()),
            close_unavailable: (panes_in_layout <= 1).then(|| {
                "This is the Session's only pane. End the Session to close it.".to_string()
            }),
            paste_unavailable: (!accepts_terminal_input)
                .then(|| "Not while a sheet is open.".to_string()),
            search_unavailable: None,
        };
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

            // The close control owns the right-hand end of the header. Everything else in
            // the header is measured against it rather than drawn on top of it.
            let mut title_limit = pane_menu_slot(header).min.x - 6.0;
            if arrangement.zoomed {
                title_limit = ui
                    .painter()
                    .text(
                        egui::pos2(title_limit, header.min.y + 4.0),
                        Align2::RIGHT_TOP,
                        "zoomed",
                        FontId::new(11.0, egui::FontFamily::Monospace),
                        theme.attention,
                    )
                    .min
                    .x
                    - 6.0;
            }
            ui.painter()
                .with_clip_rect(Rect::from_min_max(
                    header.min,
                    egui::pos2(title_limit.max(header.min.x), header.max.y),
                ))
                .text(
                    header.min + Vec2::new(8.0, 4.0),
                    Align2::LEFT_TOP,
                    &title,
                    FontId::new(11.0, egui::FontFamily::Monospace),
                    if focused { theme.text } else { theme.text_dim },
                );
            ui.painter().hline(
                header.x_range(),
                header.max.y,
                Stroke::new(1.0, theme.border),
            );
            actions.extend(pane_header_controls(
                ui,
                theme,
                keymap,
                placed,
                &arrangement,
                &mut state.dragged_pane,
                &title,
            ));

            let restore = self.restore.and_then(|restore| {
                restore
                    .panes
                    .iter()
                    .find(|outcome| outcome.pane_id == placed.pane_id)
                    .map(|outcome| (restore, outcome))
            });
            match (restore, content) {
                (Some((_restore, outcome)), _) => {
                    ui.painter().rect_filled(body, 0.0, theme.background);
                    let content_rect = Rect::from_center_size(
                        body.center(),
                        Vec2::new(body.width().min(340.0), body.height().min(132.0)),
                    );
                    ui.scope_builder(region(content_rect, "restored-pane-action"), |ui| {
                        ui.vertical_centered(|ui| {
                            let orphaned = outcome.lifecycle.is_running();
                            let pending = self.relaunching.contains(&outcome.node_id);
                            // Only what would actually use the checkout waits for the
                            // confirmation. A pane that opens the user's own shell starts
                            // now — including the terminal they need in order to go and
                            // stop whatever the confirmation is about.
                            let lease_blocked = (self.recovery_lease.is_some()
                                || self.reclaiming_write_access)
                                && outcome.needs_checkout_write;
                            let unreachable_blocked = self.unreachable_processes > 0;
                            let heading = if orphaned {
                                "Process survived outside Turn"
                            } else if pending {
                                "Starting automatically…"
                            } else if lease_blocked {
                                "Waiting for write access"
                            } else if unreachable_blocked {
                                "Waiting for the surviving process"
                            } else if selected_archived {
                                "Session is archived"
                            } else if outcome.can_relaunch {
                                "Restarting automatically…"
                            } else {
                                "Process is stopped"
                            };
                            ui.label(RichText::new(heading).color(theme.text).strong());
                            ui.label(
                                RichText::new(
                                    outcome
                                        .command
                                        .as_deref()
                                        .unwrap_or("Opening your configured shell"),
                                )
                                .monospace()
                                .color(theme.text_dim),
                            );
                            let explanation = if orphaned {
                                "Its terminal belonged to the previous daemon and cannot be reattached."
                            } else if pending || outcome.can_relaunch && !lease_blocked && !unreachable_blocked {
                                "Turn is restoring this pane; no action is required."
                            } else if lease_blocked {
                                "Automatic recovery will continue after write access is confirmed."
                            } else if unreachable_blocked {
                                "Turn will continue automatically when it is safe to avoid a duplicate process."
                            } else if selected_archived {
                                "Archived work stays stopped."
                            } else {
                                "This consequential command is not configured for automatic restart."
                            };
                            ui.label(
                                RichText::new(explanation)
                                    .color(theme.text_faint)
                                    .small(),
                            );
                        });
                    });
                }
                (None, Some(content)) => {
                    let options = PaneOptions {
                        focused,
                        accepts_input: focused && accepts_terminal_input,
                        now_ms: self.now_ms,
                        scrolled: content.scrolled,
                        history_complete: content.history_complete,
                    };
                    let id = ui.id().with(("pane", placed.pane_id.as_str()));
                    // Through `show_pane` rather than `show`, which is what makes the pane's
                    // own menu, its links and its find bar reachable at all. They were
                    // written, tested and then left behind the entry point that discards
                    // every request a pane makes — so following an OSC-8 hyperlink worked in
                    // the module's tests and did nothing in the window.
                    let outcome = terminal::show_pane(
                        ui,
                        state.pane(&placed.pane_id),
                        terminal::PaneInput {
                            theme,
                            rect: body,
                            grid: content.grid,
                            options,
                            id,
                            chrome: Some(terminal::PaneChrome {
                                shortcuts: &shortcuts,
                                context: &pane_context,
                            }),
                        },
                    );
                    actions.extend(self.pane_outcome(
                        state,
                        &placed.pane_id,
                        outcome,
                        panes_in_layout,
                        content.grid.scrollback_offset,
                    ));
                }
                (None, None) => {
                    ui.painter().rect_filled(body, 0.0, theme.background);
                    let stopped = placed.node_id.as_ref().and_then(|node_id| {
                        hierarchy.and_then(|snapshot| {
                            snapshot
                                .workspaces
                                .iter()
                                .flat_map(|workspace| &workspace.sessions)
                                .flat_map(|session| &session.nodes)
                                .find(|node| {
                                    &node.node_id == node_id && node.lifecycle.is_terminal()
                                })
                        })
                    });
                    if let Some(node) = stopped {
                        let content_rect = Rect::from_center_size(
                            body.center(),
                            Vec2::new(body.width().min(320.0), body.height().min(108.0)),
                        );
                        ui.scope_builder(region(content_rect, "stopped-pane-action"), |ui| {
                            ui.vertical_centered(|ui| {
                                ui.label(
                                    RichText::new("Session process stopped")
                                        .color(theme.text)
                                        .strong(),
                                );
                                ui.label(
                                    RichText::new(process_title(node))
                                        .monospace()
                                        .color(theme.text_dim),
                                );
                                let pending = self.relaunching.contains(&node.node_id);
                                ui.label(
                                    RichText::new(if pending {
                                        "Starting automatically…"
                                    } else {
                                        "Turn could not restart this process automatically."
                                    })
                                    .color(if pending {
                                        theme.running
                                    } else {
                                        theme.text_faint
                                    })
                                    .small(),
                                );
                            });
                        });
                    } else {
                        // A pane the window has not attached to yet, or one with no
                        // process. Said plainly rather than left blank, because a blank
                        // pane looks like a rendering bug.
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
        }

        // A drag whose header stopped being drawn — its pane closed, or a new layout
        // arrived from the daemon mid-gesture — never reports that it stopped. Forgetting
        // it once the pointer is up is what stops a dead gesture holding on to Escape.
        let dragged_pane_is_gone = state
            .dragged_pane
            .as_ref()
            .is_some_and(|moved| arrangement.pane(moved).is_none());
        if dragged_pane_is_gone
            || (state.dragged_pane.is_some() && !ui.input(|input| input.pointer.any_down()))
        {
            state.dragged_pane = None;
        }

        for divider in &arrangement.dividers {
            actions.extend(draggable_divider(ui, theme, divider));
        }
        actions
    }

    fn floating_panes(
        &self,
        ui: &mut Ui,
        theme: &Theme,
        _keymap: &Keymap,
        state: &mut ViewState,
    ) -> Vec<ViewAction> {
        let Some(layout) = &self.layout else {
            return Vec::new();
        };
        let mut actions = Vec::new();
        let live_ids: HashSet<PaneId> = layout
            .floating
            .iter()
            .map(|floating| floating.pane_id.clone())
            .collect();
        state
            .floating_geometry
            .retain(|pane_id, _| live_ids.contains(pane_id));

        for floating in &layout.floating {
            let Some(pane) = layout.get(&floating.pane_id) else {
                continue;
            };
            let pane_id = pane.id.clone();
            let title = self
                .panes
                .iter()
                .find(|content| content.pane_id == pane_id)
                .map(|content| content.title.clone())
                .or_else(|| pane.title.clone())
                .unwrap_or_else(|| format!("{:?}", pane.kind).to_lowercase());
            let geometry = state
                .floating_geometry
                .get(&pane_id)
                .copied()
                .unwrap_or(floating.geometry);
            let content = self.panes.iter().find(|content| content.pane_id == pane_id);
            let mut window_actions = Vec::new();
            let shown = egui::Window::new(title.clone())
                .id(ui.id().with(("floating-pane", pane_id.as_str())))
                .default_pos(egui::pos2(geometry.x, geometry.y))
                .default_size(Vec2::new(geometry.width, geometry.height))
                .min_size(Vec2::new(160.0, 100.0))
                .collapsible(false)
                .resizable(true)
                .show(ui.ctx(), |ui| {
                    ui.horizontal(|ui| {
                        if ui.small_button("Dock").clicked() {
                            window_actions.push(ViewAction::DockPane {
                                pane_id: pane_id.clone(),
                            });
                        }
                        if ui.small_button("Duplicate").clicked() {
                            window_actions.push(ViewAction::DuplicatePane {
                                pane_id: pane_id.clone(),
                            });
                        }
                        ui.menu_button("View type", |ui| {
                            for (label, kind) in pane_kind_choices() {
                                if ui.selectable_label(pane.kind == kind, label).clicked() {
                                    window_actions.push(ViewAction::ChangePaneKind {
                                        pane_id: pane_id.clone(),
                                        kind,
                                    });
                                    ui.close();
                                }
                            }
                        });
                        if layout.pane_count() > 1 && ui.small_button("Close view").clicked() {
                            window_actions.push(ViewAction::ClosePane {
                                pane_id: pane_id.clone(),
                            });
                        }
                    });
                    ui.separator();
                    let body = ui.available_rect_before_wrap();
                    if let Some(content) = content {
                        let options = PaneOptions {
                            focused: layout.active.as_ref() == Some(&pane_id),
                            accepts_input: !state.is_sensitive()
                                && self.write_conflict.is_none()
                                && self.link_confirmation.is_none(),
                            now_ms: self.now_ms,
                            scrolled: content.scrolled,
                            history_complete: content.history_complete,
                        };
                        let pane_actions = terminal::show(
                            ui,
                            theme,
                            body,
                            content.grid,
                            state.pane(&pane_id),
                            options,
                            ui.id().with(("floating-terminal", pane_id.as_str())),
                        );
                        window_actions.extend(pane_actions.into_iter().map(|action| {
                            ViewAction::Pane {
                                pane_id: pane_id.clone(),
                                action,
                            }
                        }));
                    } else {
                        ui.centered_and_justified(|ui| {
                            ui.label(
                                RichText::new(format!("{:?} view", pane.kind))
                                    .color(theme.text_dim),
                            );
                        });
                    }
                });
            actions.extend(window_actions);

            if let Some(shown) = shown {
                let rect = shown.response.rect;
                let next = PaneGeometry {
                    x: rect.min.x,
                    y: rect.min.y,
                    width: rect.width(),
                    height: rect.height(),
                };
                let released = ui.input(|input| input.pointer.any_released());
                if released && next.is_valid() && next != geometry {
                    state.floating_geometry.insert(pane_id.clone(), next);
                    actions.push(ViewAction::SetFloatingPaneGeometry {
                        pane_id: pane_id.clone(),
                        geometry: next,
                    });
                }
            }
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
        if let (Some(surface_id), Some(target_pane_id)) = (
            temporary.pane.binding.surface_id.clone(),
            self.layout
                .as_ref()
                .and_then(|layout| layout.active.clone()),
        ) {
            let keep_rect = Rect::from_min_size(
                close_rect.left_top() - Vec2::new(122.0, 0.0),
                Vec2::new(116.0, 24.0),
            );
            if ui
                .put(keep_rect, egui::Button::new("Keep in layout…"))
                .clicked()
            {
                state.pane_placement = Some(PanePlacementDraft {
                    source: PanePlacementSource::Temporary {
                        surface_id,
                        session_id: temporary.pane.binding.session_id.clone(),
                        pane_id: temporary.pane.binding.pane_id.clone(),
                    },
                    target_pane_id,
                    placement: match self.preferred_pane_placement() {
                        PanePlacement::Temporary => PanePlacement::SplitRight,
                        placement => placement,
                    },
                    remember: true,
                });
            }
        }

        let body = Rect::from_min_max(header.left_bottom(), panel.max).shrink(10.0);
        match (&temporary.pane.capability, temporary.grid) {
            (NodePaneCapability::Terminal { .. }, Some(grid)) => {
                let options = PaneOptions {
                    focused: true,
                    accepts_input: !state.is_sensitive()
                        && self.write_conflict.is_none()
                        && self.link_confirmation.is_none(),
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
                    if ui.button("Open temporary pane").clicked() {
                        state.push_hierarchy_action(HierarchyAction::OpenTemporaryPane {
                            surface_id: snapshot.tree_state.surface_id.clone(),
                            session_id: node.session_id.clone(),
                            node_id: node.node_id.clone(),
                        });
                    }
                    if let Some(target_pane_id) = self
                        .layout
                        .as_ref()
                        .and_then(|layout| layout.active.clone())
                    {
                        if ui.button("Open as pane…").clicked() {
                            state.pane_placement = Some(PanePlacementDraft {
                                source: PanePlacementSource::Node {
                                    surface_id: snapshot.tree_state.surface_id.clone(),
                                    session_id: node.session_id.clone(),
                                    node_id: node.node_id.clone(),
                                },
                                target_pane_id,
                                placement: self.preferred_pane_placement(),
                                remember: true,
                            });
                            state.quick_preview = None;
                        }
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

    fn context_handoff_overlay(
        &self,
        ui: &mut Ui,
        theme: &Theme,
        snapshot: &HierarchySnapshot,
        state: &mut ViewState,
        full: Rect,
    ) -> Vec<ViewAction> {
        let mut actions = Vec::new();
        let Some(identity) = state
            .context_handoff
            .as_ref()
            .map(|draft| (draft.session_id.clone(), draft.source_node_id.clone()))
        else {
            return actions;
        };
        let Some(session) = snapshot
            .workspaces
            .iter()
            .flat_map(|workspace| &workspace.sessions)
            .find(|session| session.session.id == identity.0)
        else {
            state.context_handoff = None;
            return actions;
        };
        let Some(source) = session.nodes.iter().find(|node| node.node_id == identity.1) else {
            state.context_handoff = None;
            return actions;
        };
        let candidates: Vec<&TreeNodeView> = session
            .nodes
            .iter()
            .filter(|node| node.is_agentic && node.node_id != source.node_id)
            .collect();

        let width = 720.0_f32.min((full.width() - 36.0).max(0.0));
        let height = 560.0_f32.min((full.height() - 36.0).max(0.0));
        let panel = Rect::from_center_size(full.center(), Vec2::new(width, height));
        ui.painter()
            .rect_filled(full, 0.0, Color32::from_black_alpha(135));
        ui.painter().rect_filled(panel, 0.0, theme.panel);
        ui.painter().rect_stroke(
            panel,
            0.0,
            Stroke::new(1.0, theme.border),
            egui::StrokeKind::Outside,
        );

        let mut close = false;
        let draft = state
            .context_handoff
            .as_mut()
            .expect("the handoff identity came from this draft");
        if draft.target_node_id.is_none() {
            draft.target_node_id = candidates
                .iter()
                .find(|candidate| context_target_unavailable_reason(candidate).is_none())
                .map(|candidate| candidate.node_id.clone());
        }
        ui.scope_builder(region(panel.shrink(18.0), "context-handoff"), |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("PASS CONTEXT")
                        .color(theme.text_dim)
                        .small(),
                );
                ui.label(
                    RichText::new(process_title(source))
                        .color(theme.text)
                        .strong(),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add_enabled(
                            !draft.preparing && !draft.delivering,
                            egui::Button::new(if draft.delivered { "Done" } else { "Cancel" }),
                        )
                        .clicked()
                    {
                        close = true;
                    }
                });
            });
            ui.separator();

            if draft.delivered {
                let target = draft
                    .prepared
                    .as_ref()
                    .map(|handoff| handoff.target_label.as_str())
                    .unwrap_or("Agent");
                ui.add_space(16.0);
                ui.label(
                    RichText::new(format!("✓ Context sent to {target}"))
                        .color(theme.done)
                        .size(18.0),
                );
                ui.add_space(8.0);
                ui.label(
                    RichText::new(
                        "The reviewed payload was submitted once. No Pane, selection or layout changed.",
                    )
                    .color(theme.text_dim),
                );
                return;
            }

            if let Some(handoff) = draft.prepared.as_ref() {
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new(format!(
                            "{}  →  {}",
                            handoff.source_label, handoff.target_label
                        ))
                        .color(theme.text)
                        .strong(),
                    );
                    ui.label(
                        RichText::new(format!("{} stable facts", handoff.preview_count))
                            .monospace()
                            .color(theme.text_dim)
                            .small(),
                    );
                    ui.label(
                        RichText::new(handoff.mode.label())
                            .monospace()
                            .color(theme.text_dim)
                            .small(),
                    );
                    if handoff.redacted {
                        ui.label(
                            RichText::new("SECRETS REDACTED")
                                .monospace()
                                .color(theme.attention)
                                .small(),
                        );
                    }
                });
                ui.add_space(8.0);
                ui.label(
                    RichText::new("EXACT PAYLOAD — CONTEXT IS UNTRUSTED UNTIL VERIFIED")
                        .monospace()
                        .color(theme.text_dim)
                        .small(),
                );
                egui::Frame::new()
                    .fill(theme.background)
                    .stroke(Stroke::new(1.0, theme.border))
                    .inner_margin(10.0)
                    .show(ui, |ui| {
                        egui::ScrollArea::both()
                            .id_salt("context-handoff-body")
                            .max_height(330.0)
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                ui.label(
                                    RichText::new(handoff.body.as_str())
                                        .monospace()
                                        .color(theme.text),
                                );
                            });
                    });
                ui.add_space(8.0);
                let mut go_back = false;
                let mut send = false;
                let delivery_session_id = handoff.session_id.clone();
                let delivery_handoff_id = handoff.handoff_id.clone();
                ui.horizontal(|ui| {
                    go_back = ui
                        .add_enabled(!draft.delivering, egui::Button::new("Back"))
                        .clicked();
                    let send_label = if draft.delivering {
                        "Sending…".to_string()
                    } else {
                        format!("Send to {}", handoff.target_label)
                    };
                    send = ui
                        .add_enabled(
                            !draft.delivering && draft.error.is_none(),
                            egui::Button::new(send_label),
                        )
                        .clicked();
                });
                if go_back {
                    draft.invalidate_review();
                } else if send {
                    draft.delivering = true;
                    draft.error = None;
                    actions.push(ViewAction::DeliverContextHandoff {
                        session_id: delivery_session_id,
                        handoff_id: delivery_handoff_id,
                    });
                }
            } else {
                ui.label(
                    RichText::new("MODE")
                        .monospace()
                        .color(theme.text_dim)
                        .small(),
                );
                let previous_mode = draft.mode;
                egui::ComboBox::from_id_salt("context-handoff-mode")
                    .selected_text(draft.mode.label())
                    .width(ui.available_width())
                    .show_ui(ui, |ui| {
                        for mode in [
                            ContextHandoffMode::ContinueWith,
                            ContextHandoffMode::ReviewHandoff,
                            ContextHandoffMode::SecondOpinion,
                            ContextHandoffMode::PromoteToMain,
                        ] {
                            ui.selectable_value(&mut draft.mode, mode, mode.label());
                        }
                    });
                if draft.mode != previous_mode {
                    draft.invalidate_review();
                }
                ui.add_space(10.0);
                ui.label(RichText::new("FROM").monospace().color(theme.text_dim).small());
                ui.label(
                    RichText::new(process_title(source))
                        .color(theme.text)
                        .strong(),
                );
                ui.add_space(10.0);
                ui.label(RichText::new("TO").monospace().color(theme.text_dim).small());
                let selected_label = draft
                    .target_node_id
                    .as_ref()
                    .and_then(|id| candidates.iter().find(|node| &node.node_id == id))
                    .map(|node| process_title(node).to_string())
                    .unwrap_or_else(|| "Choose an Agent".to_string());
                let previous_target = draft.target_node_id.clone();
                egui::ComboBox::from_id_salt("context-handoff-target")
                    .selected_text(selected_label)
                    .width(ui.available_width())
                    .show_ui(ui, |ui| {
                        for candidate in &candidates {
                            let reason = context_target_unavailable_reason(candidate);
                            ui.add_enabled_ui(reason.is_none(), |ui| {
                                let label = match reason {
                                    Some(reason) => {
                                        format!("{} — {reason}", process_title(candidate))
                                    }
                                    None => process_title(candidate).to_string(),
                                };
                                ui.selectable_value(
                                    &mut draft.target_node_id,
                                    Some(candidate.node_id.clone()),
                                    label,
                                );
                            });
                        }
                    });
                if draft.target_node_id != previous_target {
                    draft.invalidate_review();
                }

                if let Some(reason) = draft
                    .target_node_id
                    .as_ref()
                    .and_then(|id| candidates.iter().find(|node| &node.node_id == id))
                    .and_then(|node| context_target_unavailable_reason(node))
                {
                    ui.label(RichText::new(reason).color(theme.attention).small());
                }
                let anchor_pane = source
                    .pane_bindings
                    .iter()
                    .find(|binding| !binding.temporary)
                    .or_else(|| {
                        session
                            .nodes
                            .iter()
                            .flat_map(|node| &node.pane_bindings)
                            .find(|binding| !binding.temporary)
                    })
                    .map(|binding| binding.pane_id.clone());
                if let Some(pane_id) = anchor_pane {
                    if ui.small_button("+ Create Agent in this Session").clicked() {
                        actions.push(ViewAction::CreateContextHandoffTarget {
                            session_id: draft.session_id.clone(),
                            pane_id,
                        });
                    }
                }
                ui.add_space(10.0);
                ui.label(
                    RichText::new("OPTIONAL INSTRUCTION")
                        .monospace()
                        .color(theme.text_dim)
                        .small(),
                );
                let instruction = ui.add(
                    egui::TextEdit::multiline(&mut draft.instruction)
                        .hint_text("What should the destination Agent do with this context?")
                        .desired_rows(4)
                        .desired_width(f32::INFINITY),
                );
                if instruction.changed() {
                    draft.invalidate_review();
                }
                ui.add_space(8.0);
                ui.label(
                    RichText::new(
                        "Turn includes only stable, redacted activity facts — never raw terminal scrollback. Nothing is sent during Review.",
                    )
                    .color(theme.text_faint)
                    .small(),
                );
                let target_ready = draft
                    .target_node_id
                    .as_ref()
                    .and_then(|id| candidates.iter().find(|node| &node.node_id == id))
                    .is_some_and(|node| context_target_unavailable_reason(node).is_none());
                ui.add_space(10.0);
                if ui
                    .add_enabled(
                        target_ready && !draft.preparing,
                        egui::Button::new(if draft.preparing {
                            "Preparing review…"
                        } else {
                            "Review exact context"
                        }),
                    )
                    .clicked()
                {
                    let target_node_id = draft
                        .target_node_id
                        .clone()
                        .expect("the enabled button has a destination");
                    draft.preparing = true;
                    draft.error = None;
                    let instruction = (!draft.instruction.trim().is_empty())
                        .then(|| draft.instruction.clone());
                    actions.push(ViewAction::PrepareContextHandoff {
                        session_id: draft.session_id.clone(),
                        source_node_id: draft.source_node_id.clone(),
                        target_node_id,
                        mode: draft.mode,
                        instruction,
                    });
                }
            }

            if let Some(error) = &draft.error {
                ui.add_space(8.0);
                ui.label(RichText::new(error).color(theme.failure));
            }
        });

        let id = ui.id().with("context-handoff-accessibility");
        ui.ctx().accesskit_node_builder(id, |node| {
            node.set_role(egui::accesskit::Role::Dialog);
            node.set_label(format!("Pass context from {}", process_title(source)));
            node.set_modal();
        });
        if close {
            state.context_handoff = None;
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
        state: &mut ViewState,
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
            // Two chords on one key is the failure this whole sheet has to make visible: the
            // second binding never fires and nothing else would say so. Reported before the
            // list, because it is about the set rather than about any one row.
            let conflicts = keymap.conflicts();
            for (chord, commands) in &conflicts {
                ui.label(
                    RichText::new(format!(
                        "{} is bound to {} commands: {}. Only the first will fire.",
                        chord.describe(keymap.platform()),
                        commands.len(),
                        commands
                            .iter()
                            .map(|command| command.title())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ))
                    .color(theme.failure)
                    .small(),
                );
            }
            let taken: std::collections::BTreeMap<String, Command> = keymap
                .bindings()
                .iter()
                .map(|bound| (bound.chord.describe(keymap.platform()), bound.command))
                .collect();
            ui.add_space(6.0);
            ui.label(
                RichText::new("Type a chord and press Enter. An empty field unbinds the command.")
                    .color(theme.text_faint)
                    .small(),
            );
            ui.add_space(6.0);
            egui::ScrollArea::vertical()
                .id_salt("shortcut-rows")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for bound in keymap.bindings() {
                        let written = bound.chord.describe(keymap.platform());
                        ui.horizontal(|ui| {
                            let id = bound.command.id();
                            let draft = state
                                .shortcut_drafts
                                .entry(id.to_string())
                                .or_insert_with(|| written.clone());
                            let response = ui.add(
                                egui::TextEdit::singleline(draft)
                                    .desired_width(160.0)
                                    .font(egui::TextStyle::Monospace),
                            );
                            describe_control(
                                &response,
                                egui::accesskit::Role::TextInput,
                                bound.command.title(),
                            );
                            let committed = response.lost_focus()
                                || (response.has_focus()
                                    && ui.input(|input| input.key_pressed(egui::Key::Enter)));
                            if committed {
                                let typed = draft.trim().to_string();
                                state.shortcut_drafts.remove(id);
                                if typed != written {
                                    actions.push(ViewAction::RebindCommand {
                                        command: id.to_string(),
                                        chord: typed,
                                    });
                                }
                            }
                            ui.label(
                                RichText::new(bound.command.title())
                                    .color(theme.text_dim)
                                    .small(),
                            );
                            if bound.customised && ui.small_button("Reset").clicked() {
                                actions.push(ViewAction::RebindCommand {
                                    command: id.to_string(),
                                    // The sentinel that means "no opinion" rather than
                                    // "unbound": an empty chord unbinds, and there has to be a
                                    // way back to the default that is neither.
                                    chord: DEFAULT_CHORD.to_string(),
                                });
                            }
                            // A chord already in use, named. Said on the row that would lose,
                            // because that is the row the user is looking at.
                            if let Some(other) =
                                taken.get(&written).filter(|other| **other != bound.command)
                            {
                                ui.label(
                                    RichText::new(format!("also {}", other.title()))
                                        .color(theme.failure)
                                        .small(),
                                );
                            }
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

    /// The preferences, section by section, with where each value came from.
    ///
    /// Three things make this honest rather than a list of controls:
    ///
    /// **The level is chosen once, at the top, and shown on every control.** A change is
    /// four different acts depending on the level, and a sheet that let the user set a font
    /// size without saying where would be a sheet that edits their whole account when they
    /// meant this Session. The selector offers only levels that exist.
    ///
    /// **Every value says where it came from.** "Turn's default", or the level that set it.
    /// Without that the user cannot tell a value they chose from one that came with the
    /// product, and "reset" has nothing to mean.
    ///
    /// **Reset appears only when there is something to reset at the chosen level**, and its
    /// hover says what would come back. A greyed-out reset on an inherited value teaches
    /// nothing; one that silently did nothing would be worse.
    fn settings_preferences(
        &self,
        ui: &mut Ui,
        theme: &Theme,
        state: &mut ViewState,
    ) -> Vec<ViewAction> {
        let mut actions = Vec::new();
        let Some(settings) = self.settings else {
            ui.label(
                RichText::new("loading preferences…")
                    .color(theme.text_faint)
                    .small(),
            );
            return actions;
        };

        // The narrowest level that exists, unless the user has already chosen one. Narrowest
        // because a change made there surprises the fewest other things.
        let chosen = state
            .settings_level
            .filter(|scope| settings.level(*scope).is_some())
            .or_else(|| settings.levels.last().map(|level| level.scope));
        let Some(chosen) = chosen else {
            return actions;
        };
        state.settings_level = Some(chosen);

        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Changes apply to").color(theme.text_dim));
            for level in &settings.levels {
                let selected = level.scope == chosen;
                if ui
                    .selectable_label(
                        selected,
                        RichText::new(match level.scope {
                            turn_core::settings::Scope::Global
                            | turn_core::settings::Scope::Temporary => level.label.clone(),
                            _ => format!("{} · {}", level.scope.label(), level.label),
                        })
                        .color(if selected {
                            theme.text
                        } else {
                            theme.text_dim
                        }),
                    )
                    .on_hover_text(match level.scope {
                        turn_core::settings::Scope::Global => {
                            "Everywhere in Turn, for every Workspace and Session."
                        }
                        turn_core::settings::Scope::Workspace => {
                            "This project, and every Session in it that does not say otherwise."
                        }
                        turn_core::settings::Scope::Template => {
                            "Every Session made from this Template."
                        }
                        turn_core::settings::Scope::Session => "This Session alone.",
                        turn_core::settings::Scope::Temporary => {
                            "This window, until it closes. Not saved."
                        }
                    })
                    .clicked()
                {
                    state.settings_level = Some(level.scope);
                }
            }
        });
        let owner = settings
            .level(chosen)
            .map(|level| level.owner_id.clone())
            .unwrap_or_default();
        ui.add_space(10.0);

        egui::ScrollArea::vertical()
            .id_salt("settings-preferences")
            .max_height(300.0)
            .show(ui, |ui| {
                for area in turn_core::settings::Area::ALL {
                    let entries: Vec<&turn_proto::SettingsEntry> = settings.in_area(area).collect();
                    // An area with nothing in it is not drawn. The empty section is honest in
                    // the catalogue, where it says the area exists; on screen it would be a
                    // heading over nothing.
                    if entries.is_empty() {
                        continue;
                    }
                    ui.add_space(4.0);
                    ui.label(RichText::new(area.title()).color(theme.text).strong());
                    ui.add_space(4.0);
                    for entry in entries {
                        actions.extend(self.settings_row(ui, theme, state, entry, chosen, &owner));
                    }
                    ui.add_space(6.0);
                }
            });
        actions
    }

    /// One preference: its control, where its value came from, and its way back.
    fn settings_row(
        &self,
        ui: &mut Ui,
        theme: &Theme,
        state: &mut ViewState,
        entry: &turn_proto::SettingsEntry,
        chosen: turn_core::settings::Scope,
        owner: &str,
    ) -> Vec<ViewAction> {
        let mut actions = Vec::new();
        let key = entry.resolution.key.clone();
        // Whether this key can be written at the level the user picked. A control drawn where
        // the write would be refused is a control that lies, so it is disabled and says why.
        let settable = entry.settable_at.contains(&chosen);
        // A narrower winner can hide this level's own value. It is still present and must
        // remain independently editable/resettable rather than disappearing from the sheet.
        let set_here = entry.override_at(chosen).is_some();
        let editing_value = entry.value_for_editing_at(chosen).clone();

        egui::Frame::new()
            .fill(theme.raised)
            .stroke(Stroke::new(1.0, theme.border))
            .corner_radius(6.0)
            .inner_margin(egui::Margin::symmetric(12, 8))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width(ui.available_width() * 0.55);
                        ui.label(RichText::new(&entry.title).color(theme.text).strong());
                        ui.label(
                            RichText::new(&entry.description)
                                .color(theme.text_dim)
                                .small(),
                        );
                        // Where it came from, always. Sentence rather than a badge: "from the
                        // Workspace" is what the user needs to know before changing it.
                        let (origin, colour) = match entry.resolution.origin {
                            None => ("Effective · Turn's default".to_string(), theme.text_faint),
                            Some(scope) if scope == chosen => {
                                (format!("Effective · set here · {}", scope.label()), theme.running)
                            }
                            Some(scope) => (
                                format!("Effective · from {}", scope.label()),
                                theme.provisional,
                            ),
                        };
                        ui.label(RichText::new(origin).color(colour).small());
                        let overrides = entry
                            .override_scopes()
                            .into_iter()
                            .map(|scope| scope.label())
                            .collect::<Vec<_>>();
                        ui.label(
                            RichText::new(if overrides.is_empty() {
                                "Overrides · none".to_string()
                            } else {
                                format!("Overrides · {}", overrides.join(" → "))
                            })
                            .color(theme.text_faint)
                            .small(),
                        );
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // Reset first in right-to-left order, so it sits at the far edge and
                        // the control the user came for is next to its own value.
                        if set_here {
                            let reveals = match entry.origin_without(chosen) {
                                Some(scope) if entry.resolution.origin != Some(chosen) => format!(
                                    "Remove this {} value; {} remains effective",
                                    chosen.label(),
                                    scope.label()
                                ),
                                Some(scope) => format!(
                                    "Remove this {} value; {} takes over again",
                                    chosen.label(),
                                    scope.label()
                                ),
                                None => format!(
                                    "Remove this {} value and go back to Turn's default",
                                    chosen.label()
                                ),
                            };
                            let response = ui.button("Reset").on_hover_text(reveals);
                            crate::icons::describe(
                                &response,
                                &format!("Reset {}", entry.title),
                            );
                            if response.clicked() {
                                let draft_key = settings_draft_key(chosen, &key);
                                state.settings_drafts.remove(&draft_key);
                                state.secret_settings_drafts.0.remove(&draft_key);
                                actions.push(ViewAction::ResetSetting {
                                    scope: chosen,
                                    owner_id: owner.to_string(),
                                    key: key.clone(),
                                });
                            }
                        }
                        if entry.hidden && entry.known && settable {
                            actions.extend(secret_settings_control(
                                ui, state, entry, chosen, owner, &key,
                            ));
                        } else {
                            ui.add_enabled_ui(settable && !entry.hidden, |ui| {
                                actions.extend(settings_control(
                                    ui,
                                    theme,
                                    state,
                                    entry,
                                    &editing_value,
                                    SettingsWriteTarget {
                                        scope: chosen,
                                        owner,
                                        key: &key,
                                    },
                                ));
                            });
                        }
                    });
                });
                if entry.hidden {
                    if entry.known {
                        ui.label(
                            RichText::new(
                                "The current value is never shown. Type the complete replacement and choose Replace; Reset removes this level's copy.",
                            )
                            .color(theme.text_faint)
                            .small(),
                        );
                    } else {
                        ui.label(
                            RichText::new(
                                "Written by a newer version of Turn. Its sensitivity is unknown, so the value stays hidden and can only be reset.",
                            )
                            .color(theme.attention)
                            .small(),
                        );
                    }
                } else if !settable {
                    ui.label(
                        RichText::new(format!(
                            "Cannot be set at the {} level. It belongs to: {}",
                            chosen.label(),
                            entry
                                .settable_at
                                .iter()
                                .map(|scope| scope.label())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ))
                        .color(theme.text_faint)
                        .small(),
                    );
                }
            });
        ui.add_space(5.0);
        actions
    }

    fn settings_sheet(
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
                    // The preferences first, because they are what "Settings" means to
                    // somebody opening this. The Layout presets and the archived filter below
                    // are about what Turn shows rather than how it behaves.
                    actions.extend(self.settings_preferences(ui, theme, state));
                    ui.add_space(12.0);
                    ui.separator();
                    ui.add_space(6.0);
                    // The archived filter used to be a third button in the Workspaces bar,
                    // beside two that create things. It is a preference about what the list
                    // contains, not an action, and it belongs where preferences are.
                    let mut include_archived = self.include_archived;
                    if ui
                        .checkbox(
                            &mut include_archived,
                            "Show archived Workspaces and Sessions",
                        )
                        .changed()
                    {
                        actions.push(ViewAction::SetArchivedVisibility {
                            include: include_archived,
                        });
                    }
                    ui.add_space(12.0);
                    ui.separator();
                    ui.add_space(6.0);
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
            });
        });
        actions
    }
}

/// The control for one preference, and the value it produces when the user finishes with it.
///
/// **Nothing here validates.** The daemon refuses a value out of range and says what would be
/// accepted; a window that also checked would be a second validator able to disagree about
/// which values exist. What the control does is stay inside the bounds it was *told*, which is
/// a different thing: a slider cannot express 400 in the first place, so the user is not
/// offered a value that would be refused.
///
/// **A field commits on Enter or on losing focus, never per keystroke.** A `set_setting` per
/// character would be a round trip per character, and every intermediate value on the way to
/// `14` — `1` — would be refused as out of range, so the user would type into a field that
/// rejected them until they finished.
fn settings_control(
    ui: &mut Ui,
    theme: &Theme,
    state: &mut ViewState,
    entry: &turn_proto::SettingsEntry,
    editing_value: &serde_json::Value,
    target: SettingsWriteTarget<'_>,
) -> Vec<ViewAction> {
    use turn_proto::SettingsControl;
    let SettingsWriteTarget { scope, owner, key } = target;
    let mut actions = Vec::new();
    let set = |value: serde_json::Value| ViewAction::SetSetting {
        scope,
        owner_id: owner.to_string(),
        key: key.to_string(),
        value,
    };
    // Every control carries the preference's own title as its accessible name. There is no
    // DOM here, so a control whose name is only the label drawn beside it is a control a
    // screen reader — and a test — cannot find. The title is already on screen to the left,
    // so the name is set on the widget rather than drawn again.
    match &entry.control {
        SettingsControl::Toggle => {
            let mut on = editing_value.as_bool().unwrap_or(false);
            let response = ui.checkbox(&mut on, "");
            describe_control(&response, egui::accesskit::Role::CheckBox, &entry.title);
            if response.changed() {
                actions.push(set(serde_json::Value::Bool(on)));
            }
        }
        SettingsControl::Integer { min, max } => {
            let mut number = editing_value.as_i64().unwrap_or(*min);
            let response = ui
                .add(egui::DragValue::new(&mut number).range(*min..=*max))
                .on_hover_text(&entry.accepts);
            describe_control(&response, egui::accesskit::Role::SpinButton, &entry.title);
            if response.changed() {
                actions.push(set(serde_json::Value::from(number)));
            }
        }
        SettingsControl::Number { min, max } => {
            let mut number = editing_value.as_f64().unwrap_or(*min);
            let response = ui
                .add(
                    egui::DragValue::new(&mut number)
                        .range(*min..=*max)
                        .speed(0.05),
                )
                .on_hover_text(&entry.accepts);
            describe_control(&response, egui::accesskit::Role::SpinButton, &entry.title);
            if response.changed() {
                actions.push(set(serde_json::json!(number)));
            }
        }
        SettingsControl::Choice { options } => {
            let current = editing_value.as_str().unwrap_or_default().to_string();
            egui::ComboBox::from_id_salt(("setting-choice", scope, key))
                .selected_text(if current.is_empty() {
                    "—".to_string()
                } else {
                    current.clone()
                })
                .show_ui(ui, |ui| {
                    for option in options {
                        if ui
                            .selectable_label(option == &current, option.as_str())
                            .clicked()
                            && option != &current
                        {
                            actions.push(set(serde_json::Value::String(option.clone())));
                        }
                    }
                });
        }
        SettingsControl::MultiChoice { options } => {
            let selected = editing_value.as_array().cloned().unwrap_or_default();
            let selected_strings = selected
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>();
            ui.vertical(|ui| {
                for option in options {
                    let mut enabled = selected_strings.contains(option);
                    let response = ui.checkbox(&mut enabled, option.replace('_', " "));
                    if response.changed() {
                        let mut next = selected_strings.clone();
                        if enabled {
                            if !next.contains(option) {
                                next.push(option.clone());
                            }
                        } else {
                            next.retain(|value| value != option);
                        }
                        actions.push(set(serde_json::Value::Array(
                            next.into_iter().map(serde_json::Value::String).collect(),
                        )));
                    }
                }
            });
        }
        SettingsControl::Text | SettingsControl::TextList | SettingsControl::TextMap => {
            // The stored value as text the user can edit, and back again on commit. One
            // line per item for a list, `NAME=value` per line for a map — which is how the
            // user already writes both of these everywhere else.
            let stored = settings_text_of(editing_value, &entry.control);
            let draft_key = settings_draft_key(scope, key);
            let draft = state
                .settings_drafts
                .entry(draft_key.clone())
                .or_insert_with(|| stored.clone());
            let multiline = !matches!(entry.control, SettingsControl::Text);
            let response = if multiline {
                ui.add(
                    egui::TextEdit::multiline(draft)
                        .desired_rows(2)
                        .desired_width(220.0)
                        .hint_text(&entry.accepts),
                )
            } else {
                ui.add(
                    egui::TextEdit::singleline(draft)
                        .desired_width(220.0)
                        .hint_text(&entry.accepts),
                )
            };
            describe_control(&response, egui::accesskit::Role::TextInput, &entry.title);
            // `has_focus` matters as much as the key: `key_pressed` is window-wide input, so
            // without it one Enter would commit every field on the sheet at once.
            let committed = response.lost_focus()
                || (!multiline
                    && response.has_focus()
                    && ui.input(|input| input.key_pressed(egui::Key::Enter)));
            if committed {
                let typed = draft.clone();
                state.settings_drafts.remove(&draft_key);
                if typed != stored {
                    actions.push(set(settings_value_of(&typed, &entry.control)));
                }
            }
            let _ = theme;
        }
        SettingsControl::Unknown => {
            // Shown as it was stored, with no control. Anything else would be a guess about
            // what a newer build meant, presented as an editor.
            ui.label(
                RichText::new(entry.resolution.value.to_string())
                    .monospace()
                    .color(theme.text_dim)
                    .small(),
            );
        }
    }
    actions
}

#[derive(Clone, Copy)]
struct SettingsWriteTarget<'a> {
    scope: turn_core::settings::Scope,
    owner: &'a str,
    key: &'a str,
}

fn settings_draft_key(scope: turn_core::settings::Scope, key: &str) -> String {
    format!("{}:{key}", scope.label())
}

/// Blind replacement for a secret: the existing value never enters an editable widget and
/// focus changes never commit half a credential. The explicit button is the only write.
fn secret_settings_control(
    ui: &mut Ui,
    state: &mut ViewState,
    entry: &turn_proto::SettingsEntry,
    chosen: turn_core::settings::Scope,
    owner: &str,
    key: &str,
) -> Vec<ViewAction> {
    let draft_key = settings_draft_key(chosen, key);
    let draft = state
        .secret_settings_drafts
        .0
        .entry(draft_key.clone())
        .or_default();
    let response = ui.add(
        egui::TextEdit::multiline(draft)
            .password(true)
            .desired_rows(2)
            .desired_width(180.0)
            .hint_text("NAME=value"),
    );
    describe_control(
        &response,
        egui::accesskit::Role::TextInput,
        &format!("Replacement for {}", entry.title),
    );
    let replace = ui
        .add_enabled(!draft.trim().is_empty(), egui::Button::new("Replace"))
        .on_hover_text("Replace the complete hidden value at this level");
    crate::icons::describe(&replace, &format!("Replace {}", entry.title));
    if !replace.clicked() {
        return Vec::new();
    }
    let typed = state
        .secret_settings_drafts
        .0
        .remove(&draft_key)
        .unwrap_or_default();
    vec![ViewAction::SetSetting {
        scope: chosen,
        owner_id: owner.to_string(),
        key: key.to_string(),
        value: settings_value_of(&typed, &entry.control),
    }]
}

/// Puts a settings control into the accessibility tree under its own name and role.
///
/// `icons::describe` exists for the icon buttons and calls everything a button, which is right
/// for those and wrong here: a checkbox announced as a button tells a listener they can press
/// it and not whether it is on. There is no DOM to infer any of this from, so the role is
/// stated per control.
fn describe_control(response: &Response, role: egui::accesskit::Role, label: &str) {
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            match role {
                egui::accesskit::Role::CheckBox => egui::WidgetType::Checkbox,
                egui::accesskit::Role::TextInput => egui::WidgetType::TextEdit,
                _ => egui::WidgetType::Button,
            },
            response.enabled(),
            label,
        )
    });
    response.ctx.accesskit_node_builder(response.id, |node| {
        node.set_role(role);
        node.set_label(label.to_string());
        node.add_action(egui::accesskit::Action::Click);
    });
}

/// A stored value as the text a user edits.
fn settings_text_of(value: &serde_json::Value, control: &turn_proto::SettingsControl) -> String {
    use turn_proto::SettingsControl;
    match (control, value) {
        (
            SettingsControl::TextList | SettingsControl::MultiChoice { .. },
            serde_json::Value::Array(items),
        ) => items
            .iter()
            .filter_map(|item| item.as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        (SettingsControl::TextMap, serde_json::Value::Object(pairs)) => pairs
            .iter()
            .map(|(name, value)| format!("{name}={}", value.as_str().unwrap_or_default()))
            .collect::<Vec<_>>()
            .join("\n"),
        (_, serde_json::Value::String(text)) => text.clone(),
        // Null is the deliberate nothing, and an empty field is how the user says it.
        (_, serde_json::Value::Null) => String::new(),
        (_, other) => other.to_string(),
    }
}

/// The text a user typed, as the value to store.
///
/// An empty field is `null` — the deliberate "nothing here" — rather than an empty string or
/// an empty list, because that is the value that overrides an inherited something with an
/// absence. Clearing a field and resetting are different acts and produce different results:
/// this one says "nothing, here"; reset says "whatever the level below says".
fn settings_value_of(typed: &str, control: &turn_proto::SettingsControl) -> serde_json::Value {
    use turn_proto::SettingsControl;
    if typed.trim().is_empty() {
        return serde_json::Value::Null;
    }
    match control {
        SettingsControl::TextList | SettingsControl::MultiChoice { .. } => {
            serde_json::Value::Array(
                typed
                    .lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .map(|line| serde_json::Value::String(line.to_string()))
                    .collect(),
            )
        }
        SettingsControl::TextMap => {
            let mut pairs = serde_json::Map::new();
            for line in typed.lines().map(str::trim).filter(|l| !l.is_empty()) {
                // A line with no `=` is a name with no value, which is a name the user is
                // part-way through typing. Kept with an empty value rather than dropped: a
                // line that vanished on commit would look like the field losing text.
                let (name, value) = line.split_once('=').unwrap_or((line, ""));
                pairs.insert(
                    name.trim().to_string(),
                    serde_json::Value::String(value.trim().to_string()),
                );
            }
            serde_json::Value::Object(pairs)
        }
        _ => serde_json::Value::String(typed.to_string()),
    }
}

/// The chord text that means "no opinion, use Turn's own".
///
/// Needed because the preference has three states and a text field only naturally has two: a
/// chord, an empty string that unbinds, and the absence of an entry that inherits. This is how
/// the third is asked for, and it never reaches the stored value — the daemon is asked to
/// remove the key instead.
pub const DEFAULT_CHORD: &str = "<default>";

/// What the window knows about one row that the row itself cannot work out: whether it is
/// the selection, whether it is open, and whether anything of it is on screen.
///
/// A struct rather than four more parameters, because four booleans in a row is a call site
/// nobody can read and an argument order anybody can get wrong.
#[derive(Clone, Copy, Default)]
struct RowState {
    selected: bool,
    expanded: bool,
    /// The pane this row's Process is showing has keyboard focus.
    focused_pane: bool,
    /// This row's Session is the one whose layout the window is drawing.
    active_session: bool,
    /// How long a worker an agent is managing has had nothing to say.
    ///
    /// Computed by the caller rather than in the row, because it needs the whole Session to know
    /// whether this node is being managed at all, and a row only has itself.
    idle: Option<crate::spotlight::Idleness>,
}

fn hierarchy_row(
    ui: &mut Ui,
    theme: &Theme,
    row: HierarchyRow<'_>,
    width: f32,
    visibility: TreeVisibilityMode,
    state: RowState,
) -> Response {
    let RowState {
        selected,
        expanded,
        focused_pane,
        active_session,
        idle: idle_note,
    } = state;
    // The width is passed in rather than read from the `Ui`, so every row in the tree is
    // the same width and the column its controls occupy is in the same place on all of
    // them. See the note where it is measured.
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(width, row.height(visibility)), Sense::click());
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

    // The same indent [`row_text_x`] works from, so the room reserved for the controls is
    // measured against where the text really starts.
    let indent = row_text_x(row) - 15.0;
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
        HierarchyRow::Workspace(workspace) if workspace.workspace.archived => {
            Some("ARCHIVED".into())
        }
        HierarchyRow::Session { session, .. }
            if session.session.status == SessionStatus::Archived =>
        {
            Some("ARCHIVED".into())
        }
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
    // A row carries its lifecycle controls at its right edge. The name and the tag are
    // painted rather than laid out, so the room those controls need has to be subtracted
    // here — otherwise the name runs under a button and is truncated mid-word, which is
    // exactly the failure earlier snapshots caught.
    let reserved = row_action_width(row, rect.width());
    let tag_colour = if needs_user {
        theme.attention
    } else {
        theme.running
    };
    // Measured, not reserved at a guess. The tag column used to be a flat 94 points whether
    // the tag said `ARCHIVED` or nothing at all, and every point of it was taken from the
    // name — which is what left `Task 01 on a longish branch name` ending at `branch`.
    let tag = right_tag.map(|tag| {
        ui.painter().layout_no_wrap(
            tag,
            FontId::new(9.0, egui::FontFamily::Monospace),
            tag_colour,
        )
    });
    let tag_right = rect.max.x - 9.0 - reserved;
    // Two widths, not one, and the controls and the tag are only subtracted from the first.
    // Both sit on the row's *first* line, so charging the second line for them as well is
    // what cut `archived` short to `archivec` and `1 running · enforced` to `enforce` —
    // words halved by room nothing was going to use.
    let title_width = match &tag {
        Some(tag) => tag_right - tag.size().x - 8.0 - text_x,
        None => rect.max.x - 10.0 - reserved - text_x,
    }
    .max(24.0);
    let detail_width = (rect.max.x - 10.0 - text_x).max(24.0);
    // Nothing may draw outside its own row, whatever the arithmetic above says.
    let painter = ui.painter().with_clip_rect(rect);

    match row {
        HierarchyRow::Workspace(workspace) => {
            paint_line(
                &painter,
                egui::pos2(text_x, rect.min.y + 4.0),
                title_width,
                &workspace.workspace.name,
                theme.ui_font.clone(),
                theme.text,
            );
            let mut detail = format!("WORKSPACE · {} sessions", workspace.workspace.session_count);
            if workspace.workspace.archived {
                detail.push_str(" · archived");
            }
            if workspace.workspace.lease_reconciliation_required {
                detail.push_str(" · LEASE CHECK");
            }
            paint_line(
                &painter,
                egui::pos2(text_x, rect.min.y + 22.0),
                detail_width,
                &detail,
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
            paint_line(
                &painter,
                egui::pos2(text_x, rect.min.y + 4.0),
                title_width,
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
            if summary.status == SessionStatus::Archived {
                detail.push_str(" · archived");
            }
            if let Some(guard) = read_only_guard_label(summary) {
                detail.push_str(" · ");
                detail.push_str(guard);
            }
            if summary.muted {
                detail.push_str(" · muted");
            }
            paint_line(
                &painter,
                egui::pos2(text_x, rect.min.y + 23.0),
                detail_width,
                &detail,
                FontId::new(10.0, egui::FontFamily::Monospace),
                colour,
            );
        }
        HierarchyRow::Process { node, .. } => {
            paint_line(
                &painter,
                egui::pos2(text_x, rect.min.y + 3.0),
                title_width,
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
            paint_line(
                &painter,
                egui::pos2(text_x, rect.min.y + 21.0),
                detail_width,
                &format!(
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
            if visibility != TreeVisibilityMode::Normal {
                let idle = idle_note.filter(|idle| idle.worth_saying);
                let (third_line, third_colour) = if visibility == TreeVisibilityMode::Technical {
                    (
                        format!(
                            "pid {} · ppid {} · {}",
                            node.pid.map_or_else(|| "—".into(), |pid| pid.to_string()),
                            node.ppid.map_or_else(|| "—".into(), |pid| pid.to_string()),
                            node.command
                        ),
                        theme.text_faint,
                    )
                } else {
                    match &idle {
                        Some(idle) => (
                            format!(
                                "nothing for {} — click to see its pane",
                                crate::spotlight::describe_silence(idle.silent_ms)
                            ),
                            theme.attention,
                        ),
                        None => (
                            visible_preview(node)
                                .map(|preview| preview.normalized_text.clone())
                                .unwrap_or_else(|| "no activity preview".into()),
                            theme.text_faint,
                        ),
                    }
                };
                paint_line(
                    &painter,
                    egui::pos2(text_x, rect.min.y + 39.0),
                    detail_width,
                    &third_line,
                    FontId::new(10.0, egui::FontFamily::Proportional),
                    third_colour,
                );
            }
        }
    }

    if let Some(tag) = tag {
        // The galley the title width was measured against, so the two cannot disagree.
        painter.galley(
            egui::pos2(tag_right - tag.size().x, rect.min.y + 6.0),
            tag,
            tag_colour,
        );
    }

    let accessible_name = row.accessible_name(focused_pane, visibility);
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

/// Applies the ordinary single-click contract of the unified navigator.
///
/// A Session row is both the navigation destination and the container whose saved
/// Layout belongs in the centre, so selecting it activates that Session immediately.
/// Process rows remain selection-only: inspecting a background Agent must not switch
/// Sessions, open a Pane or disturb the current Layout.
fn select_hierarchy_row(
    state: &mut ViewState,
    snapshot: &HierarchySnapshot,
    row: HierarchyRow<'_>,
) -> Vec<ViewAction> {
    set_hierarchy_selection(state, snapshot, row.key());
    match row {
        HierarchyRow::Session { session, .. } => {
            vec![ViewAction::SelectSession(session.session.id.clone())]
        }
        HierarchyRow::Workspace(_) | HierarchyRow::Process { .. } => Vec::new(),
    }
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

fn set_hierarchy_expanded_all(state: &mut ViewState, snapshot: &HierarchySnapshot, expanded: bool) {
    for row in ordered_hierarchy_rows(snapshot, true, effective_manual_order(snapshot, state)) {
        if row.child_count() > 0 {
            state.tree_expansion.insert(row.key(), expanded);
        }
    }
    state.push_hierarchy_action(HierarchyAction::SetExpandedAll {
        surface_id: snapshot.tree_state.surface_id.clone(),
        expanded,
    });
}

fn hierarchy_reorder_menu(
    ui: &mut Ui,
    state: &mut ViewState,
    snapshot: &HierarchySnapshot,
    row: HierarchyRow<'_>,
) {
    let parent = row.parent_key();
    let key = row.key();
    let siblings: Vec<_> =
        ordered_hierarchy_rows(snapshot, true, effective_manual_order(snapshot, state))
            .into_iter()
            .filter(|candidate| candidate.parent_key() == parent)
            .map(HierarchyRow::key)
            .collect();
    let Some(index) = siblings.iter().position(|candidate| candidate == &key) else {
        return;
    };
    let move_up = ui
        .add_enabled(index > 0, egui::Button::new("Move up"))
        .clicked();
    let move_down = ui
        .add_enabled(index + 1 < siblings.len(), egui::Button::new("Move down"))
        .clicked();
    if !move_up && !move_down {
        ui.separator();
        return;
    }

    let mut reordered = siblings.clone();
    let target = if move_up { index - 1 } else { index + 1 };
    reordered.swap(index, target);
    let before = if move_up {
        Some(siblings[index - 1].clone())
    } else {
        siblings.get(index + 2).cloned()
    };
    let sibling_set: HashSet<_> = siblings.into_iter().collect();
    let mut optimistic: Vec<_> = effective_manual_order(snapshot, state)
        .iter()
        .filter(|candidate| !sibling_set.contains(*candidate))
        .cloned()
        .collect();
    optimistic.extend(reordered);
    state.tree_manual_order = optimistic;
    state.push_hierarchy_action(HierarchyAction::Move {
        surface_id: snapshot.tree_state.surface_id.clone(),
        key,
        before,
    });
    ui.close();
}

fn push_tree_presentation(state: &mut ViewState, snapshot: &HierarchySnapshot) {
    state.push_hierarchy_action(HierarchyAction::SetPresentation {
        surface_id: snapshot.tree_state.surface_id.clone(),
        filters: state.tree_filters.iter().copied().collect(),
        visibility_mode: state.tree_visibility,
        scroll_anchor: state.tree_scroll_anchor.clone(),
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

fn hierarchy_accepts_keyboard(state: &ViewState) -> bool {
    state.tree_has_focus && !state.is_sensitive()
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
    apply_hierarchy_keypress(snapshot, state, rows, keypress)
}

fn apply_hierarchy_keypress(
    snapshot: &HierarchySnapshot,
    state: &mut ViewState,
    rows: &[HierarchyRow<'_>],
    keypress: HierarchyKeypress,
) -> Vec<ViewAction> {
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
    // Three columns, measured right to left. The shortcut column used to be a fixed
    // hundred points, and `Opt+Shift+Left` does not fit in a hundred points: the group name
    // was painted underneath the shortcut's last word, which a recorded palette shows as
    // `PaneOpt+Shift+Left`. Measuring the text is the only version of this that cannot be
    // wrong for a chord somebody rebinds.
    let shortcut = painter.layout_no_wrap(
        row.shortcut.clone().unwrap_or_default(),
        FontId::new(11.0, egui::FontFamily::Monospace),
        theme.text_faint,
    );
    let group = painter.layout_no_wrap(
        row.group.to_string(),
        FontId::new(10.0, egui::FontFamily::Monospace),
        theme.text_faint,
    );
    let shortcut_left = rect.right() - 8.0 - shortcut.size().x;
    let group_left = shortcut_left - 10.0 - group.size().x;
    let title_clip = Rect::from_min_max(
        rect.min,
        egui::pos2((group_left - 8.0).max(rect.min.x), rect.max.y),
    );
    painter.with_clip_rect(title_clip).text(
        rect.left_center() + Vec2::new(8.0, 0.0),
        Align2::LEFT_CENTER,
        row.title,
        theme.ui_font.clone(),
        theme.text,
    );
    painter.galley(
        egui::pos2(shortcut_left, rect.center().y - shortcut.size().y / 2.0),
        shortcut,
        theme.text_faint,
    );
    painter.galley(
        egui::pos2(group_left, rect.center().y - group.size().y / 2.0),
        group,
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

/// The header strip of a pane.
fn pane_header_rect(pane: Rect) -> Rect {
    Rect::from_min_size(pane.min, Vec2::new(pane.width(), PANE_HEADER))
}

/// Where a pane's close control sits inside its header.
fn pane_close_slot(header: Rect) -> Rect {
    Rect::from_min_size(
        header.right_top() + Vec2::new(-PANE_CLOSE_WIDTH - 3.0, 3.0),
        Vec2::new(PANE_CLOSE_WIDTH, PANE_HEADER - 6.0),
    )
}

fn pane_menu_slot(header: Rect) -> Rect {
    let close = pane_close_slot(header);
    Rect::from_min_size(
        close.left_top() - Vec2::new(PANE_CLOSE_WIDTH + 2.0, 0.0),
        close.size(),
    )
}

/// What a drop zone does, said in words.
///
/// Spelled here rather than at the two places that paint it so the region the user sees
/// and the sentence a screen reader hears cannot describe different outcomes.
pub fn drop_zone_phrase(zone: DropZone) -> &'static str {
    match zone {
        DropZone::Left => "left of",
        DropZone::Right => "right of",
        DropZone::Above => "above",
        DropZone::Below => "below",
        DropZone::Centre => "swap with",
    }
}

/// The short form, for a region too narrow to hold the sentence.
fn drop_zone_word(zone: DropZone) -> &'static str {
    match zone {
        DropZone::Left => "left",
        DropZone::Right => "right",
        DropZone::Above => "above",
        DropZone::Below => "below",
        DropZone::Centre => "swap",
    }
}

/// Paints where a dragged pane is about to land.
///
/// The feedback *is* the feature. An outline of the whole target pane says "somewhere in
/// here", which is exactly what five zones exist to stop it saying: the highlight is the
/// shape the pane will occupy, so half a pane means a split and a full pane means an
/// exchange, and the two are told apart without letting go to find out.
///
/// Everything goes into the foreground layer, because the target pane's terminal is
/// painted after this header and would cover anything left underneath it.
fn paint_drop_preview(
    ui: &Ui,
    theme: &Theme,
    source: Rect,
    target: Option<&DropTarget>,
    target_title: &str,
    id: egui::Id,
) {
    let ahead = ui
        .painter()
        .clone()
        .with_layer_id(egui::LayerId::new(egui::Order::Foreground, id));
    // The pane the user picked up, marked as the one in flight for as long as the
    // gesture lasts. It stays where it is: the window rearranges nothing locally.
    ahead.rect_stroke(
        source.shrink(1.0),
        0.0,
        Stroke::new(1.0, theme.provisional),
        egui::StrokeKind::Inside,
    );
    let Some(target) = target else {
        return;
    };

    // Translucent, so the pane underneath is still legible: the user is aiming at its
    // edges and needs to see where they are.
    ahead.rect_filled(target.preview, 0.0, theme.running.gamma_multiply(0.22));
    ahead.rect_stroke(
        target.preview.shrink(1.0),
        0.0,
        Stroke::new(2.0, theme.running),
        egui::StrokeKind::Inside,
    );

    // The words, if they fit. The shape has already said where the pane lands, so a
    // region too small for even one word carries none rather than one that spills.
    let sentence = format!("{} {target_title}", drop_zone_phrase(target.zone));
    paint_widest_that_fits(
        &ahead,
        target.preview.shrink(4.0),
        target.preview.center(),
        &[sentence.as_str(), drop_zone_word(target.zone)],
        FontId::new(11.0, egui::FontFamily::Monospace),
        theme.running,
    );
}

/// Paints the first of several phrasings that fits inside `room`, centred on `at`.
///
/// Text wider than the thing it describes is the failure that keeps coming back to this
/// window: a caption painted past the edge of a narrow pane lands on the pane next door and
/// says something untrue about it. Returns whether anything was painted; nothing is, when
/// even the shortest phrasing does not fit, because a legible layout is worth more than a
/// label.
fn paint_widest_that_fits(
    painter: &egui::Painter,
    room: Rect,
    at: egui::Pos2,
    options: &[&str],
    font: FontId,
    colour: Color32,
) -> bool {
    for text in options {
        let galley = painter.layout_no_wrap((*text).to_string(), font.clone(), colour);
        if galley.size().x <= room.width() && galley.size().y <= room.height() {
            painter.galley(at - galley.size() / 2.0, galley, colour);
            return true;
        }
    }
    false
}

/// The two things a pane header is for besides saying what is in the pane: closing it,
/// and moving it.
///
/// `Command::ClosePane` has existed since the first keymap and had no affordance at all,
/// which for anybody who has not read the shortcut sheet is the same as not existing. The
/// control is here, on the pane it closes, and its tooltip carries the chord so the window
/// teaches the keyboard rather than replacing it.
///
/// Moving is a drag of the header onto one of another pane's five regions — its four edges
/// and its middle — which is the gesture every tiling editor already taught the user, and
/// which needs no instructions as long as the feedback is right. It is not the only way:
/// `MovePane…` is bound in all four directions and relocates exactly the same way, because
/// a drag is unusable without a pointer. Both go through the daemon: the request names two
/// panes and a zone, and the window redraws whatever layout comes back, so a refused move
/// leaves the window agreeing with the daemon instead of showing a rearrangement that did
/// not happen.
fn pane_header_controls(
    ui: &mut Ui,
    theme: &Theme,
    keymap: &Keymap,
    placed: &panes::PaneRect,
    arrangement: &Arrangement,
    drag: &mut Option<PaneId>,
    title: &str,
) -> Vec<ViewAction> {
    let mut actions = Vec::new();
    let header = pane_header_rect(placed.rect);
    let close_slot = pane_close_slot(header);
    let menu_slot = pane_menu_slot(header);

    let grip = Rect::from_min_max(
        header.min,
        egui::pos2((menu_slot.min.x - 2.0).max(header.min.x), header.max.y),
    );
    let grip_response = ui.interact(
        grip,
        ui.id().with(("pane-header", placed.pane_id.as_str())),
        Sense::click_and_drag(),
    );
    let movable = arrangement.panes.len() > 1;
    if movable && (grip_response.hovered() || grip_response.dragged()) {
        ui.ctx().set_cursor_icon(if grip_response.dragged() {
            egui::CursorIcon::Grabbing
        } else {
            egui::CursorIcon::Grab
        });
    }

    // Recorded from `dragged` rather than only from `drag_started`, so a gesture whose
    // first frame was missed — a pane that appeared under a pointer already down — is still
    // known to be in progress.
    if movable && grip_response.dragged() && drag.as_ref() != Some(&placed.pane_id) {
        *drag = Some(placed.pane_id.clone());
    }
    // Recomputed from the pointer every frame rather than remembered: a layout arriving
    // from the daemon in the middle of a drag must not leave a landing spot on screen
    // that no longer exists. After Escape there is no drag left to ask, so this is `None`
    // and the gesture ends having changed nothing.
    let landing = grip_response
        .interact_pointer_pos()
        .and_then(|pointer| arrangement.drop_target_at(&placed.pane_id, pointer));
    let landing_title = landing
        .as_ref()
        .and_then(|landing| arrangement.pane(&landing.pane_id))
        .map(pane_title_of)
        .unwrap_or_default();

    if movable && grip_response.dragged() {
        paint_drop_preview(
            ui,
            theme,
            placed.rect,
            landing.as_ref(),
            &landing_title,
            ui.id().with("pane-move-hint"),
        );
    }
    if grip_response.drag_stopped() {
        // A drop outside every pane, or on the pane itself, leaves the layout exactly as it
        // was: `landing` is `None` and nothing is sent.
        if let Some(landing) = &landing {
            actions.push(ViewAction::RelocatePane {
                moved: placed.pane_id.clone(),
                target: landing.pane_id.clone(),
                zone: landing.zone,
            });
        }
        if drag.as_ref() == Some(&placed.pane_id) {
            *drag = None;
        }
    } else if grip_response.clicked() {
        actions.push(ViewAction::Pane {
            pane_id: placed.pane_id.clone(),
            action: PaneAction::Focus,
        });
    }
    let move_shortcut = keymap
        .chord_for(Command::MovePaneRight)
        .map(|chord| chord.describe(keymap.platform()))
        .unwrap_or_else(|| "the move pane commands".to_string());
    // The accessible name carries the whole gesture, including what the drag is currently
    // about to do: a screen reader has no highlighted region to read.
    let grip_name = match &landing {
        Some(landing) => format!(
            "{title} pane header — moving {} {landing_title}; Escape cancels",
            drop_zone_phrase(landing.zone)
        ),
        None if grip_response.dragged() => format!(
            "{title} pane header — moving it; drop it on another pane's edge or middle, or press Escape to cancel"
        ),
        None => format!(
            "{title} pane header — click to focus, drag onto another pane's edge to move it there or its middle to swap"
        ),
    };
    icons::describe(&grip_response, &grip_name);
    grip_response.on_hover_text(format!(
        "{title} — drag this header onto another pane: its edges put this pane beside it, its middle swaps the two. Or press {move_shortcut}"
    ));

    ui.scope_builder(
        keyed_region(menu_slot, "pane-menu", placed.pane_id.as_str()),
        |ui| {
            ui.menu_button("...", |ui| {
                if ui.button("Duplicate view").clicked() {
                    actions.push(ViewAction::DuplicatePane {
                        pane_id: placed.pane_id.clone(),
                    });
                    ui.close();
                }
                ui.menu_button("View type", |ui| {
                    for (label, kind) in pane_kind_choices() {
                        if ui.selectable_label(placed.kind == kind, label).clicked() {
                            actions.push(ViewAction::ChangePaneKind {
                                pane_id: placed.pane_id.clone(),
                                kind,
                            });
                            ui.close();
                        }
                    }
                });
                ui.separator();
                if ui
                    .add_enabled(movable, egui::Button::new("Detach as floating Pane"))
                    .on_disabled_hover_text("Keep at least one Pane docked in the Session.")
                    .clicked()
                {
                    actions.push(ViewAction::FloatPane {
                        pane_id: placed.pane_id.clone(),
                        geometry: PaneGeometry {
                            x: placed.rect.min.x + 32.0,
                            y: placed.rect.min.y + 32.0,
                            width: placed.rect.width().max(480.0),
                            height: placed.rect.height().max(320.0),
                        },
                    });
                    ui.close();
                }
            });
        },
    );

    let close_shortcut = keymap
        .chord_for(Command::ClosePane)
        .map(|chord| chord.describe(keymap.platform()));
    let close_name = format!("Close pane {title}");
    ui.scope_builder(
        keyed_region(close_slot, "pane-close", placed.pane_id.as_str()),
        |ui| {
            // Through the shared placement, which is what keeps it inside its slot: added to
            // this region directly it was sized from the style's interaction floor — 28 points
            // tall in a 16-point header — so the hover frame overflowed the header and was
            // clipped, which is what "the close button does not draw properly" was.
            let close = icons::glyph_button(
                ui,
                close_slot,
                icons::CLOSE,
                12.0,
                true,
                Some(theme.text_dim),
            );
            icons::describe(&close, &close_name);
            let hint = match &close_shortcut {
                Some(chord) => format!(
                    "{close_name} · {chord} — the process keeps running; stopping it is a separate command"
                ),
                None => format!(
                    "{close_name} — the process keeps running; stopping it is a separate command"
                ),
            };
            if close.on_hover_text(hint).clicked() {
                actions.push(ViewAction::ClosePane {
                    pane_id: placed.pane_id.clone(),
                });
            }
        },
    );
    actions
}

fn pane_kind_choices() -> [(&'static str, PaneKind); 13] {
    [
        ("Terminal", PaneKind::Terminal),
        ("Agent terminal", PaneKind::Agent),
        ("Shell", PaneKind::Shell),
        ("Terminal app", PaneKind::Tui),
        ("Logs", PaneKind::Logs),
        ("Test output", PaneKind::TestOutput),
        ("Server", PaneKind::Server),
        ("Event log", PaneKind::EventLog),
        ("Agent tree", PaneKind::AgentTree),
        ("Process details", PaneKind::ProcessDetails),
        ("Preview", PaneKind::Preview),
        ("tmux terminal", PaneKind::TmuxTerminal),
        ("Placeholder", PaneKind::Placeholder),
    ]
}

/// What a pane calls itself, for a sentence about it.
fn pane_title_of(pane: &panes::PaneRect) -> String {
    pane.title
        .clone()
        .unwrap_or_else(|| format!("{:?}", pane.kind).to_lowercase())
}

/// Which way a directional pane command points.
///
/// Focusing and moving share the geometry deliberately: "move left" has to land where
/// "focus left" would have gone, or the two commands would disagree about which pane is
/// on the left.
pub fn side_for(command: Command) -> Option<Side> {
    match command {
        Command::FocusPaneLeft | Command::MovePaneLeft => Some(Side::Left),
        Command::FocusPaneRight | Command::MovePaneRight => Some(Side::Right),
        Command::FocusPaneUp | Command::MovePaneUp => Some(Side::Up),
        Command::FocusPaneDown | Command::MovePaneDown => Some(Side::Down),
        _ => None,
    }
}

/// The pane a directional command would move to, given what is on screen.
pub fn neighbour_for(arrangement: &Arrangement, from: &PaneId, command: Command) -> Option<PaneId> {
    panes::neighbour(arrangement, from, side_for(command)?)
}

/// The relocation a `MovePane…` command means: the pane to name, and where beside it the
/// moved pane lands.
///
/// `None` when there is nowhere to go, which the caller reports rather than sending a
/// request that could only be refused.
pub fn relocation_for(
    arrangement: &Arrangement,
    from: &PaneId,
    command: Command,
) -> Option<(PaneId, DropZone)> {
    panes::relocation(arrangement, from, side_for(command)?)
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
                ..TreeSurfaceState::empty("window-test")
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
    fn a_context_handoff_from_the_palette_uses_the_exact_selected_agent() {
        let (snapshot, root_id, child_id, session_id) = hierarchy_fixture();
        let draft = ContextHandoffDraft::from_selection(
            &snapshot,
            Some(&HierarchyKey::process(root_id.clone())),
        )
        .expect("the selected Agent has one same-Session destination");

        assert_eq!(draft.session_id, session_id);
        assert_eq!(draft.source_node_id, root_id);
        assert_eq!(draft.target_node_id, Some(child_id));
        assert_eq!(draft.mode, ContextHandoffMode::ContinueWith);
        assert!(ContextHandoffDraft::from_selection(
            &snapshot,
            Some(&HierarchyKey::session(draft.session_id))
        )
        .is_none());
    }

    #[test]
    fn read_only_guard_state_is_explicit_in_visual_and_accessibility_copy() {
        let (mut snapshot, _, _, _) = hierarchy_fixture();
        let summary = &mut snapshot.workspaces[0].sessions[0].session;
        assert_eq!(
            read_only_guard_label(summary),
            Some("read-only guard enforced; checkout writes blocked")
        );

        summary.read_only_enforced = false;
        assert_eq!(
            read_only_guard_label(summary),
            Some("read-only guard unavailable; processes disabled")
        );
        let row = HierarchyRow::Session {
            workspace: &snapshot.workspaces[0],
            session: &snapshot.workspaces[0].sessions[0],
        };
        assert!(row
            .accessible_name(false, TreeVisibilityMode::Expanded)
            .contains("read-only guard unavailable; processes disabled"));
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
        let collapsed = visible_hierarchy_rows(&snapshot, &state, false);
        assert_eq!(collapsed.len(), 3, "workspace, session, collapsed agent");
        assert_eq!(
            collapsed.iter().map(|row| row.depth()).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert!(!collapsed
            .iter()
            .any(|row| row.key() == HierarchyKey::process(child_id.clone())));

        set_hierarchy_expanded(&mut state, &snapshot, HierarchyKey::process(root_id), true);
        let expanded = visible_hierarchy_rows(&snapshot, &state, false);
        assert_eq!(expanded.len(), 4);
        assert_eq!(expanded.last().map(|row| row.depth()), Some(3));
        assert!(expanded
            .iter()
            .any(|row| row.key() == HierarchyKey::process(child_id.clone())));
    }

    #[test]
    fn search_keeps_ancestors_and_keyboard_navigation_stays_inside_the_projection() {
        let (snapshot, root_id, child_id, _) = hierarchy_fixture();
        let workspace_key = HierarchyKey::workspace(snapshot.workspaces[0].workspace.id.clone());
        let session_key =
            HierarchyKey::session(snapshot.workspaces[0].sessions[0].session.id.clone());
        let mut state = ViewState {
            tree_query: "navigation projection".into(),
            selected_tree: Some(workspace_key.clone()),
            ..ViewState::default()
        };
        let rows = visible_hierarchy_rows(&snapshot, &state, false);
        assert_eq!(
            rows.iter().map(|row| row.key()).collect::<Vec<_>>(),
            [
                workspace_key,
                session_key.clone(),
                HierarchyKey::process(root_id.clone()),
            ],
            "a preview hit keeps its Workspace and Session but not an unrelated child"
        );

        apply_hierarchy_keypress(&snapshot, &mut state, &rows, HierarchyKeypress::Down);
        assert_eq!(state.selected_tree, Some(session_key));
        apply_hierarchy_keypress(&snapshot, &mut state, &rows, HierarchyKeypress::Down);
        assert_eq!(state.selected_tree, Some(HierarchyKey::process(root_id)));
        assert_ne!(state.selected_tree, Some(HierarchyKey::process(child_id)));
    }

    #[test]
    fn filter_families_or_within_the_family_and_and_across_families() {
        let (snapshot, _, _, _) = hierarchy_fixture();
        let mut state = ViewState::default();
        state.tree_filters.extend([
            TreeFilter::Agents,
            TreeFilter::ReadOnly,
            TreeFilter::Running,
            TreeFilter::Failed,
        ]);
        let rows = visible_hierarchy_rows(&snapshot, &state, false);
        assert_eq!(
            rows.len(),
            4,
            "Running OR Failed, intersected with Agents and Read-only"
        );

        state.tree_filters.remove(&TreeFilter::ReadOnly);
        state.tree_filters.insert(TreeFilter::Main);
        assert!(
            visible_hierarchy_rows(&snapshot, &state, false).is_empty(),
            "a Read-only Session cannot leak through the Main mode filter"
        );
    }

    fn loaded_hierarchy_fixture() -> (HierarchySnapshot, Vec<NodeId>) {
        let workspace = Workspace::new("scale", "/repo/scale", T0);
        let mut session = Session::new(
            workspace.id.clone(),
            "Thirty workers",
            "/repo/scale",
            Layout::single(Pane::new(PaneKind::Agent)),
            T0,
        );
        let mut agent_ids = Vec::new();
        for worker in 0..30 {
            let mut agent = ProcessNode::agent(
                session.id.clone(),
                format!("worker-{worker}"),
                "/repo/scale",
                T0 + worker,
            );
            agent.lifecycle = Lifecycle::Alive;
            agent.turn = Some(Turn::Active);
            agent.pid = Some(20_000 + worker as u32);
            let agent_id = session.tree.insert(agent);
            agent_ids.push(agent_id.clone());
            for tool in 0..10 {
                let mut process = ProcessNode::process(
                    session.id.clone(),
                    NodeKind::Background,
                    format!("worker-{worker}-tool-{tool}"),
                    "/repo/scale",
                    T0 + 100 + worker * 10 + tool,
                );
                process.lifecycle = Lifecycle::Alive;
                process.pid = Some(30_000 + (worker * 10 + tool) as u32);
                process.ppid = Some(20_000 + worker as u32);
                process.link_to(agent_id.clone(), Relation::Confirmed);
                session.tree.insert(process);
            }
        }
        let summary = SessionSummary::from_session(&session, 0, false, T0 + 1_000);
        let workspace_summary =
            WorkspaceSummary::from_workspace(&workspace, std::slice::from_ref(&summary));
        let mut nodes = TreeNodeView::for_session(&session, T0 + 1_000);
        for node in &mut nodes {
            node.ephemeral = node.kind == NodeKind::Background;
        }
        let mut expanded = vec![
            HierarchyKey::workspace(workspace.id.clone()),
            HierarchyKey::session(session.id.clone()),
        ];
        expanded.extend(agent_ids.iter().cloned().map(HierarchyKey::process));
        (
            HierarchySnapshot {
                revision: 1,
                tree_state: TreeSurfaceState {
                    surface_id: "window-scale".into(),
                    expanded,
                    ..TreeSurfaceState::empty("window-scale")
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
            },
            agent_ids,
        )
    }

    #[test]
    fn thirty_agents_and_hundreds_of_discovered_processes_have_reproducible_projection() {
        let (mut snapshot, agent_ids) = loaded_hierarchy_fixture();
        let mut state = ViewState {
            tree_visibility: TreeVisibilityMode::Technical,
            ..ViewState::default()
        };
        let technical = visible_hierarchy_rows(&snapshot, &state, false);
        assert_eq!(
            technical.len(),
            332,
            "Workspace + Session + 30 Agents + 300 processes"
        );
        let last_process = technical.last().copied().expect("last discovered process");
        assert!(last_process
            .accessible_name(false, TreeVisibilityMode::Technical)
            .contains("pid 30299"));

        state.tree_visibility = TreeVisibilityMode::Normal;
        let normal = visible_hierarchy_rows(&snapshot, &state, false);
        assert_eq!(
            normal.len(),
            32,
            "Normal hides all 300 ephemeral process-table rows"
        );

        state.tree_query = "worker-29-tool-9".into();
        let searched = visible_hierarchy_rows(&snapshot, &state, false);
        assert_eq!(
            searched.len(),
            4,
            "search reveals one ephemeral hit and its ancestors"
        );
        assert_eq!(searched.last().map(|row| row.depth()), Some(3));

        state.tree_query.clear();
        state.tree_filters.insert(TreeFilter::Agents);
        state.tree_filters.insert(TreeFilter::Running);
        let filtered = visible_hierarchy_rows(&snapshot, &state, false);
        assert_eq!(filtered.len(), 32);

        // Manual ordering changes sibling order only; selection remains the exact stable key.
        let selected = HierarchyKey::process(agent_ids[0].clone());
        state.selected_tree = Some(selected.clone());
        snapshot.tree_state.manual_order = vec![
            HierarchyKey::process(agent_ids[1].clone()),
            selected.clone(),
        ];
        state.tree_manual_order.clear();
        let manually_ordered = visible_hierarchy_rows(&snapshot, &state, false);
        let agent_positions: Vec<_> = manually_ordered
            .iter()
            .filter_map(|row| match row {
                HierarchyRow::Process { node, .. } if node.is_agentic => Some(node.node_id.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(agent_positions[0], agent_ids[1]);
        assert_eq!(agent_positions[1], agent_ids[0]);
        assert_eq!(state.selected_tree, Some(selected));
    }

    #[test]
    fn expand_and_collapse_all_are_single_durable_actions() {
        let (snapshot, _, _, _) = hierarchy_fixture();
        let mut state = ViewState::default();
        set_hierarchy_expanded_all(&mut state, &snapshot, true);
        assert!(state.tree_expansion.values().all(|expanded| *expanded));
        assert!(matches!(
            state.take_hierarchy_actions().as_slice(),
            [HierarchyAction::SetExpandedAll { expanded: true, .. }]
        ));
        set_hierarchy_expanded_all(&mut state, &snapshot, false);
        assert!(state.tree_expansion.values().all(|expanded| !*expanded));
    }

    #[test]
    fn selection_expansion_and_focus_are_different_typed_actions() {
        let (snapshot, root_id, _, _) = hierarchy_fixture();
        let mut state = ViewState::default();
        let key = HierarchyKey::process(root_id.clone());

        set_hierarchy_expanded(&mut state, &snapshot, key.clone(), true);
        let process = visible_hierarchy_rows(&snapshot, &state, false)
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
    fn clicking_a_session_selects_it_and_activates_its_layout() {
        let (snapshot, _, _, session_id) = hierarchy_fixture();
        let session = &snapshot.workspaces[0].sessions[0];
        let mut state = ViewState::default();

        let actions = select_hierarchy_row(
            &mut state,
            &snapshot,
            HierarchyRow::Session {
                workspace: &snapshot.workspaces[0],
                session,
            },
        );

        assert_eq!(
            state.selected_tree,
            Some(HierarchyKey::session(session_id.clone()))
        );
        assert!(matches!(
            actions.as_slice(),
            [ViewAction::SelectSession(selected)] if selected == &session_id
        ));
        assert!(state.take_hierarchy_actions().iter().any(|action| matches!(
            action,
            HierarchyAction::Select {
                key: HierarchyKey::Session { session_id: selected },
                ..
            } if selected == &session_id
        )));
    }

    #[test]
    fn clicking_a_background_process_does_not_replace_the_active_session_layout() {
        let (snapshot, root_id, _, _) = hierarchy_fixture();
        let session = &snapshot.workspaces[0].sessions[0];
        let process = session
            .nodes
            .iter()
            .find(|node| node.node_id == root_id)
            .expect("root process");
        let mut state = ViewState::default();

        let actions = select_hierarchy_row(
            &mut state,
            &snapshot,
            HierarchyRow::Process {
                session,
                node: process,
            },
        );

        assert!(actions.is_empty());
        assert_eq!(
            effective_selection(&snapshot, &state),
            Some(HierarchyKey::process(root_id))
        );
    }

    #[test]
    fn node_less_attention_is_visible_without_relabelling_the_running_agent() {
        let (snapshot, root_id, _, _) = hierarchy_fixture();
        let workspace = HierarchyRow::Workspace(&snapshot.workspaces[0]);
        assert!(workspace
            .accessible_name(false, TreeVisibilityMode::Expanded)
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
        .accessible_name(false, TreeVisibilityMode::Expanded)
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
                priority_boost: 0,
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
        assert_eq!(side_for(Command::MovePaneLeft), Some(Side::Left));
        assert_eq!(side_for(Command::MovePaneRight), Some(Side::Right));
        assert_eq!(side_for(Command::MovePaneUp), Some(Side::Up));
        assert_eq!(side_for(Command::MovePaneDown), Some(Side::Down));
        assert_eq!(side_for(Command::ZoomPane), None);
        assert_eq!(side_for(Command::CyclePane), None);
    }

    /// The toolbar has to survive a window nobody sized for it. Dropping buttons from the
    /// end is the behaviour; drawing them past the edge of their zone, on top of the
    /// version, is the failure this replaced.
    #[test]
    fn a_toolbar_too_narrow_for_every_button_drops_them_from_the_end() {
        assert_eq!(toolbar_capacity(0.0), 0);
        assert_eq!(toolbar_capacity(icons::SIZE.x - 1.0), 0);
        assert_eq!(toolbar_capacity(icons::SIZE.x), 1);
        assert_eq!(toolbar_capacity(icons::PITCH + icons::SIZE.x), 2);
        assert_eq!(
            toolbar_capacity(10_000.0),
            TOOLBAR.len(),
            "a wide window shows all of them and invents none"
        );
        // Whatever fits, it fits: the room the buttons need never exceeds the room given.
        for width in [30.0_f32, 61.0, 100.0, 187.0, 260.0, 400.0] {
            let count = toolbar_capacity(width);
            let needed = count as f32 * icons::PITCH - (icons::PITCH - icons::SIZE.x);
            assert!(
                needed <= width + 0.01,
                "{count} buttons need {needed} points but were given {width}"
            );
        }
    }

    /// The rows gained controls, and the thing that goes wrong when a row runs out of room
    /// is a name cut off mid-word. At the tree's normal width every row can afford its
    /// controls *and* a legible name; in a tree squeezed to two hundred points the controls
    /// give way instead of the name, because a name identifies the row and the controls are
    /// also on its context menu and on the keyboard.
    #[test]
    fn a_row_too_narrow_for_a_name_and_its_controls_keeps_the_name() {
        let (snapshot, _, _, _) = hierarchy_fixture();
        let workspace = &snapshot.workspaces[0];
        let session = &workspace.sessions[0];
        let rows = [
            HierarchyRow::Workspace(workspace),
            HierarchyRow::Session { workspace, session },
        ];

        for row in rows {
            let reserved = row_action_width(row, SIDEBAR_WIDTH);
            assert!(
                reserved > 0.0,
                "the tree's own width must fit a row's controls"
            );
            // What the name is left with, in the case that reserves the most: a row with a
            // status tag on it.
            let left_for_name = SIDEBAR_WIDTH - row_text_x(row) - TAG_COLUMN - reserved;
            assert!(
                left_for_name >= ROW_MIN_TITLE,
                "a name would be left {left_for_name} points"
            );
            assert_eq!(
                row_action_width(row, 218.0),
                0.0,
                "a tree this narrow must keep the name rather than the controls"
            );
        }

        // A Process row has no controls at any width: stopping one Agent is its own action
        // in its own menu, not a lifecycle act on the tree.
        let process = HierarchyRow::Process {
            session,
            node: &session.nodes[0],
        };
        for width in [120.0, SIDEBAR_WIDTH, 1_000.0] {
            assert_eq!(row_action_width(process, width), 0.0);
        }
    }

    /// Archiving is only believable if the row leaves. The preference in Settings decides
    /// what the tree contains, and it decides it here as well as in the request.
    #[test]
    fn an_archived_row_is_out_of_the_tree_until_the_preference_asks_for_it() {
        let (mut snapshot, _, _, session_id) = hierarchy_fixture();
        snapshot.workspaces[0].sessions[0].session.status = SessionStatus::Archived;
        let state = ViewState::default();

        let hidden = visible_hierarchy_rows(&snapshot, &state, false);
        assert!(
            !hidden
                .iter()
                .any(|row| row.key() == HierarchyKey::session(session_id.clone())),
            "the archived Session must leave the tree"
        );
        assert!(
            hidden.iter().any(|row| matches!(
                row,
                HierarchyRow::Workspace(workspace) if !workspace.workspace.archived
            )),
            "its Workspace stays: it is not the thing that was archived"
        );
        assert_eq!(
            hidden.len(),
            1,
            "and its Processes go with it rather than being left parentless"
        );

        let shown = visible_hierarchy_rows(&snapshot, &state, true);
        assert!(
            shown
                .iter()
                .any(|row| row.key() == HierarchyKey::session(session_id.clone())),
            "and the preference brings it back"
        );

        // An archived Workspace takes its Sessions out of the tree with it.
        snapshot.workspaces[0].workspace.archived = true;
        assert!(visible_hierarchy_rows(&snapshot, &state, false).is_empty());
        assert!(!visible_hierarchy_rows(&snapshot, &state, true).is_empty());
    }

    /// Every toolbar button says what it does in words, and every one of them is an
    /// action that already existed. A toolbar entry with no label would be a control
    /// recognisable only by its picture.
    #[test]
    fn every_toolbar_button_has_words_and_a_distinct_icon() {
        let mut labels: Vec<&str> = TOOLBAR.iter().map(|button| button.label).collect();
        let count = labels.len();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), count, "two toolbar buttons share a label");
        let mut glyphs: Vec<&str> = TOOLBAR.iter().map(|button| button.icon).collect();
        glyphs.sort_unstable();
        glyphs.dedup();
        assert_eq!(glyphs.len(), count, "two toolbar buttons share an icon");
        for button in TOOLBAR {
            assert!(
                button.label.len() > 3,
                "{:?} has no words to announce",
                button.intent
            );
            if let ToolbarIntent::Run(command) = button.intent {
                assert!(
                    Command::ALL.contains(&command),
                    "{command:?} is on the toolbar but not in the palette"
                );
            }
        }
    }

    /// Moving and focusing must resolve to the same neighbour, and a move must name the
    /// side it is going to. A layout where "focus right" and "move right" disagreed would
    /// be a pane that moved somewhere other than where the user was looking.
    #[test]
    fn moving_a_pane_resolves_to_the_same_neighbour_as_focusing_one_and_names_that_side() {
        let mut layout = Layout::single(Pane::new(PaneKind::Terminal).with_title("left"));
        let left = layout.panes()[0].id.clone();
        layout.split(
            &left,
            Direction::Horizontal,
            Pane::new(PaneKind::Terminal).with_title("right"),
        );
        let arrangement = panes::arrange(
            &layout,
            Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(1000.0, 600.0)),
        );
        for (moving, focusing, zone) in [
            // Rightwards there is a neighbour and the pane lands past it. Upwards there is
            // none, and the move becomes one to the outer edge — the row becomes a column —
            // rather than nothing. Leftwards this pane is the only one against that edge, so
            // there is genuinely nowhere to go.
            (
                Command::MovePaneRight,
                Command::FocusPaneRight,
                Some(DropZone::Right),
            ),
            (Command::MovePaneLeft, Command::FocusPaneLeft, None),
            (
                Command::MovePaneUp,
                Command::FocusPaneUp,
                Some(DropZone::Above),
            ),
        ] {
            assert_eq!(
                neighbour_for(&arrangement, &left, moving),
                neighbour_for(&arrangement, &left, focusing),
                "{moving:?} and {focusing:?} disagree about which pane is there"
            );
            assert_eq!(
                relocation_for(&arrangement, &left, moving).map(|(_, zone)| zone),
                zone,
                "{moving:?} must land on the side it is named after"
            );
        }
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
    fn an_end_session_dialog_blocks_tree_shortcuts_behind_it() {
        let mut state = ViewState {
            tree_has_focus: true,
            lifecycle_confirmation: Some(LifecycleConfirmation::EndSession {
                session_id: SessionId::from_stored("sess_modal_shortcuts"),
                name: "Do not mutate behind me".into(),
                running_count: 2,
                escaped_count: 0,
            }),
            ..ViewState::default()
        };
        assert!(!hierarchy_accepts_keyboard(&state));

        state.lifecycle_confirmation = None;
        assert!(hierarchy_accepts_keyboard(&state));
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
    fn a_secret_draft_is_redacted_from_window_diagnostics() {
        let secret = "token-that-must-never-reach-a-log";
        let mut state = ViewState::default();
        state
            .secret_settings_drafts
            .0
            .insert("Global:environment.variables".into(), secret.into());
        let diagnostic = format!("{state:?}");
        assert!(
            !diagnostic.contains(secret),
            "diagnostic leaked {diagnostic}"
        );
        assert!(diagnostic.contains("entries: 1"));
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
