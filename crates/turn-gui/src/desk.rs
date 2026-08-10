//! What the window knows, and what it does about what arrives.
//!
//! This is the whole application except the drawing: the sessions, the selected one, a
//! screen per attached pane, the attention queue, and the rules for turning a command or
//! a push into requests. It is deliberately free of `egui`, so the behaviour that matters
//! can be tested by feeding it protocol messages and reading back what it would send.
//!
//! ## It computes nothing the daemon computed
//!
//! Display states, state labels, severities, badge counts, queue order and focus
//! decisions all arrive derived. The desk stores and renders them. The one thing it does
//! compute is geometry — rectangles, which pane is to the left — because the daemon has
//! no pixels.
//!
//! ## Three product rules live here as code paths that do not exist
//!
//! There is no path from a permission to an automatic write: answering an agent is
//! [`Reaction::Send`] carrying `write_pty` with bytes the user typed. The distinct
//! reviewed context-handoff route is refused while any interaction is pending. There is no path
//! from a heuristic to a focus change: focus moves only when [`turn_core::Effect::Focus`]
//! arrives, and `focus_deferred` and `focus_denied` are dropped by
//! [`crate::announce::Announcement`]. And nothing relaunches: `relaunch_node` is only
//! ever sent from an explicit command.

use std::collections::{BTreeMap, HashMap, HashSet};

use turn_core::attention::{AttentionPolicy, Effect};
use turn_core::ids::{HandoffId, NodeId, PaneId, SessionId, TemplateId, WorkspaceId};
use turn_core::model::{
    ActivityPreview, Direction, Layout, LeaseState, PaneKind, PreviewVisibility, SessionStatus,
};
use turn_core::state::Lifecycle;
use turn_proto::cells::Grid;
use turn_proto::{
    AttentionView, CloseDisposition, ContextHandoffText, ContextHandoffView, FocusTarget,
    HierarchyKey, HierarchySnapshot, NewPane, NodePaneCapability, NodePaneView, ProtoErrorContext,
    PtySize, Request, Response, SessionConflictAlternative, SessionSummary, TemplateSummary,
    TerminalBytes, TreeNodeView, WorkspaceSummary,
};

use crate::announce::Announcement;
use crate::keymap::Command;
use crate::panes::{self, Arrangement};
use crate::terminal::feed::{Desync, PaneFeed};
use crate::terminal::PaneAction;
use crate::transport::{Ask, ConnectionState, Inbound};
use crate::view::{
    HierarchyAction, LifecycleConfirmation, PaneContent, PendingPermission, QueueItem,
    SessionDraft, SessionRestoreView, SessionRow, TemporaryPaneContent, TurnView, ViewAction,
};

/// Something the application must do as a result.
///
/// Returned rather than performed so the state machine has no I/O in it: the caller owns
/// the socket, the clipboard and the notification centre.
#[derive(Debug, Clone, PartialEq)]
pub enum Reaction {
    Send {
        ask: Ask,
        request: Request,
    },
    Announce(Announcement),
    Copy(String),
    /// Something to show the user in the status area.
    Notice(String),
    WorkspaceCreated {
        workspace_id: WorkspaceId,
        continue_to_session: bool,
    },
    SessionCreated {
        session_id: SessionId,
    },
    WorkspaceCreationFailed(String),
    SessionCreationFailed(String),
    SessionCreationCancelled,
    TemplateCreated {
        template_id: TemplateId,
    },
    TemplateCreationFailed(String),
    ContextHandoffPrepared(ContextHandoffView),
    ContextHandoffDelivered {
        handoff_id: HandoffId,
    },
    ContextHandoffPrepareFailed {
        session_id: SessionId,
        source_node_id: NodeId,
        target_node_id: NodeId,
        message: String,
    },
    ContextHandoffDeliveryFailed {
        handoff_id: HandoffId,
        message: String,
    },
    ContextHandoffInvalidated,
}

/// The default geometry a pane is attached at, before it has been laid out.
///
/// Replaced by the real size on the first frame, from the rectangle the pane actually
/// occupies. It exists because `attach_pane` needs a size and the answer is not known
/// until something has been drawn.
const INITIAL_SIZE: PtySize = PtySize { rows: 24, cols: 80 };

/// The original daemon-owned Template intent retained while the user chooses a
/// typed lease-conflict alternative. It deliberately stores no flattened panes:
/// only `turnd` may turn a Template id into commands, environment and policy.
#[derive(Debug, Clone)]
struct PendingSessionDraft {
    workspace_id: WorkspaceId,
    template_id: turn_core::ids::TemplateId,
    name: Option<String>,
    cwd: Option<String>,
    branch: Option<String>,
    task: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PartialRevision {
    Apply,
    IgnoreStale,
    Resync,
}

/// Everything the window is showing.
pub struct Desk {
    connection: ConnectionState,
    notice: Option<String>,
    /// Launcher-specific diagnostics disappear only after a real protocol handshake.
    /// Request errors are independent and must not be erased by connecting.
    companion_notice: Option<String>,
    /// The sole persistent navigation projection. Flat collections below are
    /// compatibility indexes for commands and pane ownership, never a second
    /// navigation model.
    hierarchy: Option<HierarchySnapshot>,
    include_archived: bool,
    surface_id: String,
    /// Window-local selection that has rendered but may not have been acknowledged by
    /// the daemon yet. Creation commands must follow what the user can see, not the
    /// previous persisted row.
    navigation_hint: Option<HierarchyKey>,
    preview_history: HashMap<NodeId, Vec<ActivityPreview>>,
    /// Per-Session recovery offers emitted after a daemon restart. Kept structured so
    /// one Session cannot overwrite another with a global red string.
    restores: HashMap<SessionId, SessionRestoreView>,
    relaunching: HashSet<NodeId>,
    /// Nodes this window has already tried to start on its own.
    ///
    /// Separate from [`Self::relaunching`], which is cleared when a request finishes — including
    /// when it *fails*. Without a second record a failing relaunch would be asked for again on
    /// the next sweep, and again, for as long as the window stayed open. A node is tried once;
    /// after that the pane's own button is the way, and it is the user asking rather than Turn.
    auto_started: HashSet<NodeId>,
    reclaiming_leases: HashSet<WorkspaceId>,
    temporary_pane: Option<NodePaneView>,
    write_conflict: Option<ProtoErrorContext>,
    pending_workspace_creation: bool,
    pending_session: Option<PendingSessionDraft>,
    workspaces: Vec<WorkspaceSummary>,
    templates: Vec<TemplateSummary>,
    /// In the daemon's own order, re-sorted locally with the daemon's own ranking after
    /// a push so a state change moves a row without a round trip.
    sessions: Vec<SessionSummary>,
    selected: Option<SessionId>,
    /// A layout per Session the window has fetched. The selected Session and explicit
    /// temporary Panes are the only layouts that cause terminal attachments; cached
    /// layouts never become a second navigation or monitoring surface.
    layouts: HashMap<SessionId, Layout>,
    trees: HashMap<SessionId, Vec<TreeNodeView>>,
    policies: HashMap<SessionId, AttentionPolicy>,
    /// Which session a pane belongs to.
    pane_owner: HashMap<PaneId, SessionId>,
    /// A screen per pane the window has attached to.
    feeds: BTreeMap<PaneId, PaneFeed>,
    attaching: HashSet<PaneId>,
    /// The size each pane's pty was last told, so a resize is sent on a change.
    pty_sizes: HashMap<PaneId, PtySize>,
    queue: Vec<AttentionView>,
    /// The last arrangement drawn, for directional pane navigation — which is a question
    /// about rectangles and therefore needs the ones that were on screen.
    arrangement: Arrangement,
}

impl Default for Desk {
    fn default() -> Self {
        Self::new()
    }
}

impl Desk {
    pub fn new() -> Self {
        Desk {
            connection: ConnectionState::Starting,
            notice: None,
            companion_notice: None,
            hierarchy: None,
            include_archived: false,
            surface_id: "main-window".to_string(),
            navigation_hint: None,
            preview_history: HashMap::new(),
            restores: HashMap::new(),
            relaunching: HashSet::new(),
            auto_started: HashSet::new(),
            reclaiming_leases: HashSet::new(),
            temporary_pane: None,
            write_conflict: None,
            pending_workspace_creation: false,
            pending_session: None,
            workspaces: Vec::new(),
            templates: Vec::new(),
            sessions: Vec::new(),
            selected: None,
            layouts: HashMap::new(),
            trees: HashMap::new(),
            policies: HashMap::new(),
            pane_owner: HashMap::new(),
            feeds: BTreeMap::new(),
            attaching: HashSet::new(),
            pty_sizes: HashMap::new(),
            queue: Vec::new(),
            arrangement: Arrangement::default(),
        }
    }

    pub fn connection(&self) -> &ConnectionState {
        &self.connection
    }

    /// Shows a startup failure before a protocol connection exists. Request failures
    /// normally arrive through [`Inbound`]; companion launch is necessarily earlier.
    pub fn show_companion_notice(&mut self, message: impl Into<String>) {
        self.companion_notice = Some(message.into());
    }

    pub fn selected(&self) -> Option<&SessionId> {
        self.selected.as_ref()
    }

    pub fn hierarchy(&self) -> Option<&HierarchySnapshot> {
        self.hierarchy.as_ref()
    }

    pub fn preview_history(&self, node: &NodeId) -> &[ActivityPreview] {
        self.preview_history
            .get(node)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn preview_histories(&self) -> &HashMap<NodeId, Vec<ActivityPreview>> {
        &self.preview_history
    }

    pub fn temporary_pane(&self) -> Option<&NodePaneView> {
        self.temporary_pane.as_ref()
    }

    pub fn write_conflict(&self) -> Option<&ProtoErrorContext> {
        self.write_conflict.as_ref()
    }

    pub fn sessions(&self) -> &[SessionSummary] {
        &self.sessions
    }

    pub fn queue(&self) -> &[AttentionView] {
        &self.queue
    }

    pub fn has_workspaces(&self) -> bool {
        !self.workspaces.is_empty()
    }

    /// Serialises the creation lifecycle. Until protocol operations carry durable
    /// operation ids, allowing two creates in flight would let a late lease conflict
    /// borrow the other form's Template and task.
    pub fn creation_in_progress(&self) -> bool {
        self.pending_workspace_creation
            || self.pending_session.is_some()
            || self.write_conflict.is_some()
    }

    /// Mirrors the tree's immediate-mode optimistic selection for the next command.
    /// The key is accepted only when it resolves inside the current hierarchy.
    pub fn set_navigation_hint(&mut self, key: Option<HierarchyKey>) {
        self.navigation_hint = key.filter(|key| {
            self.hierarchy
                .as_ref()
                .and_then(|hierarchy| workspace_for_key(hierarchy, key))
                .is_some()
        });
    }

    /// Makes a command-level failure visible in the window instead of leaving it in
    /// tracing alone. Inbound protocol failures already set this themselves; local
    /// guards (for example Quick New before templates arrive) use this path.
    pub fn show_notice(&mut self, message: impl Into<String>) {
        self.notice = Some(message.into());
    }

    pub fn new_session_draft(&self) -> Option<SessionDraft> {
        let workspace_id = self.current_workspace()?;
        let template_id = self
            .preferred_template(&workspace_id)
            .map(|template| template.id.clone());
        Some(SessionDraft::new(workspace_id, template_id))
    }

    pub fn new_session_draft_for(&self, workspace_id: WorkspaceId) -> SessionDraft {
        let template_id = self
            .preferred_template(&workspace_id)
            .map(|template| template.id.clone());
        SessionDraft::new(workspace_id, template_id)
    }

    /// The workspace a new session would go in.
    ///
    /// The selected session's, or the first there is. `None` when the daemon has not
    /// answered yet, which is why every command that needs one checks.
    fn current_workspace(&self) -> Option<WorkspaceId> {
        self.hierarchy
            .as_ref()
            .zip(self.navigation_hint.as_ref())
            .and_then(|(hierarchy, key)| workspace_for_key(hierarchy, key))
            .or_else(|| {
                self.hierarchy
                    .as_ref()
                    .and_then(|hierarchy| {
                        hierarchy
                            .tree_state
                            .selected
                            .as_ref()
                            .map(|key| (hierarchy, key))
                    })
                    .and_then(|(hierarchy, key)| workspace_for_key(hierarchy, key))
            })
            .or_else(|| {
                self.selected_summary()
                    .map(|summary| summary.workspace_id.clone())
            })
            .or_else(|| self.workspaces.first().map(|w| w.id.clone()))
    }

    fn preferred_template(&self, workspace_id: &WorkspaceId) -> Option<&TemplateSummary> {
        let configured = self
            .workspaces
            .iter()
            .find(|workspace| &workspace.id == workspace_id)
            .and_then(|workspace| workspace.default_template.as_ref());
        configured
            .and_then(|id| self.templates.iter().find(|template| &template.id == id))
            .or_else(|| self.templates.first())
    }

    fn selected_summary(&self) -> Option<&SessionSummary> {
        let id = self.selected.as_ref()?;
        self.sessions.iter().find(|summary| &summary.id == id)
    }

    /// The row the tree is pointing at: the window's optimistic selection when it has one,
    /// and the daemon's persisted selection otherwise.
    fn selected_tree_key(&self) -> Option<&HierarchyKey> {
        self.navigation_hint.as_ref().or_else(|| {
            self.hierarchy
                .as_ref()
                .and_then(|hierarchy| hierarchy.tree_state.selected.as_ref())
        })
    }

    /// The Session a lifecycle command acts on.
    ///
    /// The tree's selected row when that row is a Session or one of its Processes, and the
    /// Session whose panes are on screen otherwise. A selected *Workspace* deliberately
    /// resolves to nothing: "the first Session in it" is not what the user pointed at, and
    /// a command that guessed would archive or close a Session nobody named.
    pub fn target_session(&self) -> Option<&SessionSummary> {
        let from_tree = self.selected_tree_key().and_then(|key| match key {
            HierarchyKey::Session { session_id } => Some(session_id.clone()),
            HierarchyKey::Process { .. } => self
                .hierarchy
                .as_ref()
                .and_then(|snapshot| session_for_key(snapshot, key)),
            HierarchyKey::Workspace { .. } => None,
        });
        let id = from_tree.or_else(|| self.selected.clone())?;
        self.sessions.iter().find(|summary| summary.id == id)
    }

    /// The Workspace a lifecycle command acts on, with its Sessions, so a confirmation can
    /// say how much of the world it is about to reach.
    ///
    /// A Session or Process selection resolves to the Workspace that contains it, which is
    /// the one the row is visibly nested under.
    pub fn target_workspace_branch(&self) -> Option<&turn_proto::WorkspaceTreeView> {
        let workspace_id = self.current_workspace()?;
        self.hierarchy
            .as_ref()?
            .workspaces
            .iter()
            .find(|branch| branch.workspace.id == workspace_id)
    }

    /// The confirmation for closing the Session the tree points at, or the reason there is
    /// none.
    ///
    /// A `Result` rather than an `Option` because the window must show one or the other and
    /// never both, and because the reason belongs next to the rule that produced it rather
    /// than at the call site.
    pub fn end_session_confirmation(&self) -> Result<LifecycleConfirmation, &'static str> {
        let Some(summary) = self.target_session() else {
            return Err("select a Session in the tree before closing it");
        };
        if summary.status == SessionStatus::Archived {
            return Err("this Session is archived and has nothing running; restore it first");
        }
        Ok(LifecycleConfirmation::end_session(summary))
    }

    /// The same for a whole Workspace. Closing one reaches every Session in it, which is
    /// why the confirmation is built from the tree branch and not from a name.
    pub fn stop_workspace_confirmation(&self) -> Result<LifecycleConfirmation, &'static str> {
        let Some(branch) = self.target_workspace_branch() else {
            return Err("select a Workspace in the tree before closing it");
        };
        if branch.workspace.archived {
            return Err("this Workspace is archived and has nothing running; restore it first");
        }
        Ok(LifecycleConfirmation::stop_workspace(branch))
    }

    /// The confirmation for deleting the Session the tree is pointing at.
    ///
    /// Unlike ending it, an archived Session is a valid target — it is the likeliest one. A
    /// Session that has been filed away is exactly what somebody clearing out their tree wants
    /// to be rid of, and refusing here would leave an archived row with no way out at all.
    pub fn delete_session_confirmation(&self) -> Result<LifecycleConfirmation, &'static str> {
        let Some(summary) = self.target_session() else {
            return Err("select a Session in the tree before deleting it");
        };
        Ok(LifecycleConfirmation::delete_session(summary))
    }

    /// The same for a whole Workspace, built from the tree branch so the dialog can say how
    /// many Sessions it reaches and which directory it leaves alone.
    pub fn delete_workspace_confirmation(&self) -> Result<LifecycleConfirmation, &'static str> {
        let Some(branch) = self.target_workspace_branch() else {
            return Err("select a Workspace in the tree before deleting it");
        };
        Ok(LifecycleConfirmation::delete_workspace(branch))
    }

    /// Whether starting a process for a Session would violate recovery or archival
    /// state. Every launch surface (toolbar, keymap and context action) uses the same
    /// guard so a shortcut cannot bypass a disabled button.
    ///
    /// `needs_checkout_write` is what the launch would actually do with the shared
    /// checkout. Archival blocks everything — an archived Session is not a place to work
    /// — but withheld write access only blocks what would use it. Opening a terminal is
    /// not a write to the checkout, and blocking it left the user without the one thing
    /// they needed to go and stop the process Turn was asking them about.
    fn session_launch_blocked(&self, session_id: &SessionId, needs_checkout_write: bool) -> bool {
        if self
            .sessions
            .iter()
            .find(|summary| &summary.id == session_id)
            .is_some_and(|summary| summary.status == SessionStatus::Archived)
        {
            return true;
        }
        self.hierarchy.as_ref().is_some_and(|snapshot| {
            snapshot.workspaces.iter().any(|workspace| {
                let Some(session) = workspace
                    .sessions
                    .iter()
                    .find(|session| &session.session.id == session_id)
                else {
                    return false;
                };
                if workspace.workspace.archived {
                    return true;
                }
                needs_checkout_write
                    && (self.reclaiming_leases.contains(&workspace.workspace.id)
                        || workspace.write_lease.as_ref().is_some_and(|lease| {
                            &lease.session_id == session_id
                                && lease.state == LeaseState::RecoveryRequired
                        })
                        || session
                            .nodes
                            .iter()
                            .any(|node| node.lifecycle == Lifecycle::Orphaned))
            })
        })
    }

    fn selected_launch_blocked(&self, needs_checkout_write: bool) -> bool {
        self.selected
            .as_ref()
            .is_some_and(|session_id| self.session_launch_blocked(session_id, needs_checkout_write))
    }

    /// Whether starting this node's pane again would use the Session's checkout write
    /// authority, according to the daemon's own restore report.
    ///
    /// Unknown means yes: the field only ever unlocks something, so a node with no report
    /// is treated as the gated case.
    fn node_needs_checkout_write(&self, session_id: &SessionId, node_id: &NodeId) -> bool {
        self.restores
            .get(session_id)
            .and_then(|restore| {
                restore
                    .panes
                    .iter()
                    .find(|outcome| &outcome.node_id == node_id)
            })
            .is_none_or(|outcome| outcome.needs_checkout_write)
    }

    fn node_is_orphaned(&self, session_id: &SessionId, node_id: &NodeId) -> bool {
        self.trees
            .get(session_id)
            .and_then(|nodes| nodes.iter().find(|node| &node.node_id == node_id))
            .is_some_and(|node| node.lifecycle == Lifecycle::Orphaned)
    }

    /// The selected session's layout, which is what decides the geometry on screen.
    fn layout(&self) -> Option<&Layout> {
        self.layouts.get(self.selected.as_ref()?)
    }

    /// The pane the daemon says has focus in the selected session.
    pub fn active_pane(&self) -> Option<PaneId> {
        self.layout()?.active.clone()
    }

    /// The node behind a pane, for a request addressed to the process.
    fn node_of(&self, pane: &PaneId) -> Option<NodeId> {
        if let Some(temporary) = &self.temporary_pane {
            if &temporary.binding.pane_id == pane {
                return Some(temporary.binding.node_id.clone());
            }
        }
        let session = self.pane_owner.get(pane)?;
        self.layouts
            .get(session)?
            .get(pane)
            .and_then(|pane| pane.node_id.clone())
    }

    /// Applies a message from the daemon.
    pub fn apply_inbound(&mut self, message: Inbound, now_ms: i64) -> Vec<Reaction> {
        let mut out = self.apply_inbound_message(message, now_ms);
        // After every message, because any of them can be the one that revealed a stopped pane:
        // the layout arriving, the tree arriving, a process exiting, a write confirmation being
        // granted. Sweeping after each is what makes this impossible to miss — the two earlier
        // attempts each hooked a single event and each missed the case that was reported.
        out.extend(self.autostart_stopped_panes());
        out
    }

    fn apply_inbound_message(&mut self, message: Inbound, now_ms: i64) -> Vec<Reaction> {
        match message {
            Inbound::Status(state) => self.apply_status(state),
            Inbound::Event(event) => self.apply_event(*event, now_ms),
            Inbound::Answer { ask, response } => self.apply_answer(ask, *response),
            Inbound::Failed { ask, error } => {
                match &ask {
                    Ask::PrepareContextHandoff {
                        session_id,
                        source_node_id,
                        target_node_id,
                    } => {
                        return vec![Reaction::ContextHandoffPrepareFailed {
                            session_id: session_id.clone(),
                            source_node_id: source_node_id.clone(),
                            target_node_id: target_node_id.clone(),
                            message: error.message,
                        }];
                    }
                    Ask::DeliverContextHandoff { handoff_id, .. } => {
                        return vec![Reaction::ContextHandoffDeliveryFailed {
                            handoff_id: handoff_id.clone(),
                            message: error.message,
                        }];
                    }
                    _ => {}
                }
                match &ask {
                    Ask::RelaunchNode { node_id, .. } => {
                        self.relaunching.remove(node_id);
                    }
                    Ask::RestoreLeaseAcquire { workspace_id, .. } => {
                        self.reclaiming_leases.remove(workspace_id);
                    }
                    _ => {}
                }
                let lease_conflict = match (
                    &ask,
                    error.context.as_deref(),
                    self.pending_session.as_ref(),
                ) {
                    (
                        Ask::CreateSession { workspace_id },
                        Some(
                            context @ ProtoErrorContext::WorkspaceWriteLeaseConflict {
                                workspace_id: conflicted,
                                ..
                            },
                        ),
                        Some(draft),
                    ) if conflicted == workspace_id && &draft.workspace_id == workspace_id => {
                        Some(context.clone())
                    }
                    _ => None,
                };
                if let Some(context) = lease_conflict.clone() {
                    self.write_conflict = Some(context);
                } else {
                    match &ask {
                        Ask::CreateWorkspace { .. } => {
                            self.pending_workspace_creation = false;
                        }
                        Ask::CreateSession { .. } => {
                            self.write_conflict = None;
                            self.pending_session = None;
                        }
                        _ => {}
                    }
                }
                if ask.is_worth_reporting() {
                    let message = format!("{}: {}", ask.describing(), error.message);
                    self.notice = Some(message.clone());
                    let mut reactions = vec![Reaction::Notice(message.clone())];
                    if lease_conflict.is_none() {
                        match ask {
                            Ask::CreateWorkspace { .. } => {
                                reactions.push(Reaction::WorkspaceCreationFailed(message));
                            }
                            Ask::CreateSession { .. } => {
                                reactions.push(Reaction::SessionCreationFailed(message));
                            }
                            Ask::CreateTemplate => {
                                reactions.push(Reaction::TemplateCreationFailed(message));
                            }
                            _ => {}
                        }
                    }
                    return reactions;
                }
                Vec::new()
            }
            Inbound::Notice(error) => {
                self.notice = Some(error.message.clone());
                vec![Reaction::Notice(error.message)]
            }
        }
    }

    fn apply_status(&mut self, state: ConnectionState) -> Vec<Reaction> {
        let refetch = match &state {
            // A daemon that restarted has none of our attachments, and a first
            // connection has nothing cached to keep. Either way the world is re-fetched:
            // applying pushes to a stale copy is how a sidebar starts disagreeing with
            // the terminal.
            ConnectionState::Connected { .. } => true,
            _ => false,
        };
        self.connection = state;
        if !refetch {
            return vec![Reaction::ContextHandoffInvalidated];
        }
        let workspace_creation_interrupted = self.pending_workspace_creation;
        let session_creation_interrupted = self.pending_session.is_some();
        self.companion_notice = None;
        self.feeds.clear();
        self.attaching.clear();
        self.pty_sizes.clear();
        self.layouts.clear();
        self.trees.clear();
        self.policies.clear();
        self.pane_owner.clear();
        self.hierarchy = None;
        self.navigation_hint = None;
        self.preview_history.clear();
        self.restores.clear();
        self.relaunching.clear();
        self.reclaiming_leases.clear();
        self.temporary_pane = None;
        self.write_conflict = None;
        self.pending_workspace_creation = false;
        self.pending_session = None;
        self.workspaces.clear();
        self.sessions.clear();
        self.selected = None;
        let mut reactions = vec![
            Reaction::ContextHandoffInvalidated,
            Reaction::Send {
                ask: Ask::Hierarchy,
                request: Request::GetHierarchy {
                    surface_id: self.surface_id.clone(),
                    include_archived: self.include_archived,
                },
            },
            Reaction::Send {
                ask: Ask::Templates,
                request: Request::ListTemplates,
            },
            Reaction::Send {
                ask: Ask::AttentionQueue,
                request: Request::ListAttention { session_id: None },
            },
        ];
        // The transport fences every request to one socket generation and reports a
        // failure before reconnecting. Keep this defensive path too: a synthetic or
        // future transport must never leave a disabled creation sheet behind merely
        // because its connection changed underneath it.
        if workspace_creation_interrupted {
            reactions.push(Reaction::WorkspaceCreationFailed(
                "the daemon reconnected before the Workspace was created; review and try again"
                    .into(),
            ));
        }
        if session_creation_interrupted {
            reactions.push(Reaction::SessionCreationFailed(
                "the daemon reconnected before the Session was created; review and try again"
                    .into(),
            ));
        }
        reactions
    }

    fn apply_answer(&mut self, ask: Ask, response: Response) -> Vec<Reaction> {
        match (ask, response) {
            (
                Ask::RestoreLeaseAcquire {
                    workspace_id,
                    session_id,
                    checkout_id,
                },
                Response::WorkspaceWriteLease {
                    workspace_id: answered_workspace,
                    lease,
                },
            ) if answered_workspace == workspace_id => {
                let valid = lease.as_ref().is_some_and(|lease| {
                    lease.workspace_id == workspace_id
                        && lease.session_id == session_id
                        && lease.checkout_id == checkout_id
                        && lease.state == LeaseState::Active
                });
                if !valid {
                    self.reclaiming_leases.remove(&workspace_id);
                    let message =
                        "the daemon did not return the confirmed active write lease".to_string();
                    self.notice = Some(message.clone());
                    return vec![Reaction::Notice(message)];
                }
                if let Some(branch) = self.hierarchy.as_mut().and_then(|hierarchy| {
                    hierarchy
                        .workspaces
                        .iter_mut()
                        .find(|branch| branch.workspace.id == workspace_id)
                }) {
                    branch.write_lease = lease;
                }
                self.reclaiming_leases.remove(&workspace_id);
                self.notice = None;
                Vec::new()
            }
            (
                Ask::RelaunchNode {
                    session_id,
                    node_id,
                },
                Response::Node { .. },
            ) => {
                self.relaunching.remove(&node_id);
                self.resolve_restore_outcome(&session_id, &node_id);
                vec![Reaction::Send {
                    ask: Ask::Details(session_id.clone()),
                    request: Request::GetSession { session_id },
                }]
            }
            (
                Ask::CloseSession {
                    session_id,
                    disposition,
                },
                Response::Ack,
            ) => {
                self.drop_session_feeds(&session_id);
                if disposition != CloseDisposition::KeepProcesses {
                    self.restores.remove(&session_id);
                }
                if disposition == CloseDisposition::KeepProcesses
                    && self.selected.as_ref() == Some(&session_id)
                {
                    self.selected = None;
                    if let Some(next) = self
                        .sessions
                        .iter()
                        .find(|session| session.id != session_id)
                        .map(|session| session.id.clone())
                    {
                        return self.select(next);
                    }
                    return vec![Reaction::Send {
                        ask: Ask::Action("clearing the detached Session selection"),
                        request: Request::SelectTreeNode {
                            surface_id: self.surface_id.clone(),
                            selected: None,
                        },
                    }];
                }
                Vec::new()
            }
            (
                Ask::CloseWorkspace {
                    workspace_id,
                    disposition,
                },
                Response::Ack,
            ) => {
                let sessions: HashSet<SessionId> = self
                    .hierarchy
                    .as_ref()
                    .and_then(|snapshot| {
                        snapshot
                            .workspaces
                            .iter()
                            .find(|branch| branch.workspace.id == workspace_id)
                    })
                    .map(|branch| {
                        branch
                            .sessions
                            .iter()
                            .map(|session| session.session.id.clone())
                            .collect()
                    })
                    .unwrap_or_default();
                for session_id in &sessions {
                    self.drop_session_feeds(session_id);
                }
                if disposition != CloseDisposition::KeepProcesses {
                    self.restores
                        .retain(|session_id, _| !sessions.contains(session_id));
                }
                let selected_closed = self
                    .selected
                    .as_ref()
                    .is_some_and(|session_id| sessions.contains(session_id));
                let mut reactions = vec![Reaction::Send {
                    ask: Ask::Action("collapsing the stopped Workspace"),
                    request: Request::SetTreeExpanded {
                        surface_id: self.surface_id.clone(),
                        key: HierarchyKey::workspace(workspace_id.clone()),
                        expanded: false,
                    },
                }];
                if selected_closed {
                    self.selected = None;
                    if let Some(next) = self.hierarchy.as_ref().and_then(|snapshot| {
                        snapshot
                            .workspaces
                            .iter()
                            .filter(|branch| branch.workspace.id != workspace_id)
                            .flat_map(|branch| branch.sessions.iter())
                            .map(|session| session.session.id.clone())
                            .next()
                    }) {
                        reactions.extend(self.select(next));
                    } else {
                        reactions.push(Reaction::Send {
                            ask: Ask::Action("clearing the stopped Workspace selection"),
                            request: Request::SelectTreeNode {
                                surface_id: self.surface_id.clone(),
                                selected: None,
                            },
                        });
                    }
                }
                reactions
            }
            (Ask::ArchiveSession { .. } | Ask::ArchiveWorkspace { .. }, Response::Ack) => {
                // The tree is refetched rather than edited in place. Whether an archived
                // row still belongs in it is a question about the archived preference and
                // about what else the daemon has, so the daemon answers it — and the row
                // leaves, or comes back, as a consequence of the same request that decides
                // what the tree contains.
                vec![self.hierarchy_request()]
            }
            (_, Response::Hierarchy { snapshot }) => self.replace_hierarchy(*snapshot, true),
            (_, Response::TreeState { state }) => {
                if let Some(hierarchy) = self.hierarchy.as_mut() {
                    hierarchy.tree_state = state;
                }
                Vec::new()
            }
            (
                _,
                Response::WorkspaceWriteLease {
                    workspace_id,
                    lease,
                },
            ) => {
                if let Some(branch) = self.hierarchy.as_mut().and_then(|hierarchy| {
                    hierarchy
                        .workspaces
                        .iter_mut()
                        .find(|branch| branch.workspace.id == workspace_id)
                }) {
                    branch.write_lease = lease;
                }
                Vec::new()
            }
            (_, Response::Workspaces { workspaces }) => {
                self.workspaces = workspaces;
                Vec::new()
            }
            (
                Ask::CreateWorkspace {
                    continue_to_session,
                },
                Response::Workspace { workspace },
            ) => {
                let workspace_id = workspace.id.clone();
                self.notice = None;
                self.pending_workspace_creation = false;
                self.workspaces.retain(|known| known.id != workspace_id);
                self.workspaces.push(workspace);
                vec![
                    self.hierarchy_request(),
                    Reaction::Send {
                        ask: Ask::Action("expanding the new Workspace"),
                        request: Request::SetTreeExpanded {
                            surface_id: self.surface_id.clone(),
                            key: HierarchyKey::workspace(workspace_id.clone()),
                            expanded: true,
                        },
                    },
                    Reaction::Send {
                        ask: Ask::Action("selecting the new Workspace"),
                        request: Request::SelectTreeNode {
                            surface_id: self.surface_id.clone(),
                            selected: Some(HierarchyKey::workspace(workspace_id.clone())),
                        },
                    },
                    Reaction::WorkspaceCreated {
                        workspace_id,
                        continue_to_session,
                    },
                ]
            }
            (_, Response::Workspace { workspace }) => {
                self.workspaces.retain(|known| known.id != workspace.id);
                self.workspaces.push(workspace);
                vec![self.hierarchy_request()]
            }
            (_, Response::Templates { templates }) => {
                self.templates = templates;
                Vec::new()
            }
            (Ask::CreateTemplate, Response::Template { template }) => {
                let template_id = template.id.clone();
                self.templates.retain(|known| known.id != template_id);
                self.templates.push(template);
                self.templates.sort_by(|left, right| {
                    right
                        .built_in
                        .cmp(&left.built_in)
                        .then_with(|| left.name.cmp(&right.name))
                });
                vec![Reaction::TemplateCreated { template_id }]
            }
            (_, Response::Template { template }) => {
                self.templates.retain(|known| known.id != template.id);
                self.templates.push(template);
                Vec::new()
            }
            (_, Response::Sessions { sessions }) => {
                self.sessions = sessions;
                self.sort_sessions();
                // Nothing selected yet: open the one the daemon ranks first, which is
                // the one that needs the user most.
                if self.selected.is_none() {
                    if let Some(first) = self.sessions.first().map(|s| s.id.clone()) {
                        return self.select(first);
                    }
                }
                Vec::new()
            }
            (Ask::CreateSession { workspace_id }, Response::Session { session }) => {
                if session.workspace_id != workspace_id {
                    self.pending_session = None;
                    return vec![Reaction::SessionCreationFailed(
                        "the daemon returned the new Session in a different Workspace".into(),
                    )];
                }
                let session_id = session.id.clone();
                self.notice = None;
                self.write_conflict = None;
                self.pending_session = None;
                self.upsert_session(*session);
                let mut reactions = vec![
                    self.hierarchy_request(),
                    Reaction::Send {
                        ask: Ask::Action("expanding the Session's Workspace"),
                        request: Request::SetTreeExpanded {
                            surface_id: self.surface_id.clone(),
                            key: HierarchyKey::workspace(workspace_id),
                            expanded: true,
                        },
                    },
                ];
                reactions.extend(self.select(session_id.clone()));
                reactions.push(Reaction::SessionCreated { session_id });
                reactions
            }
            (_, Response::Session { session }) => {
                self.upsert_session(*session);
                vec![self.hierarchy_request()]
            }
            (_, Response::SessionDetails { details }) => {
                let details = *details;
                let session_id = details.summary.id.clone();
                self.upsert_session(details.summary);
                self.trees.insert(session_id.clone(), details.tree);
                self.policies.insert(session_id.clone(), details.attention);
                self.remember_layout(session_id, details.layout);
                self.attach_wanted()
            }
            (_, Response::Layout { session_id, layout }) => self.apply_layout(&session_id, layout),
            (
                Ask::Attach {
                    session_id,
                    pane_id,
                },
                Response::Attached { attachment },
            ) => {
                self.attaching.remove(&pane_id);
                let is_recovery_offer = self.restores.get(&session_id).is_some_and(|restore| {
                    restore.panes.iter().any(|pane| pane.pane_id == pane_id)
                });
                let current_owner = self.pane_owner.get(&pane_id);
                let current_node = self.node_of(&pane_id);
                let stale = is_recovery_offer
                    || attachment.session_id != session_id
                    || attachment.pane_id != pane_id
                    || current_owner != Some(&session_id)
                    || attachment.node_id != current_node;
                if stale {
                    // Restore, close and relaunch can all cross an in-flight AttachPane.
                    // Runtime identity must still match the current Layout before a
                    // screen is accepted under this visual PaneId.
                    self.feeds.remove(&pane_id);
                    self.pty_sizes.remove(&pane_id);
                    return Vec::new();
                }
                self.feeds.insert(pane_id, PaneFeed::attach(&attachment));
                Vec::new()
            }
            (
                _,
                Response::Screen {
                    pane_id,
                    next_seq,
                    grid,
                    ..
                },
            ) => {
                if let Some(feed) = self.feeds.get_mut(&pane_id) {
                    feed.resync(*grid, next_seq);
                }
                Vec::new()
            }
            (
                _,
                Response::PreviewHistory {
                    node_id, entries, ..
                },
            ) => {
                self.preview_history.insert(node_id, entries);
                Vec::new()
            }
            (
                Ask::PrepareContextHandoff {
                    session_id,
                    source_node_id,
                    target_node_id,
                },
                Response::ContextHandoff { handoff },
            ) => {
                if handoff.session_id != session_id
                    || handoff.source_node_id != source_node_id
                    || handoff.target_node_id != target_node_id
                {
                    return vec![Reaction::ContextHandoffPrepareFailed {
                        session_id,
                        source_node_id,
                        target_node_id,
                        message: "the daemon returned a context draft for different Agents".into(),
                    }];
                }
                vec![Reaction::ContextHandoffPrepared(*handoff)]
            }
            (Ask::DeliverContextHandoff { handoff_id, .. }, Response::Ack) => {
                vec![Reaction::ContextHandoffDelivered { handoff_id }]
            }
            (_, Response::NodePane { pane }) => {
                let session_id = pane.binding.session_id.clone();
                let node_id = pane.binding.node_id.clone();
                let pane_id = pane.binding.pane_id.clone();
                self.pane_owner.insert(pane_id.clone(), session_id.clone());
                let terminal = matches!(pane.capability, NodePaneCapability::Terminal { .. });
                self.temporary_pane = Some(pane);
                let mut reactions = vec![Reaction::Send {
                    ask: Ask::Preview {
                        session_id: session_id.clone(),
                        node_id: node_id.clone(),
                    },
                    request: Request::GetPreviewHistory {
                        session_id: session_id.clone(),
                        node_id,
                        limit: Some(8),
                    },
                }];
                if terminal {
                    self.attaching.insert(pane_id.clone());
                    reactions.push(Reaction::Send {
                        ask: Ask::Attach {
                            session_id: session_id.clone(),
                            pane_id: pane_id.clone(),
                        },
                        request: Request::AttachPane {
                            session_id,
                            pane_id,
                            size: INITIAL_SIZE,
                            stream: turn_proto::PaneStream::Cells,
                        },
                    });
                }
                reactions
            }
            (
                Ask::AttentionFocus {
                    session_id,
                    subject_node_id,
                },
                Response::PaneFocus { focus },
            ) => {
                let Some(focus) = focus else {
                    // Selection still landed on the exact semantic subject. With no
                    // Pane on the trusted runtime boundary there is nowhere honest to
                    // send keyboard focus, so opening remains an explicit action.
                    return Vec::new();
                };
                if focus.session_id != session_id
                    || focus.attention_subject_node_id.as_ref() != Some(&subject_node_id)
                {
                    return vec![Reaction::Notice(
                        "the daemon returned an inconsistent Attention focus route".into(),
                    )];
                }
                let preview_to_close = self.temporary_pane.as_ref().and_then(|pane| {
                    (pane.binding.session_id == focus.session_id
                        && pane.binding.node_id == subject_node_id
                        && pane.binding.pane_id != focus.pane_id
                        && matches!(pane.capability, NodePaneCapability::PreviewDetails))
                    .then(|| pane.binding.clone())
                });
                let is_temporary = self.temporary_pane.as_ref().is_some_and(|pane| {
                    pane.binding.pane_id == focus.pane_id
                        && pane.binding.session_id == focus.session_id
                });
                if is_temporary {
                    Vec::new()
                } else {
                    let mut reactions = Vec::new();
                    if let Some(preview) = preview_to_close {
                        self.temporary_pane = None;
                        self.pane_owner.remove(&preview.pane_id);
                        self.feeds.remove(&preview.pane_id);
                        self.attaching.remove(&preview.pane_id);
                        reactions.push(Reaction::Send {
                            ask: Ask::Action("closing the semantic Attention preview"),
                            request: Request::ClosePane {
                                session_id: preview.session_id,
                                pane_id: preview.pane_id,
                                disposition: CloseDisposition::KeepProcesses,
                            },
                        });
                    }
                    reactions.push(Reaction::Send {
                        ask: Ask::Action("focusing the runtime pane for Attention"),
                        request: Request::FocusPane {
                            session_id: focus.session_id,
                            target: FocusTarget::Pane {
                                pane_id: focus.pane_id,
                            },
                        },
                    });
                    reactions
                }
            }
            (_, Response::PaneFocus { focus }) => {
                let Some(focus) = focus else {
                    return Vec::new();
                };
                let mut reactions = self.select(focus.session_id.clone());
                let is_temporary = self.temporary_pane.as_ref().is_some_and(|pane| {
                    pane.binding.pane_id == focus.pane_id
                        && pane.binding.session_id == focus.session_id
                });
                if !is_temporary {
                    reactions.push(Reaction::Send {
                        ask: Ask::Action("focusing the Agent pane"),
                        request: Request::FocusPane {
                            session_id: focus.session_id,
                            target: FocusTarget::Pane {
                                pane_id: focus.pane_id,
                            },
                        },
                    });
                }
                reactions
            }
            (_, Response::AttentionList { entries }) => {
                self.queue = entries;
                Vec::new()
            }
            (_, Response::Attention { entry }) => {
                // A peek. Used only to say whether there is anything at all, so the
                // queue itself is left to `attention_queue_changed`.
                if entry.is_none() {
                    self.queue.retain(|item| !item.actionable);
                }
                Vec::new()
            }
            (_, Response::Effects { effects }) => effects
                .into_iter()
                .flat_map(|effect| self.apply_attention_effect(effect))
                .collect(),
            (Ask::Details(session_id), Response::Ack) => {
                // A close or a detach. The session list is refreshed by a push.
                let _ = session_id;
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    fn hierarchy_request(&self) -> Reaction {
        Reaction::Send {
            ask: Ask::Hierarchy,
            request: Request::GetHierarchy {
                surface_id: self.surface_id.clone(),
                include_archived: self.include_archived,
            },
        }
    }

    fn replace_hierarchy(
        &mut self,
        snapshot: HierarchySnapshot,
        allow_equal_revision: bool,
    ) -> Vec<Reaction> {
        if let Some(current) = &self.hierarchy {
            if snapshot.revision < current.revision
                || (!allow_equal_revision && snapshot.revision == current.revision)
            {
                return Vec::new();
            }
        }
        let visible_sessions: HashSet<SessionId> = snapshot
            .workspaces
            .iter()
            .flat_map(|workspace| workspace.sessions.iter())
            .map(|session| session.session.id.clone())
            .collect();
        let hidden_sessions: Vec<SessionId> = self
            .sessions
            .iter()
            .filter(|session| !visible_sessions.contains(&session.id))
            .map(|session| session.id.clone())
            .collect();
        let mut reactions = Vec::new();
        for session_id in hidden_sessions {
            reactions.extend(self.forget_hidden_session_views(&session_id));
        }
        let previous_active = self.selected.clone();
        self.workspaces = snapshot
            .workspaces
            .iter()
            .map(|branch| branch.workspace.clone())
            .collect();
        self.sessions = snapshot
            .workspaces
            .iter()
            .flat_map(|branch| {
                branch
                    .sessions
                    .iter()
                    .map(|session| session.session.clone())
            })
            .collect();
        self.trees = snapshot
            .workspaces
            .iter()
            .flat_map(|branch| {
                branch
                    .sessions
                    .iter()
                    .map(|session| (session.session.id.clone(), session.nodes.clone()))
            })
            .collect();
        let selected_from_tree = snapshot
            .tree_state
            .selected
            .as_ref()
            .and_then(|key| session_for_key(&snapshot, key));
        let active = previous_active
            .filter(|id| self.sessions.iter().any(|session| &session.id == id))
            .or(selected_from_tree)
            .or_else(|| self.sessions.first().map(|session| session.id.clone()));
        self.hierarchy = Some(snapshot);

        let Some(active) = active else {
            self.selected = None;
            return reactions;
        };
        let needs_details = !self.layouts.contains_key(&active);
        self.selected = Some(active.clone());
        if needs_details {
            reactions.push(Reaction::Send {
                ask: Ask::Details(active.clone()),
                request: Request::GetSession { session_id: active },
            });
        } else {
            reactions.extend(self.attach_wanted());
        }
        reactions
    }

    fn apply_event(&mut self, event: turn_proto::ServerEvent, now_ms: i64) -> Vec<Reaction> {
        use turn_proto::ServerEvent as E;
        match event {
            E::PaneScreen {
                pane_id,
                seq,
                update,
                session_id,
                ..
            } => {
                let Some(feed) = self.feeds.get_mut(&pane_id) else {
                    return Vec::new();
                };
                match feed.apply(seq, &update) {
                    Ok(()) => Vec::new(),
                    Err(desync) => {
                        // The window and the daemon disagree about what has been shown.
                        // Asking for the whole screen is the only honest recovery;
                        // applying the update anyway would render a screen neither of
                        // them believes in.
                        tracing::debug!(?desync, pane = %pane_id, "resynchronising a pane");
                        let owner = self
                            .session_of_pane(&pane_id)
                            .unwrap_or_else(|| session_id.clone());
                        vec![Reaction::Send {
                            ask: Ask::Attach {
                                session_id: owner.clone(),
                                pane_id: pane_id.clone(),
                            },
                            request: Request::ResyncPane {
                                session_id: owner,
                                pane_id,
                            },
                        }]
                    }
                }
            }
            E::PaneOutput { .. } | E::PaneOutputGap { .. } => {
                // This window attaches as cells. A byte frame means a second client
                // asked for bytes on a pane we share, which is not ours to render.
                Vec::new()
            }
            E::SessionStateChanged { session } => {
                let session = *session;
                let visible = self.hierarchy.as_ref().is_none_or(|hierarchy| {
                    hierarchy
                        .workspaces
                        .iter()
                        .flat_map(|workspace| workspace.sessions.iter())
                        .any(|branch| branch.session.id == session.id)
                });
                if visible {
                    self.upsert_session(session.clone());
                }
                if let Some(branch) = self.hierarchy.as_mut().and_then(|hierarchy| {
                    hierarchy
                        .workspaces
                        .iter_mut()
                        .flat_map(|workspace| workspace.sessions.iter_mut())
                        .find(|branch| branch.session.id == session.id)
                }) {
                    branch.session = session;
                }
                Vec::new()
            }
            E::SessionRemoved { session_id, .. } => {
                self.relaunching.retain(|node| {
                    self.trees.iter().any(|(owner, nodes)| {
                        owner != &session_id && nodes.iter().any(|row| &row.node_id == node)
                    })
                });
                self.sessions.retain(|summary| summary.id != session_id);
                if let Some(hierarchy) = self.hierarchy.as_mut() {
                    for workspace in &mut hierarchy.workspaces {
                        workspace
                            .sessions
                            .retain(|session| session.session.id != session_id);
                    }
                }
                self.layouts.remove(&session_id);
                self.trees.remove(&session_id);
                self.policies.remove(&session_id);
                self.drop_session_feeds(&session_id);
                let gone: Vec<PaneId> = self
                    .pane_owner
                    .iter()
                    .filter(|(_, owner)| *owner == &session_id)
                    .map(|(pane, _)| pane.clone())
                    .collect();
                for pane in gone {
                    self.pane_owner.remove(&pane);
                    self.feeds.remove(&pane);
                    self.pty_sizes.remove(&pane);
                    self.attaching.remove(&pane);
                }
                if self.selected.as_ref() == Some(&session_id) {
                    self.selected = None;
                    if let Some(next) = self.sessions.first().map(|s| s.id.clone()) {
                        return self.select(next);
                    }
                }
                Vec::new()
            }
            E::LayoutChanged { session_id, layout } => self.apply_layout(&session_id, layout),
            E::TreeChanged { session_id, nodes } => {
                let stale_temporary = self.temporary_pane.as_ref().and_then(|temporary| {
                    (temporary.binding.session_id == session_id
                        && !nodes
                            .iter()
                            .any(|node| node.node_id == temporary.binding.node_id))
                    .then(|| temporary.binding.clone())
                });
                self.trees.insert(session_id.clone(), nodes.clone());
                if let Some(branch) = self.hierarchy.as_mut().and_then(|hierarchy| {
                    hierarchy
                        .workspaces
                        .iter_mut()
                        .flat_map(|workspace| workspace.sessions.iter_mut())
                        .find(|branch| branch.session.id == session_id)
                }) {
                    branch.nodes = nodes;
                }
                if let Some(binding) = stale_temporary {
                    self.temporary_pane = None;
                    self.pane_owner.remove(&binding.pane_id);
                    self.feeds.remove(&binding.pane_id);
                    self.attaching.remove(&binding.pane_id);
                    self.pty_sizes.remove(&binding.pane_id);
                    vec![Reaction::Send {
                        ask: Ask::Action("closing a temporary view whose Process ended"),
                        request: Request::ClosePane {
                            session_id: binding.session_id,
                            pane_id: binding.pane_id,
                            disposition: CloseDisposition::KeepProcesses,
                        },
                    }]
                } else {
                    Vec::new()
                }
            }
            E::HierarchyChanged { snapshot } => self.replace_hierarchy(*snapshot, false),
            E::ActivityPreviewChanged {
                hierarchy_revision,
                session_id,
                node_id,
                preview,
            } => {
                match self.partial_revision(hierarchy_revision) {
                    PartialRevision::Apply => {}
                    PartialRevision::IgnoreStale => return Vec::new(),
                    PartialRevision::Resync => return vec![self.hierarchy_request()],
                }
                self.update_hierarchy_node(&session_id, &node_id, |node| {
                    node.activity_preview = preview;
                });
                Vec::new()
            }
            E::PaneBindingsChanged {
                hierarchy_revision,
                session_id,
                node_id,
                bindings,
            } => {
                match self.partial_revision(hierarchy_revision) {
                    PartialRevision::Apply => {}
                    PartialRevision::IgnoreStale => return Vec::new(),
                    PartialRevision::Resync => return vec![self.hierarchy_request()],
                }
                self.update_hierarchy_node(&session_id, &node_id, |node| {
                    node.pane_bindings = bindings.clone();
                });
                if let Some(temporary) = &self.temporary_pane {
                    let still_open = bindings.iter().any(|binding| {
                        binding.pane_id == temporary.binding.pane_id
                            && binding.session_id == temporary.binding.session_id
                    });
                    if temporary.binding.node_id == node_id && !still_open {
                        let pane_id = temporary.binding.pane_id.clone();
                        self.temporary_pane = None;
                        self.pane_owner.remove(&pane_id);
                        self.feeds.remove(&pane_id);
                        self.attaching.remove(&pane_id);
                        self.pty_sizes.remove(&pane_id);
                    }
                }
                Vec::new()
            }
            E::WorkspaceWriteLeaseChanged {
                hierarchy_revision,
                workspace_id,
                lease,
            } => {
                match self.partial_revision(hierarchy_revision) {
                    PartialRevision::Apply => {}
                    PartialRevision::IgnoreStale => return Vec::new(),
                    PartialRevision::Resync => return vec![self.hierarchy_request()],
                }
                if let Some(branch) = self.hierarchy.as_mut().and_then(|hierarchy| {
                    hierarchy
                        .workspaces
                        .iter_mut()
                        .find(|branch| branch.workspace.id == workspace_id)
                }) {
                    branch.write_lease = lease;
                }
                Vec::new()
            }
            E::AttentionQueueChanged { entries } => {
                self.queue = entries;
                Vec::new()
            }
            E::AttentionEffect { effect } => self.apply_attention_effect(effect),
            E::PtyResized {
                session_id,
                node_id,
                size,
            } => {
                // Another client resized a pty we share. The feed follows so the window
                // draws the right shape rather than the one it asked for.
                let _ = (session_id, node_id);
                for feed in self.feeds.values_mut() {
                    if feed.size() != size {
                        // A resize has no row correspondence, so the screen is refetched
                        // rather than reshaped locally.
                        feed.resync(Grid::blank(size.rows, size.cols), feed.next_seq());
                    }
                }
                Vec::new()
            }
            E::RestoreResult {
                session_id,
                state,
                needs_explanation,
                panes,
            } => {
                if !needs_explanation {
                    self.restores.remove(&session_id);
                    return Vec::new();
                }
                for outcome in &panes {
                    self.feeds.remove(&outcome.pane_id);
                    self.attaching.remove(&outcome.pane_id);
                    self.pty_sizes.remove(&outcome.pane_id);
                }
                // Nothing is started here. The sweep in `autostart_stopped_panes` does it, over
                // every stopped pane the window knows about — this event only fires when the
                // daemon has something to explain, and a Session whose processes had simply ended
                // never produced one, which is how the first attempt at this missed the case it
                // was written for.
                self.restores.insert(
                    session_id.clone(),
                    SessionRestoreView {
                        session_id,
                        state,
                        panes,
                    },
                );
                Vec::new()
            }
            E::NodeStateChanged {
                session_id,
                node_id,
                lifecycle,
                turn,
                display_state,
                ..
            } => {
                self.update_hierarchy_node(&session_id, &node_id, |node| {
                    node.lifecycle = lifecycle.clone();
                    node.turn = turn.clone();
                    node.display_state = display_state;
                    node.state_label = display_state.label().to_string();
                    node.severity = display_state.severity();
                    node.needs_user = display_state.demands_user();
                });
                let _ = now_ms;
                Vec::new()
            }
            E::TurnEventEmitted { .. } => Vec::new(),
        }
    }

    fn partial_revision(&mut self, revision: u64) -> PartialRevision {
        let Some(hierarchy) = self.hierarchy.as_mut() else {
            return PartialRevision::Resync;
        };
        if revision < hierarchy.revision {
            return PartialRevision::IgnoreStale;
        }
        if revision > hierarchy.revision.saturating_add(1) {
            return PartialRevision::Resync;
        }
        hierarchy.revision = revision;
        PartialRevision::Apply
    }

    /// Turns a governor-approved attention focus into three deliberately separate
    /// effects: activate the Session, select the exact tree node, and focus an existing
    /// Pane only if one already exists. It never opens a Pane for a background Agent.
    fn apply_attention_effect(&mut self, effect: Effect) -> Vec<Reaction> {
        let announcement = Announcement::from_effect(&effect);
        let mut reactions = Vec::new();
        if let Effect::Focus {
            session_id,
            node_id,
        } = &effect
        {
            reactions.extend(self.select(session_id.clone()));
            if let Some(node_id) = node_id {
                for key in self.attention_ancestor_keys(session_id, node_id) {
                    reactions.push(Reaction::Send {
                        ask: Ask::Action("revealing the Agent that needs attention"),
                        request: Request::SetTreeExpanded {
                            surface_id: self.surface_id.clone(),
                            key,
                            expanded: true,
                        },
                    });
                }
                reactions.push(Reaction::Send {
                    ask: Ask::Action("selecting the Agent that needs attention"),
                    request: Request::SelectTreeNode {
                        surface_id: self.surface_id.clone(),
                        selected: Some(HierarchyKey::process(node_id.clone())),
                    },
                });
                reactions.push(Reaction::Send {
                    ask: Ask::AttentionFocus {
                        session_id: session_id.clone(),
                        subject_node_id: node_id.clone(),
                    },
                    request: Request::FocusPaneForAttention {
                        surface_id: self.surface_id.clone(),
                        session_id: session_id.clone(),
                        subject_node_id: node_id.clone(),
                    },
                });
            }
        }
        reactions.push(Reaction::Announce(announcement));
        reactions
    }

    fn attention_ancestor_keys(
        &self,
        session_id: &SessionId,
        node_id: &NodeId,
    ) -> Vec<HierarchyKey> {
        let Some((workspace, session)) = self.hierarchy.as_ref().and_then(|hierarchy| {
            hierarchy.workspaces.iter().find_map(|workspace| {
                workspace
                    .sessions
                    .iter()
                    .find(|session| &session.session.id == session_id)
                    .map(|session| (workspace, session))
            })
        }) else {
            return Vec::new();
        };
        let mut keys = vec![
            HierarchyKey::workspace(workspace.workspace.id.clone()),
            HierarchyKey::session(session_id.clone()),
        ];
        let mut parent = session
            .nodes
            .iter()
            .find(|node| &node.node_id == node_id)
            .and_then(|node| node.parent.clone());
        let mut ancestors = Vec::new();
        let mut seen = HashSet::new();
        while let Some(id) = parent {
            if !seen.insert(id.clone()) {
                break;
            }
            ancestors.push(id.clone());
            parent = session
                .nodes
                .iter()
                .find(|node| node.node_id == id)
                .and_then(|node| node.parent.clone());
        }
        ancestors.reverse();
        keys.extend(ancestors.into_iter().map(HierarchyKey::process));
        keys
    }

    /// Starts every stopped pane that says it is safe to start, in every Session the window knows
    /// about. Called after anything arrives from the daemon.
    ///
    /// **This is where the answer belongs, and the first two attempts put it in the wrong place.**
    /// The first added a collective button; the second acted on the restore report — which the
    /// daemon only sends when it has something to *explain*, so a Session whose processes had
    /// simply ended came back to a grid of "Session process stopped / Start pane" and nothing had
    /// changed for the person looking at it. A sweep over what is actually on screen cannot miss a
    /// case the way a single event can.
    ///
    /// What it needs is in the window already: the layout says each pane's `RestoreBehaviour` and
    /// the tree says whether its process has ended. `Relaunch` means running it again is harmless,
    /// which every built-in template sets for its shells, its agent panes and its file browsers.
    /// `ReattachOnly` keeps its button, and is where a pane naming a deploy or a migration belongs.
    ///
    /// Held back in the two cases where starting would be wrong rather than merely eager: a pane
    /// that would use the checkout while a write confirmation is pending, and every pane in a
    /// Session with a process from the previous daemon still alive and out of reach.
    fn autostart_stopped_panes(&mut self) -> Vec<Reaction> {
        let mut out = Vec::new();
        let sessions: Vec<SessionId> = self.layouts.keys().cloned().collect();
        for session_id in sessions {
            if self.has_unreachable_processes(&session_id) {
                continue;
            }
            let held_back = self.write_recovery_pending(&session_id);
            let Some(layout) = self.layouts.get(&session_id) else {
                continue;
            };
            let Some(tree) = self.trees.get(&session_id) else {
                continue;
            };
            let mut wanted: Vec<NodeId> = Vec::new();
            for pane in layout.panes() {
                if pane.restore != turn_core::model::RestoreBehaviour::Relaunch {
                    continue;
                }
                let Some(node_id) = pane.node_id.clone() else {
                    continue;
                };
                if self.relaunching.contains(&node_id) || self.auto_started.contains(&node_id) {
                    continue;
                }
                // Only a pane whose process has actually ended. A running one needs nothing, and
                // a node the window has not heard about yet is not evidence of anything.
                let ended = tree
                    .iter()
                    .any(|node| node.node_id == node_id && node.lifecycle.is_terminal());
                if !ended {
                    continue;
                }
                // An agent, or anything the pane names, would use the checkout; the user's own
                // shell would not, and that shell is what they need in order to go and deal with
                // whatever the confirmation is about.
                if held_back && pane.command.is_some() {
                    continue;
                }
                wanted.push(node_id);
            }
            for node_id in wanted {
                self.relaunching.insert(node_id.clone());
                self.auto_started.insert(node_id.clone());
                tracing::info!(
                    session = %session_id,
                    node = %node_id,
                    "starting a stopped pane the Session came back with"
                );
                out.push(Reaction::Send {
                    ask: Ask::RelaunchNode {
                        session_id: session_id.clone(),
                        node_id: node_id.clone(),
                    },
                    request: Request::RelaunchNode {
                        session_id: session_id.clone(),
                        node_id,
                        resume: false,
                    },
                });
            }
        }
        out
    }

    /// Whether this Session is waiting for the user to confirm write access to the checkout.
    ///
    /// Asked per Session rather than only for the selected one: a restore report arrives for a
    /// Session the window may not be looking at, and starting its agent because the *other*
    /// Session had no pending confirmation would be the wrong answer to the right question.
    fn write_recovery_pending(&self, session_id: &SessionId) -> bool {
        let recovering = self.hierarchy.as_ref().is_some_and(|snapshot| {
            snapshot.workspaces.iter().any(|workspace| {
                workspace.write_lease.as_ref().is_some_and(|lease| {
                    &lease.session_id == session_id && lease.state == LeaseState::RecoveryRequired
                })
            })
        });
        let reclaiming = self.hierarchy.as_ref().is_some_and(|snapshot| {
            snapshot.workspaces.iter().any(|workspace| {
                self.reclaiming_leases.contains(&workspace.workspace.id)
                    && workspace
                        .sessions
                        .iter()
                        .any(|session| &session.session.id == session_id)
            })
        });
        recovering || reclaiming
    }

    /// Whether something from a previous daemon is still alive in this Session and out of reach.
    ///
    /// Nothing is started while one is: a replacement running beside a process Turn cannot
    /// control would do the work twice, and the second copy is the one nobody asked for.
    fn has_unreachable_processes(&self, session_id: &SessionId) -> bool {
        self.trees.get(session_id).is_some_and(|nodes| {
            nodes
                .iter()
                .any(|node| node.lifecycle == turn_core::state::Lifecycle::Orphaned)
        })
    }

    fn update_hierarchy_node(
        &mut self,
        session_id: &SessionId,
        node_id: &NodeId,
        update: impl FnOnce(&mut TreeNodeView),
    ) {
        let Some(node) = self.hierarchy.as_mut().and_then(|hierarchy| {
            hierarchy
                .workspaces
                .iter_mut()
                .flat_map(|workspace| workspace.sessions.iter_mut())
                .find(|branch| &branch.session.id == session_id)
                .and_then(|branch| {
                    branch
                        .nodes
                        .iter_mut()
                        .find(|node| &node.node_id == node_id)
                })
        }) else {
            return;
        };
        update(node);
    }

    fn resolve_restore_outcome(&mut self, session_id: &SessionId, node_id: &NodeId) {
        let mut empty = false;
        if let Some(restore) = self.restores.get_mut(session_id) {
            restore.panes.retain(|pane| &pane.node_id != node_id);
            empty = restore.panes.is_empty();
        }
        if empty {
            self.restores.remove(session_id);
        }
    }

    fn drop_session_feeds(&mut self, session_id: &SessionId) {
        if self
            .temporary_pane
            .as_ref()
            .is_some_and(|pane| &pane.binding.session_id == session_id)
        {
            if let Some(temporary) = self.temporary_pane.take() {
                let pane_id = temporary.binding.pane_id;
                self.pane_owner.remove(&pane_id);
                self.feeds.remove(&pane_id);
                self.attaching.remove(&pane_id);
                self.pty_sizes.remove(&pane_id);
            }
        }
        let panes: Vec<PaneId> = self
            .pane_owner
            .iter()
            .filter(|(_, owner)| *owner == session_id)
            .map(|(pane, _)| pane.clone())
            .collect();
        for pane in panes {
            self.feeds.remove(&pane);
            self.attaching.remove(&pane);
            self.pty_sizes.remove(&pane);
        }
    }

    fn forget_hidden_session_views(&mut self, session_id: &SessionId) -> Vec<Reaction> {
        let temporary = self
            .temporary_pane
            .as_ref()
            .filter(|pane| &pane.binding.session_id == session_id)
            .map(|pane| pane.binding.clone());
        self.drop_session_feeds(session_id);
        self.pane_owner.retain(|_, owner| owner != session_id);
        self.layouts.remove(session_id);
        self.trees.remove(session_id);
        self.policies.remove(session_id);
        temporary
            .map(|binding| {
                vec![Reaction::Send {
                    ask: Ask::Action("closing a temporary view hidden with its Session"),
                    request: Request::ClosePane {
                        session_id: binding.session_id,
                        pane_id: binding.pane_id,
                        disposition: CloseDisposition::KeepProcesses,
                    },
                }]
            })
            .unwrap_or_default()
    }

    /// Which session a pane belongs to.
    ///
    /// Looked up rather than assumed to be the selected one: temporary Panes and late
    /// resyncs retain their owning Session, and the daemon rejects a request addressed
    /// to the wrong one.
    fn session_of_pane(&self, pane: &PaneId) -> Option<SessionId> {
        self.pane_owner.get(pane).cloned()
    }

    fn apply_layout(&mut self, session_id: &SessionId, layout: Layout) -> Vec<Reaction> {
        self.remember_layout(session_id.clone(), layout);
        self.attach_wanted()
    }

    /// Records a session's layout, and forgets the panes that went away.
    ///
    /// A closed pane's screen is dropped rather than held for the life of the window: a
    /// grid is the largest thing the client keeps, and thirty sessions of dead panes
    /// would be a leak that looks like ordinary memory use.
    fn remember_layout(&mut self, session_id: SessionId, layout: Layout) {
        let rebound: Vec<PaneId> = self
            .layouts
            .get(&session_id)
            .map(|previous| {
                layout
                    .panes()
                    .into_iter()
                    .filter(|pane| {
                        previous
                            .get(&pane.id)
                            .is_some_and(|old| old.node_id != pane.node_id)
                    })
                    .map(|pane| pane.id.clone())
                    .collect()
            })
            .unwrap_or_default();
        for pane in rebound {
            // Pane identity is visual; its feed is runtime identity. A relaunch keeps
            // PaneId but changes node_id, so retaining this feed would leave the new
            // process running behind the dead process's final screen.
            self.feeds.remove(&pane);
            self.attaching.remove(&pane);
            self.pty_sizes.remove(&pane);
        }
        let temporary = self
            .temporary_pane
            .as_ref()
            .map(|pane| pane.binding.pane_id.clone());
        self.pane_owner
            .retain(|pane, owner| owner != &session_id || Some(pane) == temporary.as_ref());
        for pane in layout.panes() {
            self.pane_owner.insert(pane.id.clone(), session_id.clone());
        }
        self.layouts.insert(session_id, layout);

        let live: HashSet<PaneId> = self.pane_owner.keys().cloned().collect();
        self.feeds.retain(|id, _| live.contains(id));
        self.pty_sizes.retain(|id, _| live.contains(id));
        self.attaching.retain(|id| live.contains(id));
    }

    /// The panes the window wants a screen for.
    ///
    /// Every terminal Pane in the selected Session plus an explicitly opened temporary
    /// terminal Pane. Background Sessions are supervised through semantic tree state and
    /// Activity Preview; the client never attaches hidden terminals for thumbnails.
    fn wanted_panes(&self) -> Vec<(SessionId, PaneId)> {
        let mut wanted: Vec<(SessionId, PaneId)> = Vec::new();
        if let Some((session_id, layout)) = self
            .selected
            .as_ref()
            .and_then(|id| self.layouts.get(id).map(|layout| (id.clone(), layout)))
        {
            for pane in layout.panes() {
                let has_restore_outcome = self.restores.get(&session_id).is_some_and(|restore| {
                    restore
                        .panes
                        .iter()
                        .any(|outcome| outcome.pane_id == pane.id)
                });
                let runtime_cannot_attach = pane.node_id.as_ref().is_some_and(|node_id| {
                    self.trees.get(&session_id).is_some_and(|nodes| {
                        nodes.iter().any(|node| {
                            &node.node_id == node_id
                                && (node.lifecycle.is_terminal()
                                    || node.lifecycle == Lifecycle::Orphaned)
                        })
                    })
                });
                if pane.kind.is_terminal() && !has_restore_outcome && !runtime_cannot_attach {
                    wanted.push((session_id.clone(), pane.id.clone()));
                }
            }
        }
        if let Some(temporary) = &self.temporary_pane {
            if matches!(temporary.capability, NodePaneCapability::Terminal { .. }) {
                wanted.push((
                    temporary.binding.session_id.clone(),
                    temporary.binding.pane_id.clone(),
                ));
            }
        }
        wanted
    }

    /// Attaches to whatever the window wants a screen for and does not have.
    fn attach_wanted(&mut self) -> Vec<Reaction> {
        let mut reactions = Vec::new();
        for (session_id, pane_id) in self.wanted_panes() {
            if self.feeds.contains_key(&pane_id) || self.attaching.contains(&pane_id) {
                continue;
            }
            self.attaching.insert(pane_id.clone());
            reactions.push(Reaction::Send {
                ask: Ask::Attach {
                    session_id: session_id.clone(),
                    pane_id: pane_id.clone(),
                },
                request: Request::AttachPane {
                    session_id,
                    pane_id: pane_id.clone(),
                    size: self
                        .pty_sizes
                        .get(&pane_id)
                        .copied()
                        .unwrap_or(INITIAL_SIZE),
                    // Cells, always. This window has no VT emulator and does not want
                    // one; the daemon has already parsed the screen.
                    stream: turn_proto::PaneStream::Cells,
                },
            });
        }
        reactions
    }

    fn upsert_session(&mut self, summary: SessionSummary) {
        match self
            .sessions
            .iter_mut()
            .find(|existing| existing.id == summary.id)
        {
            Some(existing) => *existing = summary,
            None => self.sessions.push(summary),
        }
        if self.hierarchy.is_none() {
            self.sort_sessions();
        }
    }

    /// Re-sorts with the daemon's own ranking.
    ///
    /// `SessionSummary::sidebar_rank` exists precisely so a client can do this after a
    /// push without a round trip and without inventing an order of its own.
    fn sort_sessions(&mut self) {
        // Descending by the daemon's own rank, so the session that needs the user comes
        // first.
        self.sessions
            .sort_by_key(|summary| std::cmp::Reverse(summary.sidebar_rank()));
    }

    /// Selects a session and fetches its detail.
    pub fn select(&mut self, session_id: SessionId) -> Vec<Reaction> {
        let select_tree = Reaction::Send {
            ask: Ask::Action("selecting a Session in the workspace tree"),
            request: Request::SelectTreeNode {
                surface_id: self.surface_id.clone(),
                selected: Some(HierarchyKey::session(session_id.clone())),
            },
        };
        if self.selected.as_ref() == Some(&session_id) {
            return vec![select_tree];
        }
        self.selected = Some(session_id.clone());
        // Screens for panes nothing wants any more are dropped, including every Pane
        // in the Session just left unless it is an explicit temporary Pane.
        let wanted: HashSet<PaneId> = self
            .wanted_panes()
            .into_iter()
            .map(|(_, pane)| pane)
            .collect();
        self.feeds.retain(|pane, _| wanted.contains(pane));
        self.attaching.retain(|pane| wanted.contains(pane));

        let mut reactions = vec![
            select_tree,
            Reaction::Send {
                ask: Ask::Details(session_id.clone()),
                request: Request::GetSession { session_id },
            },
        ];
        reactions.extend(self.attach_wanted());
        reactions
    }

    /// Routes typed intents from the unified tree. Selection, Pane focus and opening a
    /// view stay separate protocol operations, so selecting a waiting subagent neither
    /// resolves its Attention nor changes the Layout.
    pub fn apply_hierarchy_action(&mut self, action: HierarchyAction) -> Vec<Reaction> {
        match action {
            HierarchyAction::Select { surface_id, key } => vec![Reaction::Send {
                ask: Ask::Action("selecting a node in the workspace tree"),
                request: Request::SelectTreeNode {
                    surface_id,
                    selected: Some(key),
                },
            }],
            HierarchyAction::SetExpanded {
                surface_id,
                key,
                expanded,
            } => vec![Reaction::Send {
                ask: Ask::Action("changing workspace tree expansion"),
                request: Request::SetTreeExpanded {
                    surface_id,
                    key,
                    expanded,
                },
            }],
            HierarchyAction::QuickPreview {
                session_id,
                node_id,
                ..
            } => vec![Reaction::Send {
                ask: Ask::Preview {
                    session_id: session_id.clone(),
                    node_id: node_id.clone(),
                },
                request: Request::GetPreviewHistory {
                    session_id,
                    node_id,
                    limit: Some(8),
                },
            }],
            HierarchyAction::SetPreviewVisibility {
                session_id,
                node_id,
                visibility,
            } => vec![Reaction::Send {
                ask: Ask::Action(match visibility {
                    PreviewVisibility::Hide => "hiding an activity preview",
                    PreviewVisibility::Inherit | PreviewVisibility::Show => {
                        "showing an activity preview"
                    }
                }),
                request: Request::SetPreviewVisibility {
                    session_id,
                    node_id,
                    visibility,
                },
            }],
            HierarchyAction::OpenTemporaryPane {
                surface_id,
                session_id,
                node_id,
            } => vec![Reaction::Send {
                ask: Ask::NodePane,
                request: Request::OpenNodeAsTemporaryPane {
                    surface_id,
                    session_id,
                    node_id,
                },
            }],
            HierarchyAction::FocusPaneForNode {
                surface_id,
                session_id,
                node_id,
            } => vec![Reaction::Send {
                ask: Ask::Action("focusing an existing Pane for a tree node"),
                request: Request::FocusPaneForNode {
                    surface_id,
                    session_id,
                    node_id,
                },
            }],
        }
    }

    /// Applies one of the daemon-advertised recovery choices for an exclusive
    /// primary-checkout conflict. Nothing is retried until this explicit call.
    pub fn resolve_write_conflict(
        &mut self,
        alternative: SessionConflictAlternative,
        now_ms: i64,
    ) -> Vec<Reaction> {
        let Some(ProtoErrorContext::WorkspaceWriteLeaseConflict {
            workspace_id,
            owner,
            alternatives,
            ..
        }) = self.write_conflict.clone()
        else {
            return Vec::new();
        };
        if !alternatives.contains(&alternative) {
            return vec![Reaction::Notice(
                "that recovery choice is not available for this checkout".into(),
            )];
        }

        match alternative {
            SessionConflictAlternative::Cancel => {
                self.write_conflict = None;
                self.pending_session = None;
                vec![Reaction::SessionCreationCancelled]
            }
            SessionConflictAlternative::FocusOwner => {
                self.write_conflict = None;
                self.pending_session = None;
                let mut reactions = self.select(owner.session_id);
                reactions.push(Reaction::SessionCreationCancelled);
                reactions
            }
            SessionConflictAlternative::CreateReadOnly => {
                let draft = self.pending_session.clone();
                self.write_conflict = None;
                vec![Reaction::Send {
                    ask: Ask::CreateSession {
                        workspace_id: workspace_id.clone(),
                    },
                    request: match draft {
                        Some(draft) => Request::CreateReadOnlySessionFromTemplate {
                            workspace_id: draft.workspace_id,
                            template_id: draft.template_id,
                            name: draft.name,
                            cwd: draft.cwd,
                            branch: draft.branch,
                            task: draft.task,
                        },
                        None => Request::CreateReadOnlySession {
                            workspace_id,
                            name: "Read-only review".into(),
                            cwd: None,
                            panes: None,
                            note: None,
                            tags: Vec::new(),
                        },
                    },
                }]
            }
            SessionConflictAlternative::CreateIsolatedWorktree => {
                let draft = self.pending_session.clone();
                self.write_conflict = None;
                vec![Reaction::Send {
                    ask: Ask::CreateSession {
                        workspace_id: workspace_id.clone(),
                    },
                    request: match draft {
                        Some(draft) => {
                            let branch_seed = draft
                                .name
                                .as_deref()
                                .or(draft.task.as_deref())
                                .unwrap_or("isolated-session")
                                .to_string();
                            let branch = isolated_branch_name(&branch_seed, now_ms);
                            Request::CreateWorktreeSessionFromTemplate {
                                workspace_id: draft.workspace_id,
                                template_id: draft.template_id,
                                name: draft.name,
                                cwd: draft.cwd,
                                template_branch: draft.branch,
                                task: draft.task,
                                branch,
                                worktree_path: None,
                            }
                        }
                        None => Request::CreateWorktreeSession {
                            workspace_id,
                            name: "Isolated worktree".into(),
                            branch: isolated_branch_name("Isolated worktree", now_ms),
                            worktree_path: None,
                            panes: None,
                            note: None,
                            tags: Vec::new(),
                        },
                    },
                }]
            }
        }
    }

    /// Runs a command.
    ///
    /// Everything that changes the world is one request, and every one of them is
    /// something the user asked for by name.
    pub fn dispatch(&mut self, command: Command, now_ms: i64) -> Vec<Reaction> {
        let session = self.selected.clone();
        let pane = self.active_pane();
        match command {
            Command::NextAttention => vec![Reaction::Send {
                ask: Ask::Action("going to the next demand"),
                // No id: the daemon picks, because it owns the order. Pressing the
                // shortcut is consent, so this bypasses the focus governor's guards.
                request: Request::GotoAttention { attention_id: None },
            }],
            Command::NextSession | Command::PreviousSession => {
                let Some(current) = session else {
                    return Vec::new();
                };
                let Some(at) = self.sessions.iter().position(|s| s.id == current) else {
                    return Vec::new();
                };
                if self.sessions.is_empty() {
                    return Vec::new();
                }
                let step: i64 = if command == Command::NextSession {
                    1
                } else {
                    -1
                };
                let next = (at as i64 + step).rem_euclid(self.sessions.len() as i64) as usize;
                match self.sessions.get(next).map(|s| s.id.clone()) {
                    Some(id) => self.select(id),
                    None => Vec::new(),
                }
            }
            Command::QuickNewSession if self.creation_in_progress() => vec![Reaction::Notice(
                "finish the Workspace or Session creation already in progress".into(),
            )],
            Command::QuickNewSession => match self.current_workspace() {
                Some(workspace_id) => {
                    let Some(template_id) = self
                        .preferred_template(&workspace_id)
                        .map(|template| template.id.clone())
                    else {
                        return vec![Reaction::Notice("no template to start from yet".into())];
                    };
                    let session_number = self
                        .sessions
                        .iter()
                        .filter(|session| session.workspace_id == workspace_id)
                        .count()
                        + 1;
                    let draft = PendingSessionDraft {
                        workspace_id: workspace_id.clone(),
                        template_id: template_id.clone(),
                        name: Some(format!("Session {session_number}")),
                        cwd: None,
                        branch: None,
                        task: None,
                    };
                    self.pending_session = Some(draft.clone());
                    vec![Reaction::Send {
                        ask: Ask::CreateSession {
                            workspace_id: workspace_id.clone(),
                        },
                        request: Request::CreateSessionFromTemplate {
                            workspace_id,
                            template_id,
                            name: draft.name,
                            cwd: None,
                            branch: None,
                            task: None,
                        },
                    }]
                }
                None => vec![Reaction::Notice(
                    "create a workspace before using Quick New".into(),
                )],
            },
            // These open window-local sheets in `TurnApp`; the Desk never invents form
            // values or silently picks a Template for the non-quick path.
            Command::NewWorkspace | Command::NewSession => Vec::new(),
            Command::SaveLayoutAsTemplate => match session {
                Some(session_id) => vec![Reaction::Send {
                    ask: Ask::Action("saving the layout as a template"),
                    request: Request::SaveLayoutAsTemplate {
                        session_id,
                        name: format!("Layout {}", self.templates.len() + 1),
                        description: None,
                        hotkey: None,
                    },
                }],
                None => Vec::new(),
            },
            Command::RenameSession => match (session, self.selected_summary()) {
                (Some(session_id), Some(summary)) => {
                    let name = format!("{} (renamed)", summary.name);
                    vec![Reaction::Send {
                        ask: Ask::Action("renaming the session"),
                        request: Request::RenameSession { session_id, name },
                    }]
                }
                _ => Vec::new(),
            },
            // Archiving acts on the row the user is looking at, which is the tree's
            // selection — not necessarily the Session whose panes are on screen.
            Command::ArchiveSession => {
                let Some(summary) = self.target_session() else {
                    return vec![Reaction::Notice(
                        "select a Session before archiving it".into(),
                    )];
                };
                let session_id = summary.id.clone();
                if summary.status == SessionStatus::Archived {
                    return vec![Reaction::Notice("this Session is already archived".into())];
                }
                if summary.running_count > 0 {
                    return vec![Reaction::Notice(
                        "end the Session before archiving it".into(),
                    )];
                }
                let owns_lease = self.hierarchy.as_ref().is_some_and(|snapshot| {
                    snapshot.workspaces.iter().any(|workspace| {
                        workspace
                            .write_lease
                            .as_ref()
                            .is_some_and(|lease| lease.session_id == session_id)
                    })
                });
                if owns_lease {
                    return vec![Reaction::Notice(
                        "release the Session's write lease before archiving it".into(),
                    )];
                }
                vec![Reaction::Send {
                    ask: Ask::ArchiveSession { archived: true },
                    request: Request::ArchiveSession {
                        session_id,
                        archived: true,
                    },
                }]
            }
            Command::ArchiveWorkspace => {
                let Some(branch) = self.target_workspace_branch() else {
                    return vec![Reaction::Notice(
                        "select a Workspace before archiving it".into(),
                    )];
                };
                let workspace_id = branch.workspace.id.clone();
                if branch.workspace.archived {
                    return vec![Reaction::Notice(
                        "this Workspace is already archived".into(),
                    )];
                }
                let running: usize = branch
                    .sessions
                    .iter()
                    .map(|session| session.session.running_count)
                    .sum();
                if running > 0 {
                    return vec![Reaction::Notice(
                        "end the Workspace's Sessions before archiving it".into(),
                    )];
                }
                if branch.write_lease.is_some() {
                    return vec![Reaction::Notice(
                        "release the Workspace's write lease before archiving it".into(),
                    )];
                }
                vec![Reaction::Send {
                    ask: Ask::ArchiveWorkspace { archived: true },
                    request: Request::ArchiveWorkspace {
                        workspace_id,
                        archived: true,
                    },
                }]
            }
            // Nothing here. Both of these stop processes, and the only thing allowed to
            // do that is an accepted confirmation, which the window opens from
            // [`Desk::end_session_confirmation`] and [`Desk::stop_workspace_confirmation`]
            // and delivers back as a [`ViewAction`]. A command that closed on the
            // keystroke would be a process stopped by surprise.
            // None of the four act on their own: each opens a question, and the dialog is the
            // only thing that can answer it.
            Command::CloseSession
            | Command::CloseWorkspace
            | Command::DeleteSession
            | Command::DeleteWorkspace => Vec::new(),
            Command::SplitHorizontal | Command::SplitVertical => {
                // A split opens a shell with no command in it, so it needs no write
                // authority; only an archived Session refuses it.
                if self.selected_launch_blocked(false) {
                    return vec![Reaction::Notice(
                        "restore this Session before starting another pane".into(),
                    )];
                }
                let Some((session_id, pane_id)) = session.zip(pane) else {
                    return Vec::new();
                };
                let direction = if command == Command::SplitHorizontal {
                    Direction::Horizontal
                } else {
                    Direction::Vertical
                };
                vec![Reaction::Send {
                    ask: Ask::Action("splitting the pane"),
                    request: Request::SplitPane {
                        session_id,
                        pane_id,
                        direction,
                        pane: NewPane::new(PaneKind::Shell),
                    },
                }]
            }
            Command::ClosePane => {
                let Some((session_id, pane_id)) = session.zip(pane) else {
                    return Vec::new();
                };
                vec![Reaction::Send {
                    ask: Ask::Action("closing the pane"),
                    request: Request::ClosePane {
                        session_id,
                        pane_id,
                        disposition: CloseDisposition::KeepProcesses,
                    },
                }]
            }
            Command::ZoomPane => {
                let Some((session_id, pane_id)) = session.zip(pane) else {
                    return Vec::new();
                };
                vec![Reaction::Send {
                    ask: Ask::Action("zooming the pane"),
                    request: Request::ZoomPane {
                        session_id,
                        pane_id,
                    },
                }]
            }
            Command::CyclePane | Command::CyclePaneBack => {
                let Some(session_id) = session else {
                    return Vec::new();
                };
                let target = if command == Command::CyclePane {
                    FocusTarget::Next
                } else {
                    FocusTarget::Previous
                };
                vec![Reaction::Send {
                    ask: Ask::Action("moving between panes"),
                    request: Request::FocusPane { session_id, target },
                }]
            }
            Command::FocusPaneLeft
            | Command::FocusPaneRight
            | Command::FocusPaneUp
            | Command::FocusPaneDown => {
                let Some((session_id, from)) = session.zip(pane) else {
                    return Vec::new();
                };
                // Geometric: "the pane to the left" is a question about rectangles, and
                // the answer comes from the arrangement that was actually drawn.
                match crate::view::neighbour_for(&self.arrangement, &from, command) {
                    Some(pane_id) => vec![Reaction::Send {
                        ask: Ask::Action("moving between panes"),
                        request: Request::FocusPane {
                            session_id,
                            target: FocusTarget::Pane { pane_id },
                        },
                    }],
                    // Nothing on that side. Doing nothing is right: wrapping would send
                    // the user to the far side of the window for pressing an arrow at
                    // the edge.
                    None => Vec::new(),
                }
            }
            Command::MovePaneLeft
            | Command::MovePaneRight
            | Command::MovePaneUp
            | Command::MovePaneDown => {
                let Some((session_id, from)) = session.zip(pane) else {
                    return Vec::new();
                };
                // The same request the drag sends, with the target and the zone worked out
                // from the same geometry the directional focus commands use — so the
                // keyboard is the accessible path to the same rearrangement rather than a
                // degraded alias for exchanging two panes. See
                // [`crate::panes::relocation`] for what "there is nothing on that side"
                // means: the pane goes to the outer edge of the layout, and only a pane
                // that already spans that edge alone has nowhere to go. The daemon does
                // the move; the window never rearranges its own copy of the layout, or the
                // two would disagree the moment a request failed.
                match crate::view::relocation_for(&self.arrangement, &from, command) {
                    Some((target, zone)) => vec![Reaction::Send {
                        ask: Ask::Action("moving the pane"),
                        request: Request::RelocatePane {
                            session_id,
                            moved: from,
                            target,
                            zone,
                        },
                    }],
                    None => vec![Reaction::Notice(
                        "this pane already spans that edge of the layout".into(),
                    )],
                }
            }
            Command::LaunchAgent | Command::LaunchShell | Command::LaunchTui => {
                let kind = match command {
                    Command::LaunchAgent => PaneKind::Agent,
                    Command::LaunchTui => PaneKind::Tui,
                    _ => PaneKind::Shell,
                };
                // An agent, or a tool the pane will name, runs against the checkout. A
                // plain shell does not, so it starts while write access is still pending.
                if self.selected_launch_blocked(kind != PaneKind::Shell) {
                    return vec![Reaction::Notice(if kind == PaneKind::Shell {
                        "restore this Session before starting another process".into()
                    } else {
                        "confirm main-checkout write access before starting an agent or a tool"
                            .to_string()
                    })];
                }
                let Some((session_id, pane_id)) = session.zip(pane) else {
                    return Vec::new();
                };
                // A split with something in it, rather than a "run this" verb: a process
                // starts from a pane definition the user chose.
                vec![Reaction::Send {
                    ask: Ask::Action("launching a process"),
                    request: Request::SplitPane {
                        session_id,
                        pane_id,
                        direction: Direction::Horizontal,
                        pane: NewPane::new(kind),
                    },
                }]
            }
            Command::InterruptProcess | Command::StopProcess => {
                let Some((session_id, pane_id)) = session.zip(pane) else {
                    return Vec::new();
                };
                let Some(node_id) = self.node_of(&pane_id) else {
                    return vec![Reaction::Notice("no process in this pane".into())];
                };
                if self.node_is_orphaned(&session_id, &node_id) {
                    return vec![Reaction::Notice(
                        "this process survived the previous daemon and is not controllable; stop it outside Turn, then confirm recovery"
                            .into(),
                    )];
                }
                let request = if command == Command::InterruptProcess {
                    Request::InterruptNode {
                        session_id,
                        node_id,
                    }
                } else {
                    Request::TerminateNode {
                        session_id,
                        node_id,
                    }
                };
                vec![Reaction::Send {
                    ask: Ask::Action("signalling the process"),
                    request,
                }]
            }
            // Handled by the window rather than the daemon.
            Command::OpenPalette
            | Command::ShowKeyboardShortcuts
            | Command::OpenSettings
            | Command::SwitchSession
            | Command::ToggleAttentionPanel
            | Command::FocusWorkspaceTree
            | Command::PassContext
            | Command::CopySelection
            | Command::PasteClipboard => {
                let _ = now_ms;
                Vec::new()
            }
        }
    }

    /// Applies something the user did in the window.
    pub fn apply_view_action(&mut self, action: ViewAction, now_ms: i64) -> Vec<Reaction> {
        match action {
            ViewAction::SelectSession(id) => self.select(id),
            ViewAction::Run(command) => self.dispatch(command, now_ms),
            ViewAction::GotoAttention(attention_id) => vec![Reaction::Send {
                ask: Ask::Action("going to a demand"),
                // The id of the demand that was shown, not whichever the queue ranks
                // first now: a banner that acted on a different demand from the one it
                // displayed would send the user somewhere they did not choose.
                request: Request::GotoAttention {
                    attention_id: Some(attention_id),
                },
            }],
            ViewAction::DismissAttention(attention_id) => vec![Reaction::Send {
                ask: Ask::Action("dismissing a demand"),
                request: Request::DismissAttention { attention_id },
            }],
            ViewAction::SnoozeAttention {
                attention_id,
                until_ms,
            } => vec![Reaction::Send {
                ask: Ask::Action("snoozing a demand"),
                request: Request::SnoozeAttention {
                    attention_id,
                    until_ms,
                },
            }],
            ViewAction::MuteAttentionSession {
                session_id,
                until_ms,
            } => vec![Reaction::Send {
                ask: Ask::Action("muting a session"),
                request: Request::MuteSession {
                    session_id,
                    until_ms,
                },
            }],
            ViewAction::TerminateNode {
                session_id,
                node_id,
            } => {
                if self.node_is_orphaned(&session_id, &node_id) {
                    return vec![Reaction::Notice(
                        "this process survived the previous daemon and is not controllable; stop it outside Turn, then confirm recovery"
                            .into(),
                    )];
                }
                vec![Reaction::Send {
                    ask: Ask::Action("stopping an Agent or Process"),
                    request: Request::TerminateNode {
                        session_id,
                        node_id,
                    },
                }]
            }
            ViewAction::CloseSession {
                session_id,
                disposition,
            } => vec![Reaction::Send {
                ask: Ask::CloseSession {
                    session_id: session_id.clone(),
                    disposition,
                },
                request: Request::CloseSession {
                    session_id,
                    disposition,
                },
            }],
            ViewAction::CloseWorkspace {
                workspace_id,
                disposition,
            } => vec![Reaction::Send {
                ask: Ask::CloseWorkspace {
                    workspace_id: workspace_id.clone(),
                    disposition,
                },
                request: Request::CloseWorkspace {
                    workspace_id,
                    disposition,
                },
            }],
            ViewAction::ZoomPane {
                session_id,
                pane_id,
            } => vec![Reaction::Send {
                ask: Ask::Action("showing the pane"),
                request: Request::ZoomPane {
                    session_id,
                    pane_id,
                },
            }],
            ViewAction::DeleteSession {
                session_id,
                disposition,
            } => vec![Reaction::Send {
                ask: Ask::DeleteSession {
                    session_id: session_id.clone(),
                },
                request: Request::DeleteSession {
                    session_id,
                    disposition,
                },
            }],
            ViewAction::DeleteWorkspace {
                workspace_id,
                disposition,
            } => vec![Reaction::Send {
                ask: Ask::DeleteWorkspace {
                    workspace_id: workspace_id.clone(),
                },
                request: Request::DeleteWorkspace {
                    workspace_id,
                    disposition,
                },
            }],
            ViewAction::RelaunchNode {
                session_id,
                node_id,
                resume,
            } => {
                if self.session_launch_blocked(
                    &session_id,
                    self.node_needs_checkout_write(&session_id, &node_id),
                ) {
                    return vec![Reaction::Notice(
                        "restore this Session and confirm write access before starting the pane"
                            .into(),
                    )];
                }
                if !self.relaunching.insert(node_id.clone()) {
                    return Vec::new();
                }
                vec![Reaction::Send {
                    ask: Ask::RelaunchNode {
                        session_id: session_id.clone(),
                        node_id: node_id.clone(),
                    },
                    request: Request::RelaunchNode {
                        session_id,
                        node_id,
                        resume,
                    },
                }]
            }
            ViewAction::SetArchivedVisibility { include } => {
                self.include_archived = include;
                vec![self.hierarchy_request()]
            }
            ViewAction::ArchiveSession {
                session_id,
                archived,
            } => {
                if !archived {
                    let workspace_archived = self.hierarchy.as_ref().is_some_and(|snapshot| {
                        snapshot.workspaces.iter().any(|workspace| {
                            workspace.workspace.archived
                                && workspace
                                    .sessions
                                    .iter()
                                    .any(|session| session.session.id == session_id)
                        })
                    });
                    if workspace_archived {
                        return vec![Reaction::Notice(
                            "restore the Workspace before restoring this Session".into(),
                        )];
                    }
                }
                vec![Reaction::Send {
                    // Nothing is stopped here, and nothing may be: archiving is the
                    // reversible half of this pair, and the request carries no
                    // disposition to argue about.
                    ask: Ask::ArchiveSession { archived },
                    request: Request::ArchiveSession {
                        session_id,
                        archived,
                    },
                }]
            }
            ViewAction::ArchiveWorkspace {
                workspace_id,
                archived,
            } => vec![Reaction::Send {
                ask: Ask::ArchiveWorkspace { archived },
                request: Request::ArchiveWorkspace {
                    workspace_id,
                    archived,
                },
            }],
            ViewAction::ReclaimWorkspaceWriteLease {
                workspace_id,
                session_id,
                checkout_id,
            } => {
                if !self.reclaiming_leases.insert(workspace_id.clone()) {
                    return Vec::new();
                }
                vec![Reaction::Send {
                    ask: Ask::RestoreLeaseAcquire {
                        workspace_id: workspace_id.clone(),
                        session_id: session_id.clone(),
                        checkout_id: checkout_id.clone(),
                    },
                    request: Request::AcquireWorkspaceWriteLease {
                        workspace_id,
                        session_id,
                        checkout_id,
                    },
                }]
            }
            ViewAction::PrepareContextHandoff {
                session_id,
                source_node_id,
                target_node_id,
                instruction,
            } => vec![Reaction::Send {
                ask: Ask::PrepareContextHandoff {
                    session_id: session_id.clone(),
                    source_node_id: source_node_id.clone(),
                    target_node_id: target_node_id.clone(),
                },
                request: Request::PrepareContextHandoff {
                    session_id,
                    source_node_id,
                    target_node_id,
                    instruction: instruction.map(ContextHandoffText::new),
                },
            }],
            ViewAction::DeliverContextHandoff {
                session_id,
                handoff_id,
            } => vec![Reaction::Send {
                ask: Ask::DeliverContextHandoff {
                    session_id: session_id.clone(),
                    handoff_id: handoff_id.clone(),
                },
                request: Request::DeliverContextHandoff {
                    session_id,
                    handoff_id,
                },
            }],
            ViewAction::CreateWorkspace {
                name,
                root,
                continue_to_session,
            } => {
                if self.creation_in_progress() {
                    let message =
                        "finish the Workspace or Session creation already in progress".to_string();
                    return vec![Reaction::Notice(message)];
                }
                self.notice = None;
                self.pending_workspace_creation = true;
                vec![Reaction::Send {
                    ask: Ask::CreateWorkspace {
                        continue_to_session,
                    },
                    request: Request::CreateWorkspace { name, root },
                }]
            }
            ViewAction::CreateSessionFromTemplate {
                workspace_id,
                template_id,
                name,
                task,
            } => {
                if self.creation_in_progress() {
                    let message =
                        "finish the Workspace or Session creation already in progress".to_string();
                    return vec![Reaction::Notice(message)];
                }
                self.notice = None;
                let name = (!name.trim().is_empty()).then(|| name.trim().to_string());
                let draft = PendingSessionDraft {
                    workspace_id: workspace_id.clone(),
                    template_id: template_id.clone(),
                    name: name.clone(),
                    cwd: None,
                    branch: None,
                    task: task.clone(),
                };
                self.pending_session = Some(draft);
                vec![Reaction::Send {
                    ask: Ask::CreateSession {
                        workspace_id: workspace_id.clone(),
                    },
                    request: Request::CreateSessionFromTemplate {
                        workspace_id,
                        template_id,
                        name,
                        cwd: None,
                        branch: None,
                        task,
                    },
                }]
            }
            ViewAction::CreateLayoutTemplate { name, layout } => vec![Reaction::Send {
                ask: Ask::CreateTemplate,
                request: Request::CreateLayoutTemplate {
                    name,
                    layout: Box::new(layout),
                    description: Some("Created in Turn's visual layout editor".into()),
                },
            }],
            ViewAction::ReleaseWorkspaceLease {
                workspace_id,
                lease_id,
                expected_generation,
            } => vec![Reaction::Send {
                ask: Ask::Action("releasing the workspace write lease"),
                request: Request::ReleaseWorkspaceWriteLease {
                    workspace_id,
                    lease_id,
                    expected_generation,
                },
            }],
            ViewAction::ResolveWriteConflict(alternative) => {
                self.resolve_write_conflict(alternative, now_ms)
            }
            ViewAction::CloseTemporaryPane {
                session_id,
                pane_id,
            } => {
                if self.temporary_pane.as_ref().is_some_and(|temporary| {
                    temporary.binding.session_id == session_id
                        && temporary.binding.pane_id == pane_id
                }) {
                    self.temporary_pane = None;
                    self.pane_owner.remove(&pane_id);
                    self.feeds.remove(&pane_id);
                    self.attaching.remove(&pane_id);
                }
                vec![Reaction::Send {
                    ask: Ask::Action("closing a temporary Agent view"),
                    request: Request::ClosePane {
                        session_id,
                        pane_id,
                        disposition: CloseDisposition::KeepProcesses,
                    },
                }]
            }
            ViewAction::ResizeDivider {
                before,
                after,
                fraction,
            } => match self.selected.clone() {
                Some(session_id) => vec![Reaction::Send {
                    ask: Ask::Action("resizing the pane"),
                    request: Request::ResizeDivider {
                        session_id,
                        before,
                        after,
                        delta: fraction,
                    },
                }],
                None => Vec::new(),
            },
            ViewAction::EqualizeDivider { before, after } => match self.selected.clone() {
                Some(session_id) => vec![Reaction::Send {
                    ask: Ask::Action("balancing the panes"),
                    request: Request::EqualizeDivider {
                        session_id,
                        before,
                        after,
                    },
                }],
                None => Vec::new(),
            },
            ViewAction::ApplyLayoutPreset(preset) => match self.selected.clone() {
                Some(session_id) => vec![Reaction::Send {
                    ask: Ask::Action("rearranging the layout"),
                    request: Request::ApplyLayoutPreset { session_id, preset },
                }],
                None => Vec::new(),
            },
            ViewAction::ClosePane { pane_id } => {
                let Some(session_id) = self
                    .session_of_pane(&pane_id)
                    .or_else(|| self.selected.clone())
                else {
                    return Vec::new();
                };
                // `KeepProcesses`, always. Closing a view is not stopping what it was
                // showing, and the header control that produces this says so.
                vec![Reaction::Send {
                    ask: Ask::Action("closing the pane"),
                    request: Request::ClosePane {
                        session_id,
                        pane_id,
                        disposition: CloseDisposition::KeepProcesses,
                    },
                }]
            }
            ViewAction::RelocatePane {
                moved,
                target,
                zone,
            } => {
                // A pane dropped on itself is not a move, and must not cost a round trip.
                if moved == target {
                    return Vec::new();
                }
                let Some(session_id) = self
                    .session_of_pane(&moved)
                    .or_else(|| self.selected.clone())
                else {
                    return Vec::new();
                };
                vec![Reaction::Send {
                    ask: Ask::Action("moving the pane"),
                    request: Request::RelocatePane {
                        session_id,
                        moved,
                        target,
                        zone,
                    },
                }]
            }
            ViewAction::ChooseWorkspaceDirectory
            | ViewAction::OpenLayoutEditor(_)
            | ViewAction::CloseLayoutEditor
            | ViewAction::CloseOverlay => Vec::new(),
            ViewAction::Pane { pane_id, action } => self.apply_pane_action(pane_id, action),
        }
    }

    fn apply_pane_action(&mut self, pane_id: PaneId, action: PaneAction) -> Vec<Reaction> {
        let Some(session_id) = self
            .session_of_pane(&pane_id)
            .or_else(|| self.selected.clone())
        else {
            return Vec::new();
        };
        match action {
            PaneAction::Write(data) => {
                // Any input returns the viewport to the live screen, which is what every
                // terminal does: typing into history would be baffling.
                if let Some(feed) = self.feeds.get_mut(&pane_id) {
                    feed.scroll_to_bottom();
                }
                let Some(node_id) = self.node_of(&pane_id) else {
                    return Vec::new();
                };
                // The direct-input path to a pty. There is no approve request:
                // answering a pending agent interaction is the human typing. Context
                // handoff is a distinct reviewed operation for an idle/done Agent.
                vec![Reaction::Send {
                    ask: Ask::Stream,
                    request: Request::WritePty {
                        session_id,
                        node_id,
                        data: TerminalBytes::new(data),
                    },
                }]
            }
            PaneAction::Resize(size) => {
                if self.pty_sizes.get(&pane_id) == Some(&size) {
                    return Vec::new();
                }
                self.pty_sizes.insert(pane_id.clone(), size);
                let Some(node_id) = self.node_of(&pane_id) else {
                    return Vec::new();
                };
                vec![Reaction::Send {
                    ask: Ask::Stream,
                    request: Request::ResizePty {
                        session_id,
                        node_id,
                        size,
                    },
                }]
            }
            PaneAction::Focus => vec![Reaction::Send {
                ask: Ask::Action("focusing the pane"),
                request: Request::FocusPane {
                    session_id,
                    target: FocusTarget::Pane { pane_id },
                },
            }],
            PaneAction::Copy(text) => vec![Reaction::Copy(text)],
            PaneAction::Scroll(rows) => {
                if let Some(feed) = self.feeds.get_mut(&pane_id) {
                    feed.scroll_by(rows);
                }
                Vec::new()
            }
        }
    }

    /// Records the arrangement that was drawn, for directional navigation.
    pub fn remember_arrangement(&mut self, arrangement: Arrangement) {
        self.arrangement = arrangement;
    }

    /// The arrangement of the selected session in an area, for the caller that draws.
    pub fn arrange(&self, area: egui::Rect) -> Arrangement {
        match self.layout() {
            Some(layout) => panes::arrange(layout, area),
            None => Arrangement::default(),
        }
    }

    /// Builds the screens for the panes about to be drawn.
    ///
    /// Separate from [`Desk::view`] because a grid is built behind a mutable borrow and
    /// the view holds shared ones. Doing it in two steps is what lets the view borrow the
    /// screens rather than clone them.
    pub fn refresh_screens(&mut self) {
        for feed in self.feeds.values_mut() {
            let _ = feed.grid();
        }
    }

    /// What the window should draw.
    ///
    /// Call [`Desk::refresh_screens`] first.
    pub fn view(&self, now_ms: i64) -> TurnView<'_> {
        let restore = self
            .selected
            .as_ref()
            .and_then(|session_id| self.restores.get(session_id));
        let recovery_lease = self.selected.as_ref().and_then(|selected| {
            self.hierarchy.as_ref().and_then(|snapshot| {
                snapshot.workspaces.iter().find_map(|workspace| {
                    workspace.write_lease.as_ref().filter(|lease| {
                        &lease.session_id == selected && lease.state == LeaseState::RecoveryRequired
                    })
                })
            })
        });
        let reclaiming_write_access = self.selected.as_ref().is_some_and(|selected| {
            self.hierarchy.as_ref().is_some_and(|snapshot| {
                snapshot.workspaces.iter().any(|workspace| {
                    self.reclaiming_leases.contains(&workspace.workspace.id)
                        && workspace
                            .sessions
                            .iter()
                            .any(|session| &session.session.id == selected)
                })
            })
        });
        let unreachable_processes = self
            .selected
            .as_ref()
            .and_then(|selected| self.trees.get(selected))
            .map(|nodes| {
                nodes
                    .iter()
                    .filter(|node| node.lifecycle == turn_core::state::Lifecycle::Orphaned)
                    .count()
            })
            .unwrap_or(0);
        let sessions: Vec<SessionRow> = self
            .sessions
            .iter()
            .map(|summary| SessionRow {
                id: summary.id.clone(),
                name: summary.name.clone(),
                state: summary.display_state,
                // The daemon's own wording, never re-derived here.
                state_label: summary.state_label.clone(),
                detail: describe(summary),
                badge: summary.badge_count,
                provisional: summary
                    .primary_agent
                    .as_ref()
                    .is_some_and(|agent| agent.pending_permission.is_none())
                    && summary.display_state == turn_core::state::DisplayState::Unknown,
                depth: 0,
                muted: summary.muted,
            })
            .collect();

        let panes: Vec<PaneContent<'_>> = match self.layout() {
            None => Vec::new(),
            Some(layout) => layout
                .panes()
                .into_iter()
                .filter_map(|pane| {
                    let feed = self.feeds.get(&pane.id)?;
                    let grid = feed.peek()?;
                    Some(PaneContent {
                        pane_id: pane.id.clone(),
                        title: pane
                            .title
                            .clone()
                            .or_else(|| pane.command.clone())
                            .unwrap_or_else(|| format!("{:?}", pane.kind).to_lowercase()),
                        grid,
                        focused: layout.active.as_ref() == Some(&pane.id),
                        scrolled: feed.offset() > 0,
                        history_complete: feed.history_complete(),
                    })
                })
                .collect(),
        };

        let temporary_pane = self.temporary_pane.as_ref().map(|pane| {
            let node = self.hierarchy.as_ref().and_then(|hierarchy| {
                hierarchy
                    .workspaces
                    .iter()
                    .flat_map(|workspace| &workspace.sessions)
                    .flat_map(|session| &session.nodes)
                    .find(|node| node.node_id == pane.binding.node_id)
            });
            TemporaryPaneContent {
                pane,
                node,
                previews: self.preview_history(&pane.binding.node_id),
                grid: self
                    .feeds
                    .get(&pane.binding.pane_id)
                    .and_then(PaneFeed::peek),
            }
        });

        TurnView {
            workspaces: &self.workspaces,
            templates: &self.templates,
            sessions,
            selected: self.selected.clone(),
            layout: self.layout().cloned(),
            panes,
            temporary_pane,
            restore,
            recovery_lease,
            unreachable_processes,
            relaunching: self.relaunching.iter().cloned().collect(),
            reclaiming_workspaces: self.reclaiming_leases.iter().cloned().collect(),
            reclaiming_write_access,
            permission: self.permission_banner(now_ms),
            queue: self.queue.iter().map(queue_item).collect(),
            connection: Some(self.connection.clone()),
            notice: self
                .companion_notice
                .clone()
                .or_else(|| self.notice.clone()),
            write_conflict: self.write_conflict(),
            include_archived: self.include_archived,
            policy: self
                .selected
                .as_ref()
                .and_then(|id| self.policies.get(id))
                .cloned(),
            now_ms,
        }
    }

    /// The permission to show in the banner, if any.
    ///
    /// The most urgent one the daemon's own queue order puts first, matched to its
    /// attention entry so acting on the banner acts on the demand it is showing.
    fn permission_banner(&self, now_ms: i64) -> Option<PendingPermission> {
        let entry = self.queue.iter().find(|item| {
            item.actionable && item.entry.reason == turn_core::AwaitingReason::Permission
        })?;
        let summary = self
            .sessions
            .iter()
            .find(|session| session.id == entry.entry.session_id)?;
        // An attention entry belongs to a Process, not merely to a Session. Prefer
        // that exact node so a Reviewer's permission never displays Claude's pending
        // command or cwd. The primary Agent is only a compatibility fallback for old,
        // completely unscoped daemon projections. A modern unresolved scope must not
        // borrow another Agent's permission details.
        let legacy_unscoped = entry.entry.node_id.is_none()
            && entry.entry.parent_node_id.is_none()
            && entry.entry.subject_external_id.is_none();
        let targeted_node = entry.entry.node_id.as_ref().and_then(|node_id| {
            self.hierarchy
                .as_ref()
                .and_then(|snapshot| {
                    snapshot.workspaces.iter().find_map(|workspace| {
                        workspace.sessions.iter().find_map(|session| {
                            session.nodes.iter().find(|node| &node.node_id == node_id)
                        })
                    })
                })
                .or_else(|| {
                    self.trees
                        .get(&entry.entry.session_id)
                        .and_then(|tree| tree.iter().find(|node| &node.node_id == node_id))
                })
        });
        // Sessions can arrive before the hierarchy/tree projection. A matching
        // primary node id is still an exact resolution, not the legacy fallback.
        let projected_primary = entry.entry.node_id.as_ref().and_then(|node_id| {
            summary
                .primary_agent
                .as_ref()
                .filter(|agent| &agent.node_id == node_id)
        });
        let agent = targeted_node
            .and_then(|node| node.agent.as_ref())
            .or(projected_primary)
            .or_else(|| {
                legacy_unscoped
                    .then_some(summary.primary_agent.as_ref())
                    .flatten()
            })?;
        let pending = agent.pending_permission.as_ref()?;
        Some(PendingPermission {
            attention_id: Some(entry.entry.id.clone()),
            session_id: summary.id.clone(),
            session: summary.name.clone(),
            summary: pending.summary.clone(),
            command: pending.command.clone(),
            // Shown verbatim. Approving something in the wrong repository is the mistake
            // this field exists to prevent.
            cwd: pending.cwd.clone().unwrap_or_else(|| summary.cwd.clone()),
            tool: pending
                .tool_name
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
            risk: pending.risk,
            blocked_secs: now_ms
                .saturating_sub(pending.requested_ms)
                .max(0)
                .saturating_div(1_000) as u64,
            provisional: entry.provisional,
        })
    }
}

/// A deterministic, Git-valid default for the explicit worktree alternative.
/// The daemon still validates it and owns the filesystem path.
fn isolated_branch_name(name: &str, now_ms: i64) -> String {
    let mut slug = String::new();
    let mut separator = false;
    for ch in name.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            if separator && !slug.is_empty() {
                slug.push('-');
            }
            slug.push(ch);
            separator = false;
        } else {
            separator = true;
        }
        if slug.len() >= 28 {
            break;
        }
    }
    let slug = slug.trim_matches('-');
    let slug = if slug.is_empty() { "session" } else { slug };
    format!("turn/{slug}-{}", now_ms.max(0))
}

/// The line under a session's name: what the daemon counted, in words.
fn describe(summary: &SessionSummary) -> String {
    let mut parts: Vec<String> = Vec::new();
    if summary.running_count > 0 {
        parts.push(format!("{} running", summary.running_count));
    }
    if summary.subagent_count > 0 {
        parts.push(format!("{} subagents", summary.subagent_count));
    }
    if summary.pane_count > 1 {
        parts.push(format!("{} panes", summary.pane_count));
    }
    if parts.is_empty() && summary.idle_ms > 60_000 {
        parts.push(format!("{}m idle", summary.idle_ms / 60_000));
    }
    if let Some(branch) = &summary.git_branch {
        parts.push(branch.clone());
    }
    parts.join(" · ")
}

fn queue_item(view: &AttentionView) -> QueueItem {
    QueueItem {
        attention_id: view.entry.id.clone(),
        session_id: view.entry.session_id.clone(),
        session_name: view.session_name.clone(),
        reason: view.entry.reason,
        summary: view.entry.summary.clone(),
        provisional: view.provisional,
        actionable: view.actionable,
    }
}

fn session_for_key(snapshot: &HierarchySnapshot, key: &HierarchyKey) -> Option<SessionId> {
    match key {
        HierarchyKey::Session { session_id } => snapshot
            .workspaces
            .iter()
            .flat_map(|workspace| &workspace.sessions)
            .any(|session| &session.session.id == session_id)
            .then(|| session_id.clone()),
        HierarchyKey::Process { node_id } => snapshot.workspaces.iter().find_map(|workspace| {
            workspace.sessions.iter().find_map(|session| {
                session
                    .nodes
                    .iter()
                    .any(|node| &node.node_id == node_id)
                    .then(|| session.session.id.clone())
            })
        }),
        HierarchyKey::Workspace { workspace_id } => snapshot
            .workspaces
            .iter()
            .find(|workspace| &workspace.workspace.id == workspace_id)
            .and_then(|workspace| workspace.sessions.first())
            .map(|session| session.session.id.clone()),
    }
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

/// Whether a desync is worth telling the user about.
///
/// A missed update is normal on a busy pane and repairs itself by resynchronising; a
/// malformed one means the two ends disagree about the protocol, which is worth a line
/// in the log.
pub fn is_worth_reporting(desync: &Desync) -> bool {
    matches!(desync, Desync::Malformed(_))
}

#[cfg(test)]
mod tests {
    use super::*;
    use turn_core::attention::{AttentionEntry, EntryState};
    use turn_core::event::{Confidence, Risk};
    use turn_core::ids::{AttentionId, WorkspaceId};
    use turn_core::model::{
        DropZone, NodeKind, Pane, PaneNodeBinding, PendingPermission as CorePermission,
        ProcessNode, Relation, Session, Template, Workspace,
    };
    use turn_core::state::{AwaitingReason, Lifecycle, Turn};
    use turn_core::Effect;
    use turn_proto::{
        AttentionView, PaneAttachment, PaneFocusView, PaneStream, ScreenUpdate, ServerEvent,
        SessionTreeView, TerminalBytes, TreeSurfaceState, Welcome, WorkspaceTreeView,
    };

    const T0: i64 = 1_700_000_000_000;

    fn workspace() -> WorkspaceId {
        WorkspaceId::from_stored("ws_desk000001")
    }

    /// A session with one agent pane, and the node behind it.
    fn session_with_agent(name: &str) -> (Session, PaneId, NodeId) {
        let mut pane = Pane::new(PaneKind::Agent).with_command("claude");
        let pane_id = pane.id.clone();
        let mut session =
            Session::new(workspace(), name, "/repo", Layout::single(pane.clone()), T0);
        let mut agent = ProcessNode::agent(session.id.clone(), "claude", "/repo", T0);
        agent.lifecycle = Lifecycle::Alive;
        agent.turn = Some(Turn::Active);
        let node_id = session.tree.insert(agent);
        pane.node_id = Some(node_id.clone());
        if let Some(slot) = session.layout.get_mut(&pane_id) {
            slot.node_id = Some(node_id.clone());
        }
        (session, pane_id, node_id)
    }

    fn summary(session: &Session, badge: usize) -> SessionSummary {
        SessionSummary::from_session(session, badge, false, T0)
    }

    fn session_with_primary_permission(name: &str) -> (Session, NodeId) {
        let (mut session, _, primary_id) = session_with_agent(name);
        let primary = session.tree.get_mut(&primary_id).expect("primary agent");
        primary.turn = Some(Turn::AwaitingUser {
            reason: AwaitingReason::Permission,
        });
        primary.agent.as_mut().unwrap().pending_permission = Some(CorePermission {
            summary: "primary command".into(),
            command: Some("make deploy".into()),
            tool_name: Some("Bash".into()),
            risk: Risk::High,
            requested_ms: T0,
            cwd: Some("/repo/main".into()),
        });
        (session, primary_id)
    }

    fn details(session: &Session) -> turn_proto::SessionDetails {
        turn_proto::SessionDetails::from_session(session, 0, false, T0)
    }

    fn connected() -> Inbound {
        let mut identity = crate::transport::DaemonIdentity::new();
        Inbound::Status(identity.observe(&Welcome::new(1, "0.1.0-test", 4242, T0)))
    }

    fn sent(reactions: &[Reaction]) -> Vec<Request> {
        reactions
            .iter()
            .filter_map(|reaction| match reaction {
                Reaction::Send { request, .. } => Some(request.clone()),
                _ => None,
            })
            .collect()
    }

    fn answer(response: Response) -> Inbound {
        Inbound::Answer {
            ask: Ask::Sessions,
            response: Box::new(response),
        }
    }

    fn attached(
        pane_id: &PaneId,
        session_id: &SessionId,
        node_id: &NodeId,
        grid: Grid,
        next_seq: u64,
    ) -> Inbound {
        Inbound::Answer {
            ask: Ask::Attach {
                session_id: session_id.clone(),
                pane_id: pane_id.clone(),
            },
            response: Box::new(Response::Attached {
                attachment: Box::new(PaneAttachment {
                    session_id: session_id.clone(),
                    pane_id: pane_id.clone(),
                    node_id: Some(node_id.clone()),
                    stream: PaneStream::Cells,
                    screen: Some(Box::new(grid)),
                    replay: TerminalBytes::new(Vec::new()),
                    size: PtySize::new(24, 80),
                    scrollback_truncated: false,
                    bytes_seen: 0,
                    next_seq,
                }),
            }),
        }
    }

    /// Everything is re-fetched on connecting. A window that applied pushes to a copy
    /// from a previous daemon is how a sidebar starts disagreeing with the terminal.
    #[test]
    fn connecting_refetches_the_world_rather_than_trusting_what_it_had() {
        let mut desk = Desk::new();
        let reactions = desk.apply_inbound(connected(), T0);
        let ops: Vec<&str> = sent(&reactions).iter().map(Request::op).collect();
        assert_eq!(
            ops,
            vec!["get_hierarchy", "list_templates", "list_attention"]
        );
        assert!(desk.connection().is_live());
    }

    #[test]
    fn disconnecting_says_so_and_asks_for_nothing() {
        let mut desk = Desk::new();
        let reactions = desk.apply_inbound(
            Inbound::Status(ConnectionState::Disconnected {
                message: "gone".into(),
                retrying: true,
            }),
            T0,
        );
        assert!(sent(&reactions).is_empty());
        assert!(!desk.connection().is_live());
    }

    /// The sidebar order is the daemon's, applied locally with the daemon's own ranking
    /// so a state change moves a row without a round trip.
    #[test]
    fn sessions_are_ordered_by_the_daemons_own_ranking() {
        let (quiet, _, _) = session_with_agent("Quiet");
        let (mut blocked, _, _) = session_with_agent("Blocked");
        let blocked_node = blocked.tree.iter().next().map(|node| node.id.clone());
        if let Some(id) = blocked_node {
            if let Some(node) = blocked.tree.get_mut(&id) {
                node.turn = Some(Turn::AwaitingUser {
                    reason: AwaitingReason::Permission,
                });
            }
        }

        let mut desk = Desk::new();
        desk.apply_inbound(
            answer(Response::Sessions {
                sessions: vec![summary(&quiet, 0), summary(&blocked, 1)],
            }),
            T0,
        );
        assert_eq!(
            desk.sessions().first().map(|s| s.name.as_str()),
            Some("Blocked"),
            "the session that needs the user comes first"
        );
        // And the local sort matches what the daemon would have produced.
        let ranks: Vec<_> = desk.sessions().iter().map(|s| s.sidebar_rank()).collect();
        let mut sorted = ranks.clone();
        sorted.sort_by(|a, b| b.cmp(a));
        assert_eq!(ranks, sorted);
    }

    /// Selecting a session fetches its detail, and the detail is what makes the window
    /// attach — as cells, because this window has no terminal emulator.
    #[test]
    fn selecting_a_session_fetches_it_and_attaches_its_panes_as_cells() {
        let (session, pane_id, _) = session_with_agent("Fix the bug");
        let mut desk = Desk::new();
        desk.apply_inbound(
            answer(Response::Sessions {
                sessions: vec![summary(&session, 0)],
            }),
            T0,
        );
        // The first session is selected automatically. Re-selecting it still persists
        // the authoritative tree selection, but it does not fetch the detail twice.
        let reactions = desk.select(session.id.clone());
        assert!(matches!(
            sent(&reactions).as_slice(),
            [Request::SelectTreeNode { selected: Some(HierarchyKey::Session { session_id }), .. }]
                if session_id == &session.id
        ));

        let reactions = desk.apply_inbound(
            answer(Response::SessionDetails {
                details: Box::new(details(&session)),
            }),
            T0,
        );
        match sent(&reactions).as_slice() {
            [Request::AttachPane {
                pane_id: attached,
                stream,
                ..
            }] => {
                assert_eq!(attached, &pane_id);
                assert_eq!(*stream, PaneStream::Cells);
            }
            other => panic!("expected one attach as cells, got {other:?}"),
        }

        // And it does not attach twice.
        let again = desk.apply_inbound(
            answer(Response::SessionDetails {
                details: Box::new(details(&session)),
            }),
            T0,
        );
        assert!(sent(&again).is_empty(), "one attach per pane");
    }

    #[test]
    fn reselecting_an_ended_session_shows_stopped_panes_instead_of_attaching_blank_grids() {
        let (mut session, pane_id, node_id) = session_with_agent("Ended work");
        session.tree.get_mut(&node_id).unwrap().lifecycle = Lifecycle::Stopped {
            signal: "Terminated".into(),
        };
        let mut desk = Desk::new();
        desk.sessions.push(summary(&session, 0));
        desk.layouts
            .insert(session.id.clone(), session.layout.clone());
        desk.trees
            .insert(session.id.clone(), TreeNodeView::for_session(&session, T0));

        let reactions = desk.select(session.id.clone());

        assert!(sent(&reactions).iter().any(|request| matches!(
            request,
            Request::GetSession { session_id } if session_id == &session.id
        )));
        assert!(!sent(&reactions).iter().any(|request| matches!(
            request,
            Request::AttachPane { pane_id: attached, .. } if attached == &pane_id
        )));
        assert!(!desk.attaching.contains(&pane_id));
    }

    #[test]
    fn alternating_sessions_replaces_the_central_layout_with_each_sessions_own_panes() {
        let (first, first_pane, _) = session_with_agent("One pane");
        let (mut second, second_first_pane, _) = session_with_agent("Two panes");
        let second_shell = Pane::new(PaneKind::Shell).with_title("second session shell");
        let second_shell_id = second_shell.id.clone();
        assert!(second
            .layout
            .split(&second_first_pane, Direction::Horizontal, second_shell));

        let mut desk = Desk::new();
        desk.apply_inbound(
            answer(Response::Sessions {
                sessions: vec![summary(&first, 0), summary(&second, 0)],
            }),
            T0,
        );
        desk.apply_inbound(
            answer(Response::SessionDetails {
                details: Box::new(details(&first)),
            }),
            T0,
        );
        // Session details may be prefetched or arrive from an earlier navigation.
        // Caching them must not make their Layout global.
        desk.apply_inbound(
            answer(Response::SessionDetails {
                details: Box::new(details(&second)),
            }),
            T0,
        );

        let first_view = desk.view(T0);
        assert_eq!(first_view.selected.as_ref(), Some(&first.id));
        assert_eq!(
            first_view
                .layout
                .as_ref()
                .expect("first layout")
                .panes()
                .iter()
                .map(|pane| pane.id.clone())
                .collect::<Vec<_>>(),
            vec![first_pane.clone()]
        );

        desk.apply_view_action(ViewAction::SelectSession(second.id.clone()), T0 + 1);
        let second_view = desk.view(T0 + 1);
        assert_eq!(second_view.selected.as_ref(), Some(&second.id));
        assert_eq!(
            second_view
                .layout
                .as_ref()
                .expect("second layout")
                .panes()
                .iter()
                .map(|pane| pane.id.clone())
                .collect::<Vec<_>>(),
            vec![second_first_pane.clone(), second_shell_id.clone()]
        );

        desk.apply_view_action(ViewAction::SelectSession(first.id.clone()), T0 + 2);
        assert_eq!(desk.view(T0 + 2).layout.unwrap().pane_count(), 1);
        desk.apply_view_action(ViewAction::SelectSession(second.id.clone()), T0 + 3);
        let restored_second = desk.view(T0 + 3).layout.expect("cached second layout");
        assert_eq!(restored_second.pane_count(), 2);
        assert!(restored_second.get(&second_first_pane).is_some());
        assert!(restored_second.get(&second_shell_id).is_some());
    }

    /// The whole point of the sequence number, from the window's side: a missed update
    /// produces a resync rather than a screen neither end believes in.
    #[test]
    fn a_missed_screen_update_asks_for_the_whole_screen_again() {
        let (session, pane_id, node_id) = session_with_agent("Busy");
        let mut desk = Desk::new();
        desk.apply_inbound(
            answer(Response::Sessions {
                sessions: vec![summary(&session, 0)],
            }),
            T0,
        );
        desk.apply_inbound(
            answer(Response::SessionDetails {
                details: Box::new(details(&session)),
            }),
            T0,
        );
        desk.apply_inbound(
            attached(&pane_id, &session.id, &node_id, Grid::blank(24, 80), 1),
            T0,
        );

        // In sequence: applied, nothing asked for.
        let fine = desk.apply_inbound(
            Inbound::Event(Box::new(ServerEvent::PaneScreen {
                session_id: session.id.clone(),
                pane_id: pane_id.clone(),
                node_id: None,
                seq: 1,
                update: ScreenUpdate::full(Grid::from_lines(&["hello"], 80)),
            })),
            T0,
        );
        assert!(sent(&fine).is_empty());

        // A jump: resynchronise.
        let gap = desk.apply_inbound(
            Inbound::Event(Box::new(ServerEvent::PaneScreen {
                session_id: session.id.clone(),
                pane_id: pane_id.clone(),
                node_id: None,
                seq: 9,
                update: ScreenUpdate::full(Grid::from_lines(&["later"], 80)),
            })),
            T0,
        );
        assert!(
            sent(&gap)
                .iter()
                .any(|request| matches!(request, Request::ResyncPane { .. })),
            "got {:?}",
            sent(&gap)
        );
    }

    /// The product's central rule, from the client's side: focus moves only when the
    /// governor granted it.
    #[test]
    fn only_a_granted_focus_effect_moves_the_window() {
        let (first, _, _) = session_with_agent("First");
        let (second, _, _) = session_with_agent("Second");
        let mut desk = Desk::new();
        desk.apply_inbound(
            answer(Response::Sessions {
                sessions: vec![summary(&first, 0), summary(&second, 0)],
            }),
            T0,
        );
        let started = desk.selected().cloned();

        // A deferral and a denial change nothing.
        for effect in [
            Effect::FocusDeferred {
                session_id: second.id.clone(),
                until_ms: T0 + 1_500,
                reason: turn_core::attention::DeferReason::UserTyping,
            },
            Effect::FocusDenied {
                session_id: second.id.clone(),
                reason: turn_core::attention::FocusDenial::RateLimited,
            },
        ] {
            desk.apply_inbound(
                Inbound::Event(Box::new(ServerEvent::AttentionEffect { effect })),
                T0,
            );
            assert_eq!(
                desk.selected().cloned(),
                started,
                "a refused focus request must not move anybody"
            );
        }

        // A granted one does.
        let target = if started.as_ref() == Some(&first.id) {
            second.id.clone()
        } else {
            first.id.clone()
        };
        desk.apply_inbound(
            Inbound::Event(Box::new(ServerEvent::AttentionEffect {
                effect: Effect::Focus {
                    session_id: target.clone(),
                    node_id: None,
                },
            })),
            T0,
        );
        assert_eq!(desk.selected(), Some(&target));
    }

    #[test]
    fn semantic_attention_selects_the_child_but_focuses_its_runtime_owner_pane() {
        let (session, pane_id, runtime_owner) = session_with_agent("Fix climbing bugs");
        let subject = NodeId::from_stored("proc_reviewer_semantic");
        let mut desk = Desk::new();
        desk.apply_inbound(
            answer(Response::Sessions {
                sessions: vec![summary(&session, 1)],
            }),
            T0,
        );
        desk.apply_inbound(
            answer(Response::SessionDetails {
                details: Box::new(details(&session)),
            }),
            T0,
        );

        let navigation = desk.apply_inbound(
            Inbound::Event(Box::new(ServerEvent::AttentionEffect {
                effect: Effect::Focus {
                    session_id: session.id.clone(),
                    node_id: Some(subject.clone()),
                },
            })),
            T0,
        );
        let requests = sent(&navigation);
        let child_selection = requests
            .iter()
            .position(|request| {
                matches!(
                    request,
                    Request::SelectTreeNode {
                        selected: Some(HierarchyKey::Process { node_id }),
                        ..
                    } if node_id == &subject
                )
            })
            .expect("the exact Reviewer node is selected");
        let focus_resolution = requests
            .iter()
            .position(|request| {
                matches!(
                    request,
                    Request::FocusPaneForAttention {
                        session_id,
                        subject_node_id,
                        ..
                    } if session_id == &session.id && subject_node_id == &subject
                )
            })
            .expect("the daemon resolves the separate input target");
        assert!(child_selection < focus_resolution);
        assert!(!requests.iter().any(|request| matches!(
            request,
            Request::FocusPaneForNode { node_id, .. } if node_id == &runtime_owner
        )));

        let focused = desk.apply_inbound(
            Inbound::Answer {
                ask: Ask::AttentionFocus {
                    session_id: session.id.clone(),
                    subject_node_id: subject.clone(),
                },
                response: Box::new(Response::PaneFocus {
                    focus: Some(PaneFocusView {
                        surface_id: "main-window".into(),
                        session_id: session.id.clone(),
                        node_id: runtime_owner,
                        pane_id: pane_id.clone(),
                        attention_subject_node_id: Some(subject.clone()),
                    }),
                }),
            },
            T0,
        );
        assert!(matches!(
            sent(&focused).as_slice(),
            [Request::FocusPane {
                session_id,
                target: FocusTarget::Pane { pane_id: focused },
            }] if session_id == &session.id && focused == &pane_id
        ));
        assert!(!sent(&focused)
            .iter()
            .any(|request| matches!(request, Request::SelectTreeNode { .. })));
    }

    #[test]
    fn semantic_preview_and_temporary_pane_never_impersonate_the_owner_terminal() {
        let session_id = SessionId::from_stored("sess_semantic_preview");
        let subject = NodeId::from_stored("proc_reviewer_semantic");
        let mut desk = Desk::new();

        assert!(matches!(
            sent(&desk.apply_hierarchy_action(HierarchyAction::QuickPreview {
                surface_id: "main-window".into(),
                session_id: session_id.clone(),
                node_id: subject.clone(),
            }))
            .as_slice(),
            [Request::GetPreviewHistory { node_id, .. }] if node_id == &subject
        ));
        assert!(matches!(
            sent(&desk.apply_hierarchy_action(HierarchyAction::OpenTemporaryPane {
                surface_id: "main-window".into(),
                session_id: session_id.clone(),
                node_id: subject.clone(),
            }))
            .as_slice(),
            [Request::OpenNodeAsTemporaryPane { node_id, .. }] if node_id == &subject
        ));

        let pane_id = PaneId::from_stored("pane_reviewer_preview");
        let opened = desk.apply_inbound(
            Inbound::Answer {
                ask: Ask::NodePane,
                response: Box::new(Response::NodePane {
                    pane: NodePaneView {
                        binding: PaneNodeBinding {
                            pane_id: pane_id.clone(),
                            session_id: session_id.clone(),
                            node_id: subject.clone(),
                            temporary: true,
                            surface_id: Some("main-window".into()),
                            opened_ms: T0,
                        },
                        capability: NodePaneCapability::PreviewDetails,
                    },
                }),
            },
            T0,
        );
        assert!(sent(&opened).iter().any(|request| matches!(
            request,
            Request::GetPreviewHistory { node_id, .. } if node_id == &subject
        )));
        assert!(!sent(&opened)
            .iter()
            .any(|request| matches!(request, Request::AttachPane { .. })));

        let runtime_pane = PaneId::from_stored("pane_claude_runtime");
        let runtime_owner = NodeId::from_stored("proc_claude_runtime");
        let routed = desk.apply_inbound(
            Inbound::Answer {
                ask: Ask::AttentionFocus {
                    session_id: session_id.clone(),
                    subject_node_id: subject.clone(),
                },
                response: Box::new(Response::PaneFocus {
                    focus: Some(PaneFocusView {
                        surface_id: "main-window".into(),
                        session_id: session_id.clone(),
                        node_id: runtime_owner,
                        pane_id: runtime_pane.clone(),
                        attention_subject_node_id: Some(subject),
                    }),
                }),
            },
            T0,
        );
        let requests = sent(&routed);
        assert!(requests.iter().any(|request| matches!(
            request,
            Request::ClosePane {
                pane_id: closed,
                disposition: CloseDisposition::KeepProcesses,
                ..
            } if closed == &pane_id
        )));
        assert!(requests.iter().any(|request| matches!(
            request,
            Request::FocusPane {
                target: FocusTarget::Pane { pane_id },
                ..
            } if pane_id == &runtime_pane
        )));
        assert!(desk.temporary_pane().is_none());
    }

    #[test]
    fn removing_a_session_drops_every_local_temporary_pane_resource() {
        let session_id = SessionId::from_stored("sess_removed_preview");
        let node_id = NodeId::from_stored("proc_removed_preview");
        let pane_id = PaneId::from_stored("pane_removed_preview");
        let mut desk = Desk::new();
        desk.apply_inbound(
            Inbound::Answer {
                ask: Ask::NodePane,
                response: Box::new(Response::NodePane {
                    pane: NodePaneView {
                        binding: PaneNodeBinding {
                            pane_id: pane_id.clone(),
                            session_id: session_id.clone(),
                            node_id,
                            temporary: true,
                            surface_id: Some("main-window".into()),
                            opened_ms: T0,
                        },
                        capability: NodePaneCapability::PreviewDetails,
                    },
                }),
            },
            T0,
        );
        desk.feeds
            .insert(pane_id.clone(), PaneFeed::blank(INITIAL_SIZE));
        desk.attaching.insert(pane_id.clone());
        desk.pty_sizes.insert(pane_id.clone(), INITIAL_SIZE);

        desk.apply_inbound(
            Inbound::Event(Box::new(ServerEvent::SessionRemoved {
                session_id: session_id.clone(),
                workspace_id: workspace(),
            })),
            T0 + 1,
        );

        assert!(desk.temporary_pane.is_none());
        assert!(!desk.pane_owner.contains_key(&pane_id));
        assert!(!desk.feeds.contains_key(&pane_id));
        assert!(!desk.attaching.contains(&pane_id));
        assert!(!desk.pty_sizes.contains_key(&pane_id));
    }

    #[test]
    fn hiding_an_archived_session_closes_its_surface_scoped_temporary_pane() {
        let (session, _, node_id) = session_with_agent("Archived task");
        let mut project = Workspace::new("project", "/repo", T0);
        project.id = session.workspace_id.clone();
        let summary = summary(&session, 0);
        let mut desk = Desk::new();
        desk.apply_inbound(
            answer(Response::Hierarchy {
                snapshot: Box::new(HierarchySnapshot {
                    revision: 1,
                    tree_state: TreeSurfaceState::empty("main-window"),
                    workspaces: vec![WorkspaceTreeView {
                        workspace: WorkspaceSummary::from_workspace(
                            &project,
                            std::slice::from_ref(&summary),
                        ),
                        checkouts: Vec::new(),
                        write_lease: None,
                        sessions: vec![SessionTreeView {
                            session: summary,
                            nodes: TreeNodeView::for_session(&session, T0),
                        }],
                    }],
                }),
            }),
            T0,
        );
        let pane_id = PaneId::from_stored("pane_hidden_preview");
        desk.apply_inbound(
            Inbound::Answer {
                ask: Ask::NodePane,
                response: Box::new(Response::NodePane {
                    pane: NodePaneView {
                        binding: PaneNodeBinding {
                            pane_id: pane_id.clone(),
                            session_id: session.id.clone(),
                            node_id,
                            temporary: true,
                            surface_id: Some("main-window".into()),
                            opened_ms: T0,
                        },
                        capability: NodePaneCapability::PreviewDetails,
                    },
                }),
            },
            T0,
        );

        let reactions = desk.apply_inbound(
            answer(Response::Hierarchy {
                snapshot: Box::new(HierarchySnapshot::empty("main-window", 2)),
            }),
            T0 + 1,
        );
        assert!(matches!(
            sent(&reactions).as_slice(),
            [Request::ClosePane {
                session_id,
                pane_id: closed,
                disposition: CloseDisposition::KeepProcesses,
            }] if session_id == &session.id && closed == &pane_id
        ));
        assert!(desk.temporary_pane.is_none());
        assert!(!desk.pane_owner.contains_key(&pane_id));
    }

    /// Answering a pending agent interaction is human input through `WritePty`.
    #[test]
    fn typing_in_a_pane_writes_to_the_pty_and_nothing_else_does() {
        let (session, pane_id, node_id) = session_with_agent("Answer me");
        let mut desk = Desk::new();
        desk.apply_inbound(
            answer(Response::Sessions {
                sessions: vec![summary(&session, 0)],
            }),
            T0,
        );
        desk.apply_inbound(
            answer(Response::SessionDetails {
                details: Box::new(details(&session)),
            }),
            T0,
        );

        let reactions = desk.apply_view_action(
            ViewAction::Pane {
                pane_id: pane_id.clone(),
                action: PaneAction::Write(b"y\r".to_vec()),
            },
            T0,
        );
        match sent(&reactions).as_slice() {
            [Request::WritePty {
                node_id: written,
                data,
                ..
            }] => {
                assert_eq!(written, &node_id, "addressed to the node, not the pane");
                assert_eq!(data.as_slice(), b"y\r");
            }
            other => panic!("expected one write, got {other:?}"),
        }

        // And no command produces anything that approves on the user's behalf.
        for command in Command::ALL {
            for request in sent(&desk.dispatch(*command, T0)) {
                assert_ne!(
                    request.op(),
                    "write_pty",
                    "{command:?} must not type on the user's behalf"
                );
            }
        }
    }

    #[test]
    fn a_resize_is_sent_when_it_changes_and_not_on_every_frame() {
        let (session, pane_id, _) = session_with_agent("Resizing");
        let mut desk = Desk::new();
        desk.apply_inbound(
            answer(Response::Sessions {
                sessions: vec![summary(&session, 0)],
            }),
            T0,
        );
        desk.apply_inbound(
            answer(Response::SessionDetails {
                details: Box::new(details(&session)),
            }),
            T0,
        );

        let first = desk.apply_view_action(
            ViewAction::Pane {
                pane_id: pane_id.clone(),
                action: PaneAction::Resize(PtySize::new(40, 120)),
            },
            T0,
        );
        assert_eq!(sent(&first).len(), 1);
        let again = desk.apply_view_action(
            ViewAction::Pane {
                pane_id: pane_id.clone(),
                action: PaneAction::Resize(PtySize::new(40, 120)),
            },
            T0,
        );
        assert!(sent(&again).is_empty(), "the same size costs nothing");
        let changed = desk.apply_view_action(
            ViewAction::Pane {
                pane_id,
                action: PaneAction::Resize(PtySize::new(41, 120)),
            },
            T0,
        );
        assert_eq!(sent(&changed).len(), 1);
    }

    /// A session of two side-by-side panes, drawn once so the arrangement exists.
    fn two_pane_session() -> (Desk, Session, PaneId, PaneId) {
        let left = Pane::new(PaneKind::Shell).with_title("left");
        let left_id = left.id.clone();
        let mut session = Session::new(workspace(), "Move me", "/repo", Layout::single(left), T0);
        let right = Pane::new(PaneKind::Shell).with_title("right");
        let right_id = right.id.clone();
        session.layout.split(&left_id, Direction::Horizontal, right);
        // The daemon says which pane has focus, and a directional command is about that
        // pane. Leaving it unset would make this a test of "nothing is selected".
        session.layout.active = Some(left_id.clone());
        let mut desk = Desk::new();
        desk.apply_inbound(
            answer(Response::Sessions {
                sessions: vec![summary(&session, 0)],
            }),
            T0,
        );
        desk.apply_inbound(
            answer(Response::SessionDetails {
                details: Box::new(details(&session)),
            }),
            T0,
        );
        // The geometry the window drew. Directional commands are answered from it rather
        // than from tree order, so a test that skipped this would be testing nothing.
        let arrangement = desk.arrange(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1000.0, 600.0),
        ));
        desk.remember_arrangement(arrangement);
        (desk, session, left_id, right_id)
    }

    /// A session of three panes: one tall on the left, two stacked on the right, drawn
    /// once so the arrangement exists. The shape a keyboard move has to be able to change.
    fn three_pane_session() -> (Desk, Session, PaneId, PaneId, PaneId) {
        let tall = Pane::new(PaneKind::Shell).with_title("tall");
        let tall_id = tall.id.clone();
        let mut session = Session::new(workspace(), "Move me", "/repo", Layout::single(tall), T0);
        let top = Pane::new(PaneKind::Shell).with_title("top");
        let top_id = top.id.clone();
        session.layout.split(&tall_id, Direction::Horizontal, top);
        let bottom = Pane::new(PaneKind::Shell).with_title("bottom");
        let bottom_id = bottom.id.clone();
        session.layout.split(&top_id, Direction::Vertical, bottom);
        session.layout.active = Some(bottom_id.clone());
        let mut desk = Desk::new();
        desk.apply_inbound(
            answer(Response::Sessions {
                sessions: vec![summary(&session, 0)],
            }),
            T0,
        );
        desk.apply_inbound(
            answer(Response::SessionDetails {
                details: Box::new(details(&session)),
            }),
            T0,
        );
        let arrangement = desk.arrange(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1000.0, 600.0),
        ));
        desk.remember_arrangement(arrangement);
        (desk, session, tall_id, top_id, bottom_id)
    }

    /// The keyboard equivalent of dragging a header, and it has to be the *same*
    /// operation: a relocation naming a zone, not an exchange of two panes. Drag-only
    /// would be unusable without a pointer, and the daemon is the one that rearranges the
    /// tree either way.
    #[test]
    fn moving_a_pane_by_keyboard_relocates_it_past_the_neighbour_on_that_side() {
        let (mut desk, session, left, right) = two_pane_session();
        match sent(&desk.dispatch(Command::MovePaneRight, T0)).as_slice() {
            [Request::RelocatePane {
                session_id,
                moved,
                target,
                zone,
            }] => {
                assert_eq!(session_id, &session.id);
                assert_eq!(moved, &left, "the active pane is the one that moves");
                assert_eq!(
                    target, &right,
                    "and it is relocated against the pane actually on that side"
                );
                assert_eq!(
                    zone,
                    &DropZone::Right,
                    "past that pane, not into the side of it that it already occupies"
                );
            }
            other => panic!("got {other:?}"),
        }
    }

    /// The reading that makes the keyboard a first-class path: with nothing above either
    /// pane of a row, "move up" turns the row into a column instead of doing nothing. A
    /// keyboard that refused here could never change a layout's orientation at all.
    #[test]
    fn moving_a_pane_of_a_row_upwards_relocates_it_to_the_top_of_the_layout() {
        let (mut desk, session, left, right) = two_pane_session();
        match sent(&desk.dispatch(Command::MovePaneUp, T0)).as_slice() {
            [Request::RelocatePane {
                session_id,
                moved,
                target,
                zone,
            }] => {
                assert_eq!(session_id, &session.id);
                assert_eq!(moved, &left);
                assert_eq!(target, &right);
                assert_eq!(zone, &DropZone::Above);
            }
            other => panic!("got {other:?}"),
        }
    }

    /// Focus belongs to the daemon, and a directional command is about the pane that has
    /// it, so a test that wants to move a different pane has to be told about it the way
    /// the window would be.
    fn focus_pane(desk: &mut Desk, session: &mut Session, pane: &PaneId) {
        session.layout.active = Some(pane.clone());
        desk.apply_inbound(
            answer(Response::SessionDetails {
                details: Box::new(details(session)),
            }),
            T0,
        );
        let arrangement = desk.arrange(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1000.0, 600.0),
        ));
        desk.remember_arrangement(arrangement);
    }

    /// The one case with nowhere to go: the pane already spans that edge of the layout on
    /// its own. The window says so rather than sending a request that could only be
    /// refused, and rather than wrapping the pane round to the far side.
    #[test]
    fn a_pane_that_already_spans_an_edge_alone_reports_that_it_cannot_move_further() {
        let (mut desk, mut session, tall, _top, _bottom) = three_pane_session();
        focus_pane(&mut desk, &mut session, &tall);

        let nothing = desk.dispatch(Command::MovePaneLeft, T0);
        assert!(sent(&nothing).is_empty(), "it sent a request anyway");
        assert!(
            nothing
                .iter()
                .any(|reaction| matches!(reaction, Reaction::Notice(_))),
            "it said nothing about doing nothing"
        );
    }

    /// A pane nested in a column, moved along the axis the column does not run in: it
    /// leaves the column. This is the rearrangement that was impossible before — the
    /// layout's shape used to be fixed at creation.
    #[test]
    fn moving_a_nested_pane_out_of_its_column_names_the_pane_owning_that_edge() {
        let (mut desk, session, tall, top, bottom) = three_pane_session();
        // Within the column there is a neighbour, and the two exchange ends of it.
        match sent(&desk.dispatch(Command::MovePaneUp, T0)).as_slice() {
            [Request::RelocatePane {
                moved,
                target,
                zone,
                ..
            }] => {
                assert_eq!(moved, &bottom);
                assert_eq!(target, &top);
                assert_eq!(zone, &DropZone::Above);
            }
            other => panic!("got {other:?}"),
        }
        // Downwards there is nothing below it, so it goes to the outer edge: beside the
        // tall pane, which is what owns the bottom of the layout.
        match sent(&desk.dispatch(Command::MovePaneDown, T0)).as_slice() {
            [Request::RelocatePane {
                session_id,
                moved,
                target,
                zone,
            }] => {
                assert_eq!(session_id, &session.id);
                assert_eq!(moved, &bottom);
                assert_eq!(target, &tall);
                assert_eq!(zone, &DropZone::Below);
            }
            other => panic!("got {other:?}"),
        }
    }

    /// The same move, from the header drag. It names both panes and the zone, so the daemon
    /// is never asked to guess which one was dropped where, or where in it.
    #[test]
    fn dropping_a_pane_on_a_region_of_another_asks_for_exactly_that_relocation() {
        let (mut desk, session, left, right) = two_pane_session();
        for zone in [
            DropZone::Left,
            DropZone::Right,
            DropZone::Above,
            DropZone::Below,
            DropZone::Centre,
        ] {
            match sent(&desk.apply_view_action(
                ViewAction::RelocatePane {
                    moved: right.clone(),
                    target: left.clone(),
                    zone,
                },
                T0,
            ))
            .as_slice()
            {
                [Request::RelocatePane {
                    session_id,
                    moved,
                    target,
                    zone: sent_zone,
                }] => {
                    assert_eq!(session_id, &session.id);
                    assert_eq!(moved, &right);
                    assert_eq!(target, &left);
                    assert_eq!(sent_zone, &zone);
                }
                other => panic!("got {other:?}"),
            }
        }
        // A pane dropped back on itself is not a move, and must not cost a round trip.
        assert!(sent(&desk.apply_view_action(
            ViewAction::RelocatePane {
                moved: left.clone(),
                target: left,
                zone: DropZone::Left,
            },
            T0,
        ))
        .is_empty());
    }

    /// The architecture's own rule, checked at the seam it matters: the window sends the
    /// request and keeps the layout it has until the daemon answers. A window that moved
    /// the pane optimistically and then received a different layout would flicker.
    #[test]
    fn relocating_a_pane_does_not_move_the_windows_own_copy_of_the_layout() {
        let (mut desk, session, left, right) = two_pane_session();
        let before = desk.layout().cloned().expect("a layout was pushed");

        let reactions = desk.apply_view_action(
            ViewAction::RelocatePane {
                moved: left.clone(),
                target: right.clone(),
                zone: DropZone::Below,
            },
            T0,
        );
        assert_eq!(sent(&reactions).len(), 1);
        assert_eq!(
            desk.layout(),
            Some(&before),
            "the window must not rearrange its own copy"
        );

        // And when the daemon answers, the window renders what it decided — including a
        // shape the window could not have produced on its own.
        let mut moved = before.clone();
        assert!(moved.relocate(&left, &right, DropZone::Below));
        desk.apply_inbound(
            answer(Response::Layout {
                session_id: session.id.clone(),
                layout: moved.clone(),
            }),
            T0,
        );
        assert_eq!(desk.layout(), Some(&moved));
        assert!(desk.layout().expect("a layout").sizes_are_normalised());
    }

    /// The close control on a pane header closes *that* pane, and the process it was
    /// showing keeps running — which is the promise the tooltip makes.
    #[test]
    fn closing_one_pane_from_its_header_keeps_its_process_running() {
        let (mut desk, session, _, right) = two_pane_session();
        match sent(&desk.apply_view_action(
            ViewAction::ClosePane {
                pane_id: right.clone(),
            },
            T0,
        ))
        .as_slice()
        {
            [Request::ClosePane {
                session_id,
                pane_id,
                disposition,
            }] => {
                assert_eq!(session_id, &session.id);
                assert_eq!(pane_id, &right, "the named pane, not whichever is active");
                assert_eq!(*disposition, CloseDisposition::KeepProcesses);
            }
            other => panic!("got {other:?}"),
        }
    }

    /// The same Session with nothing running in it, which is the state archiving requires.
    fn with_nothing_running(mut session: Session) -> Session {
        let nodes: Vec<NodeId> = session.tree.iter().map(|node| node.id.clone()).collect();
        for node in nodes {
            if let Some(process) = session.tree.get_mut(&node) {
                process.lifecycle = Lifecycle::Exited { code: 0 };
                process.turn = None;
            }
        }
        session
    }

    /// A tree of one Workspace holding `sessions`, with one row selected.
    ///
    /// Built through the same protocol answer the daemon sends, so the tests below
    /// exercise the projection the window actually navigates rather than a second model
    /// assembled for their convenience.
    fn hierarchy_of(sessions: &[&Session], selected: Option<HierarchyKey>) -> Inbound {
        let mut project = Workspace::new("project", "/repo", T0);
        project.id = workspace();
        let summaries: Vec<SessionSummary> =
            sessions.iter().map(|session| summary(session, 0)).collect();
        answer(Response::Hierarchy {
            snapshot: Box::new(HierarchySnapshot {
                revision: 1,
                tree_state: TreeSurfaceState {
                    surface_id: "main-window".into(),
                    selected,
                    expanded: vec![HierarchyKey::workspace(project.id.clone())],
                },
                workspaces: vec![WorkspaceTreeView {
                    workspace: WorkspaceSummary::from_workspace(&project, &summaries),
                    checkouts: Vec::new(),
                    write_lease: None,
                    sessions: sessions
                        .iter()
                        .zip(summaries)
                        .map(|(session, summary)| SessionTreeView {
                            session: summary,
                            nodes: TreeNodeView::for_session(session, T0),
                        })
                        .collect(),
                }],
            }),
        })
    }

    /// The rule that makes the pair of controls on every row safe to click: closing is the
    /// only thing that stops a process, and the keystroke does not close. It opens the
    /// question, and only an accepted answer sends anything.
    #[test]
    fn a_close_command_stops_nothing_until_its_confirmation_is_accepted() {
        let (session, _, _) = session_with_agent("Keep my work");
        let mut desk = Desk::new();
        desk.apply_inbound(
            hierarchy_of(&[&session], Some(HierarchyKey::session(session.id.clone()))),
            T0,
        );

        assert_eq!(
            sent(&desk.dispatch(Command::CloseSession, T0)),
            Vec::new(),
            "the keystroke may not terminate anything on its own"
        );
        assert_eq!(
            sent(&desk.dispatch(Command::CloseWorkspace, T0)),
            Vec::new(),
            "and neither may the Workspace-wide one"
        );

        // What it does instead: name what would be stopped.
        let confirmation = desk
            .end_session_confirmation()
            .expect("the selected Session can be closed");
        assert_eq!(
            confirmation,
            LifecycleConfirmation::EndSession {
                session_id: session.id.clone(),
                name: "Keep my work".into(),
                running_count: 1,
            }
        );

        // And accepting it is the one path that terminates.
        match sent(&desk.apply_view_action(
            ViewAction::CloseSession {
                session_id: session.id.clone(),
                disposition: CloseDisposition::Terminate,
            },
            T0,
        ))
        .as_slice()
        {
            [Request::CloseSession {
                session_id,
                disposition: CloseDisposition::Terminate,
            }] => assert_eq!(session_id, &session.id),
            other => panic!("got {other:?}"),
        }
    }

    /// Taking a row out of the tree is not stopping the work in it. Nothing in the archive
    /// path carries a disposition, so there is no version of this that could terminate.
    #[test]
    fn archiving_from_the_tree_selection_asks_for_nothing_to_be_stopped() {
        // Archiving requires that nothing is running, which is the point: it is what the
        // action does *not* have to stop in order to work.
        let idle = with_nothing_running(session_with_agent("Finished last week").0);
        let mut desk = Desk::new();
        desk.apply_inbound(
            hierarchy_of(&[&idle], Some(HierarchyKey::session(idle.id.clone()))),
            T0,
        );

        match sent(&desk.dispatch(Command::ArchiveSession, T0)).as_slice() {
            [Request::ArchiveSession {
                session_id,
                archived: true,
            }] => assert_eq!(session_id, &idle.id),
            other => panic!("got {other:?}"),
        }
        match sent(&desk.dispatch(Command::ArchiveWorkspace, T0)).as_slice() {
            [Request::ArchiveWorkspace {
                workspace_id,
                archived: true,
            }] => assert_eq!(workspace_id, &workspace()),
            other => panic!("got {other:?}"),
        }

        // The negative half, which is the product rule: no archive path asks for a stop.
        for command in [Command::ArchiveSession, Command::ArchiveWorkspace] {
            for request in sent(&desk.dispatch(command, T0)) {
                assert!(
                    !matches!(
                        request,
                        Request::CloseSession { .. }
                            | Request::CloseWorkspace { .. }
                            | Request::TerminateNode { .. }
                            | Request::InterruptNode { .. }
                    ),
                    "{command:?} sent {request:?}"
                );
            }
        }
    }

    /// Archiving something that is still working is refused with a reason rather than
    /// quietly stopping it to make the archive possible.
    #[test]
    fn archiving_something_that_is_still_running_says_so_and_sends_nothing() {
        let (running, _, _) = session_with_agent("Still working");
        let mut desk = Desk::new();
        desk.apply_inbound(
            hierarchy_of(&[&running], Some(HierarchyKey::session(running.id.clone()))),
            T0,
        );

        for command in [Command::ArchiveSession, Command::ArchiveWorkspace] {
            let reactions = desk.dispatch(command, T0);
            assert_eq!(sent(&reactions), Vec::new(), "{command:?} sent a request");
            assert!(
                matches!(reactions.as_slice(), [Reaction::Notice(message)] if message.contains("end the")),
                "{command:?} gave {reactions:?}"
            );
        }
    }

    /// An archived row has to leave the tree, and the tree is the daemon's answer. The
    /// acknowledgement therefore asks again, carrying the archived preference — the same
    /// request that decides what the tree contains.
    #[test]
    fn an_archive_acknowledgement_refetches_the_tree_under_the_archived_preference() {
        let mut desk = Desk::new();
        for ask in [
            Ask::ArchiveSession { archived: true },
            Ask::ArchiveWorkspace { archived: true },
        ] {
            match sent(&desk.apply_inbound(
                Inbound::Answer {
                    ask: ask.clone(),
                    response: Box::new(Response::Ack),
                },
                T0,
            ))
            .as_slice()
            {
                [Request::GetHierarchy {
                    include_archived: false,
                    ..
                }] => {}
                other => panic!("{ask:?} gave {other:?}"),
            }
        }

        // And a refusal names the act rather than arriving as a bare protocol message, which
        // is the other half of why these are typed asks and not `Ask::Action`.
        let reactions = desk.apply_inbound(
            Inbound::Failed {
                ask: Ask::ArchiveSession { archived: true },
                error: turn_proto::ProtoError::new(
                    turn_proto::ErrorCode::Conflict,
                    "the Session still holds the write lease",
                ),
            },
            T0,
        );
        match reactions.as_slice() {
            [Reaction::Notice(message)] => {
                assert!(message.contains("archiving the session"), "got {message}");
                assert!(message.contains("write lease"), "got {message}");
            }
            other => panic!("got {other:?}"),
        }

        // With the preference on, the same acknowledgement asks for the archived rows, so
        // restoring one shows it again instead of leaving a gap where it used to be.
        desk.apply_view_action(ViewAction::SetArchivedVisibility { include: true }, T0);
        match sent(&desk.apply_inbound(
            Inbound::Answer {
                ask: Ask::ArchiveSession { archived: false },
                response: Box::new(Response::Ack),
            },
            T0,
        ))
        .as_slice()
        {
            [Request::GetHierarchy {
                include_archived: true,
                ..
            }] => {}
            other => panic!("got {other:?}"),
        }
    }

    /// The tree's selection is what these act on, not whichever Session happens to own the
    /// panes on screen. Selecting a row and pressing the chord must not reach past it.
    #[test]
    fn a_lifecycle_command_acts_on_the_row_the_tree_is_pointing_at() {
        let (visible, _, _) = session_with_agent("On screen");
        let (other, _, _) = session_with_agent("Selected in the tree");
        let mut desk = Desk::new();
        desk.apply_inbound(
            hierarchy_of(
                &[&visible, &other],
                Some(HierarchyKey::session(visible.id.clone())),
            ),
            T0,
        );
        desk.select(visible.id.clone());
        desk.set_navigation_hint(Some(HierarchyKey::session(other.id.clone())));

        assert_eq!(
            desk.target_session().map(|summary| summary.id.clone()),
            Some(other.id.clone()),
            "the row the user is pointing at wins over the panes on screen"
        );
        assert_eq!(
            desk.end_session_confirmation()
                .expect("a Session is selected"),
            LifecycleConfirmation::EndSession {
                session_id: other.id.clone(),
                name: "Selected in the tree".into(),
                running_count: 1,
            }
        );

        // A selected Workspace names no Session: "the first one in it" is not the row the
        // user pointed at, and closing it would end work nobody named.
        desk.set_navigation_hint(Some(HierarchyKey::workspace(workspace())));
        assert_eq!(
            desk.target_session().map(|summary| summary.id.clone()),
            Some(visible.id.clone()),
            "with a Workspace selected it falls back to the Session on screen, not to a guess"
        );
    }

    /// Closing a Workspace reaches every Session in it, so the confirmation counts them.
    #[test]
    fn the_workspace_confirmation_counts_the_sessions_it_would_reach() {
        let (busy, _, _) = session_with_agent("Fix climbing bugs");
        let (also_busy, _, _) = session_with_agent("Review the physics");
        let quiet = with_nothing_running(session_with_agent("Nothing running here").0);
        let mut desk = Desk::new();
        desk.apply_inbound(
            hierarchy_of(
                &[&busy, &also_busy, &quiet],
                Some(HierarchyKey::workspace(workspace())),
            ),
            T0,
        );

        assert_eq!(
            desk.stop_workspace_confirmation()
                .expect("a Workspace is selected"),
            LifecycleConfirmation::StopWorkspace {
                workspace_id: workspace(),
                name: "project".into(),
                session_count: 3,
                running_sessions: 2,
                running_processes: 2,
            }
        );
    }

    /// Nothing relaunches on its own: no push and no command produces one.
    #[test]
    fn nothing_ever_relaunches_without_being_asked() {
        let (session, _, _) = session_with_agent("Restored");
        let mut desk = Desk::new();
        desk.apply_inbound(
            answer(Response::Sessions {
                sessions: vec![summary(&session, 0)],
            }),
            T0,
        );
        let reactions = desk.apply_inbound(
            Inbound::Event(Box::new(ServerEvent::RestoreResult {
                session_id: session.id.clone(),
                state: turn_core::model::RestoreState::PartiallyRestored,
                needs_explanation: true,
                panes: vec![turn_proto::PaneRestoreOutcome {
                    pane_id: PaneId::new(),
                    node_id: NodeId::from_stored("proc_watch_restore"),
                    lifecycle: Lifecycle::Lost,
                    can_relaunch: true,
                    command: Some("cargo watch -x test".into()),
                    auto_start: false,
                    needs_checkout_write: true,
                }],
            })),
            T0,
        );
        assert!(
            sent(&reactions).is_empty(),
            "a restore reports and offers; it must not start anything"
        );
        let node_id = {
            let view = desk.view(T0);
            let restore = view.restore.expect("the selected Session exposes recovery");
            assert_eq!(restore.panes.len(), 1);
            assert_eq!(
                restore.panes[0].command.as_deref(),
                Some("cargo watch -x test")
            );
            assert!(
                view.notice.is_none(),
                "recovery is an actionable state, not a red global error"
            );
            restore.panes[0].node_id.clone()
        };

        for command in Command::ALL {
            for request in sent(&desk.dispatch(*command, T0)) {
                assert_ne!(request.op(), "relaunch_node", "{command:?}");
            }
        }

        let explicit = desk.apply_view_action(
            ViewAction::RelaunchNode {
                session_id: session.id.clone(),
                node_id: node_id.clone(),
                resume: false,
            },
            T0,
        );
        assert!(matches!(
            sent(&explicit).as_slice(),
            [Request::RelaunchNode {
                session_id,
                node_id: requested,
                resume: false,
            }] if session_id == &session.id && requested == &node_id
        ));
    }

    #[test]
    fn restored_write_access_is_reclaimed_atomically_without_relaunching() {
        let (mut session, _, _) = session_with_agent("Recovered writer");
        let checkout_id = turn_core::ids::CheckoutId::primary_for(&session.workspace_id);
        session.mode = turn_core::model::SessionMode::MainCheckout;
        session.checkout_id = checkout_id.clone();
        let mut lease = turn_core::model::WorkspaceWriteLease::active(
            session.workspace_id.clone(),
            session.id.clone(),
            checkout_id.clone(),
            T0,
        );
        lease.state = LeaseState::RecoveryRequired;

        let mut desk = Desk::new();
        let acquire = desk.apply_view_action(
            ViewAction::ReclaimWorkspaceWriteLease {
                workspace_id: session.workspace_id.clone(),
                session_id: session.id.clone(),
                checkout_id: checkout_id.clone(),
            },
            T0,
        );
        assert!(matches!(
            sent(&acquire).as_slice(),
            [Request::AcquireWorkspaceWriteLease {
                workspace_id,
                session_id,
                checkout_id: requested,
            }] if workspace_id == &session.workspace_id
                && session_id == &session.id
                && requested == &checkout_id
        ));
        assert_eq!(
            sent(&acquire).len(),
            1,
            "recovery must have no unleased gap"
        );

        lease.state = LeaseState::Active;
        let answered = desk.apply_inbound(
            Inbound::Answer {
                ask: Ask::RestoreLeaseAcquire {
                    workspace_id: session.workspace_id.clone(),
                    session_id: session.id.clone(),
                    checkout_id: checkout_id.clone(),
                },
                response: Box::new(Response::WorkspaceWriteLease {
                    workspace_id: session.workspace_id.clone(),
                    lease: Some(lease),
                }),
            },
            T0 + 1,
        );
        assert!(
            sent(&answered)
                .iter()
                .all(|request| !matches!(request, Request::RelaunchNode { .. })),
            "confirming authority must still leave process launch to a second user action"
        );

        let retry = desk.apply_view_action(
            ViewAction::ReclaimWorkspaceWriteLease {
                workspace_id: session.workspace_id.clone(),
                session_id: session.id.clone(),
                checkout_id,
            },
            T0 + 2,
        );
        assert_eq!(
            sent(&retry).len(),
            1,
            "the completed recovery action is not left permanently spinning"
        );
    }

    /// While write access is pending, the Session is still usable. A pane that would only
    /// open the user's own shell starts; the one that would run an agent against the
    /// Coming back to a Session starts it.
    ///
    /// Reported four times, and the first three attempts each fixed the wrong thing: a collective
    /// button, then the restore report — which the daemon only sends when it has something to
    /// *explain*, so a Session whose processes had simply ended still came back as a grid of
    /// "Session process stopped / Start pane".
    ///
    /// This is the sweep, over what is actually on screen: a pane marked `Relaunch` whose process
    /// has ended is started, once, without being asked.
    #[test]
    fn a_stopped_pane_that_is_safe_to_start_is_started_without_being_asked() {
        let (mut session, pane_id, node_id) = session_with_agent("Restarting");
        // Marked safe to run again, which is what every built-in template does.
        if let Some(pane) = session.layout.get_mut(&pane_id) {
            pane.restore = turn_core::model::RestoreBehaviour::Relaunch;
        }
        // And its process has ended, which is the state the screenshot was taken in.
        if let Some(node) = session.tree.get_mut(&node_id) {
            node.lifecycle = Lifecycle::Lost;
            node.turn = None;
        }

        let mut desk = Desk::new();
        desk.apply_inbound(connected(), T0);
        desk.apply_inbound(
            answer(Response::Sessions {
                sessions: vec![summary(&session, 0)],
            }),
            T0,
        );
        let reactions = desk.apply_inbound(
            answer(Response::SessionDetails {
                details: Box::new(details(&session)),
            }),
            T0,
        );

        let started: Vec<Request> = sent(&reactions)
            .into_iter()
            .filter(|request| matches!(request, Request::RelaunchNode { .. }))
            .collect();
        assert_eq!(
            started.len(),
            1,
            "the stopped pane must be started without a click: {started:?}"
        );
        assert!(
            matches!(
                &started[0],
                Request::RelaunchNode { node_id: asked, resume: false, .. } if *asked == node_id
            ),
            "and it is that pane's node, started fresh rather than resumed: {started:?}"
        );

        // Asked once. The sweep runs after every message from the daemon, so anything that did
        // not remember having tried would send a relaunch per message for as long as the pane
        // stayed stopped.
        let again = desk.apply_inbound(
            answer(Response::SessionDetails {
                details: Box::new(details(&session)),
            }),
            T0 + 1,
        );
        assert!(
            !sent(&again)
                .iter()
                .any(|request| matches!(request, Request::RelaunchNode { .. })),
            "a pane is started once, not once per message: {:?}",
            sent(&again)
        );
    }

    /// A pane whose command has a consequence keeps its button.
    ///
    /// `ReattachOnly` is the default and now means what it says. This is the half of the original
    /// rule that was worth keeping: Turn does not run somebody's deploy because their window
    /// reopened.
    #[test]
    fn a_pane_that_did_not_say_it_was_safe_is_left_alone() {
        let (mut session, pane_id, node_id) = session_with_agent("Careful");
        if let Some(pane) = session.layout.get_mut(&pane_id) {
            pane.restore = turn_core::model::RestoreBehaviour::ReattachOnly;
            pane.command = Some("npm run deploy".into());
        }
        if let Some(node) = session.tree.get_mut(&node_id) {
            node.lifecycle = Lifecycle::Lost;
        }

        let mut desk = Desk::new();
        desk.apply_inbound(connected(), T0);
        let reactions = desk.apply_inbound(
            answer(Response::SessionDetails {
                details: Box::new(details(&session)),
            }),
            T0,
        );
        assert!(
            !sent(&reactions)
                .iter()
                .any(|request| matches!(request, Request::RelaunchNode { .. })),
            "nothing may run a command the pane did not say was safe to repeat"
        );
    }

    /// shared checkout waits, and says so.
    #[test]
    fn pending_write_access_holds_back_only_the_panes_that_would_use_the_checkout() {
        let (session, _, agent_node) = session_with_agent("Recovering writer");
        let shell_node = NodeId::from_stored("proc_recovering_shell");
        let mut project = Workspace::new("project", "/repo", T0);
        project.id = session.workspace_id.clone();
        let summary = summary(&session, 0);
        let mut lease = turn_core::model::WorkspaceWriteLease::active(
            session.workspace_id.clone(),
            session.id.clone(),
            session.checkout_id.clone(),
            T0,
        );
        lease.state = LeaseState::RecoveryRequired;

        let mut desk = Desk::new();
        desk.apply_inbound(
            answer(Response::Sessions {
                sessions: vec![summary.clone()],
            }),
            T0,
        );
        desk.apply_inbound(
            answer(Response::Hierarchy {
                snapshot: Box::new(HierarchySnapshot {
                    revision: 1,
                    tree_state: TreeSurfaceState::empty("main-window"),
                    workspaces: vec![WorkspaceTreeView {
                        workspace: WorkspaceSummary::from_workspace(
                            &project,
                            std::slice::from_ref(&summary),
                        ),
                        checkouts: Vec::new(),
                        write_lease: Some(lease),
                        sessions: vec![SessionTreeView {
                            session: summary,
                            nodes: TreeNodeView::for_session(&session, T0),
                        }],
                    }],
                }),
            }),
            T0,
        );
        desk.apply_inbound(
            Inbound::Event(Box::new(ServerEvent::RestoreResult {
                session_id: session.id.clone(),
                state: turn_core::model::RestoreState::LayoutOnly,
                needs_explanation: true,
                panes: vec![
                    turn_proto::PaneRestoreOutcome {
                        pane_id: PaneId::from_stored("pane_recovering_agent"),
                        node_id: agent_node.clone(),
                        lifecycle: Lifecycle::Lost,
                        can_relaunch: true,
                        command: Some("claude".into()),
                        auto_start: false,
                        needs_checkout_write: true,
                    },
                    turn_proto::PaneRestoreOutcome {
                        pane_id: PaneId::from_stored("pane_recovering_shell"),
                        node_id: shell_node.clone(),
                        lifecycle: Lifecycle::Lost,
                        can_relaunch: true,
                        command: None,
                        auto_start: false,
                        needs_checkout_write: false,
                    },
                ],
            })),
            T0,
        );

        let shell = desk.apply_view_action(
            ViewAction::RelaunchNode {
                session_id: session.id.clone(),
                node_id: shell_node.clone(),
                resume: false,
            },
            T0 + 1,
        );
        assert!(
            matches!(
                sent(&shell).as_slice(),
                [Request::RelaunchNode { node_id, .. }] if node_id == &shell_node
            ),
            "a terminal writes nothing to the checkout, so it must not wait for write access: {shell:?}"
        );

        let agent = desk.apply_view_action(
            ViewAction::RelaunchNode {
                session_id: session.id.clone(),
                node_id: agent_node,
                resume: false,
            },
            T0 + 2,
        );
        assert!(sent(&agent).is_empty());
        assert!(
            matches!(agent.as_slice(), [Reaction::Notice(message)] if message.contains("write access")),
            "the refusal has to say what is missing: {agent:?}"
        );
    }

    #[test]
    fn resolving_restore_never_attaches_until_the_replacement_layout_arrives() {
        let (session, pane_id, old_node) = session_with_agent("Recovered pane");
        let mut desk = Desk::new();
        desk.apply_inbound(
            answer(Response::Sessions {
                sessions: vec![summary(&session, 0)],
            }),
            T0,
        );
        desk.apply_inbound(
            answer(Response::SessionDetails {
                details: Box::new(details(&session)),
            }),
            T0,
        );
        desk.apply_inbound(
            Inbound::Event(Box::new(ServerEvent::RestoreResult {
                session_id: session.id.clone(),
                state: turn_core::model::RestoreState::LayoutOnly,
                needs_explanation: true,
                panes: vec![turn_proto::PaneRestoreOutcome {
                    pane_id: pane_id.clone(),
                    node_id: old_node,
                    lifecycle: Lifecycle::Lost,
                    can_relaunch: true,
                    command: Some("claude".into()),
                    auto_start: false,
                    needs_checkout_write: true,
                }],
            })),
            T0,
        );
        let resolved = desk.apply_inbound(
            Inbound::Event(Box::new(ServerEvent::RestoreResult {
                session_id: session.id.clone(),
                state: turn_core::model::RestoreState::Live,
                needs_explanation: false,
                panes: Vec::new(),
            })),
            T0 + 1,
        );
        assert!(
            sent(&resolved).is_empty(),
            "a recovery tombstone alone must never attach the old Layout"
        );

        let replacement_node = NodeId::from_stored("proc_replacement");
        let mut replacement = session.layout.clone();
        replacement.get_mut(&pane_id).unwrap().node_id = Some(replacement_node);
        let layout = desk.apply_inbound(
            Inbound::Event(Box::new(ServerEvent::LayoutChanged {
                session_id: session.id.clone(),
                layout: replacement,
            })),
            T0 + 2,
        );
        assert!(matches!(
            sent(&layout).as_slice(),
            [Request::AttachPane {
                session_id,
                pane_id: requested,
                ..
            }] if session_id == &session.id && requested == &pane_id
        ));
    }

    #[test]
    fn a_rebound_pane_discards_its_old_feed_and_ignores_a_late_attachment() {
        let (session, pane_id, old_node) = session_with_agent("Rebound pane");
        let mut desk = Desk::new();
        desk.apply_inbound(
            answer(Response::Sessions {
                sessions: vec![summary(&session, 0)],
            }),
            T0,
        );
        desk.apply_inbound(
            answer(Response::SessionDetails {
                details: Box::new(details(&session)),
            }),
            T0,
        );
        desk.apply_inbound(
            attached(
                &pane_id,
                &session.id,
                &old_node,
                Grid::from_lines(&["old screen"], 20),
                1,
            ),
            T0,
        );
        desk.refresh_screens();
        assert_eq!(desk.view(T0).panes.len(), 1);

        let new_node = NodeId::from_stored("proc_new_binding");
        let mut replacement = session.layout.clone();
        replacement.get_mut(&pane_id).unwrap().node_id = Some(new_node.clone());
        let reactions = desk.apply_inbound(
            Inbound::Event(Box::new(ServerEvent::LayoutChanged {
                session_id: session.id.clone(),
                layout: replacement,
            })),
            T0 + 1,
        );
        assert!(sent(&reactions)
            .iter()
            .any(|request| matches!(request, Request::AttachPane { pane_id: requested, .. } if requested == &pane_id)));
        desk.refresh_screens();
        assert!(desk.view(T0 + 1).panes.is_empty());

        desk.apply_inbound(
            attached(
                &pane_id,
                &session.id,
                &old_node,
                Grid::from_lines(&["late old screen"], 20),
                2,
            ),
            T0 + 2,
        );
        desk.refresh_screens();
        assert!(
            desk.view(T0 + 2).panes.is_empty(),
            "a late response for the retired node must not repopulate the Pane"
        );

        desk.apply_inbound(
            attached(
                &pane_id,
                &session.id,
                &new_node,
                Grid::from_lines(&["new screen"], 20),
                1,
            ),
            T0 + 3,
        );
        desk.refresh_screens();
        assert_eq!(desk.view(T0 + 3).panes.len(), 1);
    }

    #[test]
    fn whole_session_and_workspace_stops_name_the_destructive_disposition() {
        let (session, _, _) = session_with_agent("Stop me");
        let mut desk = Desk::new();
        assert!(matches!(
            sent(&desk.apply_view_action(
                ViewAction::CloseSession {
                    session_id: session.id.clone(),
                    disposition: CloseDisposition::Terminate,
                },
                T0,
            ))
            .as_slice(),
            [Request::CloseSession {
                session_id,
                disposition: CloseDisposition::Terminate,
            }] if session_id == &session.id
        ));
        assert!(matches!(
            sent(&desk.apply_view_action(
                ViewAction::CloseWorkspace {
                    workspace_id: session.workspace_id.clone(),
                    disposition: CloseDisposition::Terminate,
                },
                T0,
            ))
            .as_slice(),
            [Request::CloseWorkspace {
                workspace_id,
                disposition: CloseDisposition::Terminate,
            }] if workspace_id == &session.workspace_id
        ));

        assert!(matches!(
            sent(&desk.apply_view_action(ViewAction::SetArchivedVisibility { include: true }, T0,))
                .as_slice(),
            [Request::GetHierarchy {
                include_archived: true,
                ..
            }]
        ));
        assert!(matches!(
            sent(&desk.apply_view_action(
                ViewAction::ArchiveWorkspace {
                    workspace_id: session.workspace_id.clone(),
                    archived: false,
                },
                T0,
            ))
            .as_slice(),
            [Request::ArchiveWorkspace {
                workspace_id,
                archived: false,
            }] if workspace_id == &session.workspace_id
        ));
    }

    /// The banner shows one demand and must act on that one, not on whichever the queue
    /// happens to rank first by the time the button is pressed.
    #[test]
    fn going_to_a_demand_names_the_demand_that_was_shown() {
        let mut desk = Desk::new();
        let shown = AttentionId::new();
        let reactions = desk.apply_view_action(ViewAction::GotoAttention(shown.clone()), T0);
        match sent(&reactions).as_slice() {
            [Request::GotoAttention { attention_id }] => {
                assert_eq!(attention_id.as_ref(), Some(&shown));
            }
            other => panic!("got {other:?}"),
        }

        // The shortcut is the other case: no id, because the daemon owns the order and
        // pressing the shortcut is consent to go wherever it says.
        match sent(&desk.dispatch(Command::NextAttention, T0)).as_slice() {
            [Request::GotoAttention { attention_id }] => assert_eq!(*attention_id, None),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn queue_triage_actions_keep_the_exact_demand_session_and_deadline() {
        let mut desk = Desk::new();
        let attention_id = AttentionId::new();
        let session_id = SessionId::from_stored("sess_queue_triage");
        let until_ms = T0 + 10 * 60 * 1_000;

        match sent(&desk.apply_view_action(
            ViewAction::SnoozeAttention {
                attention_id: attention_id.clone(),
                until_ms,
            },
            T0,
        ))
        .as_slice()
        {
            [Request::SnoozeAttention {
                attention_id: sent_id,
                until_ms: sent_until,
            }] => {
                assert_eq!(sent_id, &attention_id);
                assert_eq!(*sent_until, until_ms);
            }
            other => panic!("got {other:?}"),
        }

        match sent(&desk.apply_view_action(
            ViewAction::MuteAttentionSession {
                session_id: session_id.clone(),
                until_ms: Some(until_ms),
            },
            T0,
        ))
        .as_slice()
        {
            [Request::MuteSession {
                session_id: sent_session,
                until_ms: Some(sent_until),
            }] => {
                assert_eq!(sent_session, &session_id);
                assert_eq!(*sent_until, until_ms);
            }
            other => panic!("got {other:?}"),
        }

        let node_id = NodeId::from_stored("agent_reviewer_triage");
        match sent(&desk.apply_view_action(
            ViewAction::TerminateNode {
                session_id: session_id.clone(),
                node_id: node_id.clone(),
            },
            T0,
        ))
        .as_slice()
        {
            [Request::TerminateNode {
                session_id: sent_session,
                node_id: sent_node,
            }] => {
                assert_eq!(sent_session, &session_id);
                assert_eq!(sent_node, &node_id);
            }
            other => panic!("got {other:?}"),
        }
    }

    /// The banner is built from the daemon's queue order and the daemon's own permission
    /// detail — including the directory, which is the field that stops somebody
    /// approving a command in the wrong repository.
    #[test]
    fn the_permission_banner_shows_the_daemons_own_detail_verbatim() {
        let (mut session, _, node_id) = session_with_agent("Fix climbing bugs");
        if let Some(node) = session.tree.get_mut(&node_id) {
            node.turn = Some(Turn::AwaitingUser {
                reason: AwaitingReason::Permission,
            });
            if let Some(agent) = node.agent.as_mut() {
                agent.pending_permission = Some(CorePermission {
                    summary: "run rm -rf build".into(),
                    command: Some("rm -rf build".into()),
                    tool_name: Some("Bash".into()),
                    risk: Risk::High,
                    requested_ms: T0,
                    cwd: Some("/repo/space-troopers".into()),
                });
            }
        }

        let entry = AttentionEntry {
            id: AttentionId::new(),
            session_id: session.id.clone(),
            node_id: Some(node_id),
            parent_node_id: None,
            subject_external_id: None,
            reason: AwaitingReason::Permission,
            summary: Some("run rm -rf build".into()),
            confidence: Confidence::Explicit,
            created_ms: T0,
            updated_ms: T0,
            state: EntryState::Pending,
            priority_boost: 0,
            survives_owner_exit: false,
            demand_kind: Default::default(),
        };

        let mut desk = Desk::new();
        desk.apply_inbound(
            answer(Response::Sessions {
                sessions: vec![summary(&session, 1)],
            }),
            T0,
        );
        desk.apply_inbound(
            answer(Response::AttentionList {
                entries: vec![AttentionView::from_entry(&entry, "Fix climbing bugs", T0)],
            }),
            T0,
        );

        let view = desk.view(T0 + 47_000);
        let banner = view.permission.expect("the banner is shown");
        assert_eq!(banner.session, "Fix climbing bugs");
        assert_eq!(banner.command.as_deref(), Some("rm -rf build"));
        assert_eq!(
            banner.cwd, "/repo/space-troopers",
            "the directory is shown verbatim"
        );
        assert_eq!(banner.risk, Risk::High);
        assert_eq!(banner.blocked_secs, 47);
        assert_eq!(
            banner.attention_id,
            Some(entry.id),
            "the banner must carry the id of what it is showing"
        );
        assert!(!banner.provisional);
    }

    #[test]
    fn a_subagent_permission_banner_never_borrows_the_primary_agents_command() {
        let (mut session, primary_id) = session_with_primary_permission("Review in background");

        let mut reviewer = ProcessNode::agent(
            session.id.clone(),
            "claude reviewer",
            "/repo/review",
            T0 + 1,
        );
        reviewer.kind = NodeKind::Subagent;
        reviewer.title = "Reviewer".into();
        reviewer.lifecycle = Lifecycle::Alive;
        reviewer.turn = Some(Turn::AwaitingUser {
            reason: AwaitingReason::Permission,
        });
        reviewer.link_to(primary_id, Relation::Confirmed);
        reviewer.agent.as_mut().unwrap().pending_permission = Some(CorePermission {
            summary: "reviewer command".into(),
            command: Some("git diff --check".into()),
            tool_name: Some("Bash".into()),
            risk: Risk::Low,
            requested_ms: T0 + 1,
            cwd: Some("/repo/review".into()),
        });
        let reviewer_id = session.tree.insert(reviewer);

        let entry = AttentionEntry {
            id: AttentionId::new(),
            session_id: session.id.clone(),
            node_id: Some(reviewer_id),
            parent_node_id: None,
            subject_external_id: None,
            reason: AwaitingReason::Permission,
            summary: Some("reviewer command".into()),
            confidence: Confidence::Explicit,
            created_ms: T0 + 1,
            updated_ms: T0 + 1,
            state: EntryState::Pending,
            priority_boost: 0,
            survives_owner_exit: false,
            demand_kind: Default::default(),
        };
        let mut desk = Desk::new();
        desk.sessions.push(summary(&session, 1));
        desk.trees.insert(
            session.id.clone(),
            TreeNodeView::for_session(&session, T0 + 2),
        );
        desk.queue
            .push(AttentionView::from_entry(&entry, &session.name, T0 + 2));

        let banner = desk.view(T0 + 2).permission.expect("permission banner");
        assert_eq!(banner.summary, "reviewer command");
        assert_eq!(banner.command.as_deref(), Some("git diff --check"));
        assert_eq!(banner.cwd, "/repo/review");
        assert_eq!(banner.risk, Risk::Low);
    }

    #[test]
    fn a_scoped_node_less_permission_does_not_borrow_primary_agent_details() {
        let (session, primary_id) = session_with_primary_permission("Review in background");
        for (parent_node_id, subject_external_id) in [
            (Some(primary_id.clone()), None),
            (None, Some("worker-reviewer".to_owned())),
        ] {
            let entry = AttentionEntry {
                id: AttentionId::new(),
                session_id: session.id.clone(),
                node_id: None,
                parent_node_id,
                subject_external_id,
                reason: AwaitingReason::Permission,
                summary: Some("reviewer permission".into()),
                confidence: Confidence::Explicit,
                created_ms: T0 + 1,
                updated_ms: T0 + 1,
                state: EntryState::Pending,
                priority_boost: 0,
                survives_owner_exit: false,
                demand_kind: Default::default(),
            };
            let mut desk = Desk::new();
            desk.sessions.push(summary(&session, 1));
            desk.queue
                .push(AttentionView::from_entry(&entry, &session.name, T0 + 2));

            assert!(
                desk.view(T0 + 2).permission.is_none(),
                "an unresolved modern scope must not borrow the primary Agent's permission"
            );
        }
    }

    #[test]
    fn a_stale_exact_permission_node_does_not_borrow_primary_agent_details() {
        let (session, _) = session_with_primary_permission("Review in background");
        let entry = AttentionEntry {
            id: AttentionId::new(),
            session_id: session.id.clone(),
            node_id: Some(NodeId::from_stored("agent_missing_permission")),
            parent_node_id: None,
            subject_external_id: None,
            reason: AwaitingReason::Permission,
            summary: Some("stale worker permission".into()),
            confidence: Confidence::Explicit,
            created_ms: T0 + 1,
            updated_ms: T0 + 1,
            state: EntryState::Pending,
            priority_boost: 0,
            survives_owner_exit: false,
            demand_kind: Default::default(),
        };
        let mut desk = Desk::new();
        desk.sessions.push(summary(&session, 1));
        desk.queue
            .push(AttentionView::from_entry(&entry, &session.name, T0 + 2));

        assert!(
            desk.view(T0 + 2).permission.is_none(),
            "a stale exact identity must not fall back to the primary Agent"
        );
    }

    #[test]
    fn a_fully_unscoped_legacy_permission_still_uses_the_primary_agent() {
        let (session, _) = session_with_primary_permission("Legacy daemon");
        let entry = AttentionEntry {
            id: AttentionId::new(),
            session_id: session.id.clone(),
            node_id: None,
            parent_node_id: None,
            subject_external_id: None,
            reason: AwaitingReason::Permission,
            summary: Some("legacy permission".into()),
            confidence: Confidence::Explicit,
            created_ms: T0 + 1,
            updated_ms: T0 + 1,
            state: EntryState::Pending,
            priority_boost: 0,
            survives_owner_exit: false,
            demand_kind: Default::default(),
        };
        let mut desk = Desk::new();
        desk.sessions.push(summary(&session, 1));
        desk.queue
            .push(AttentionView::from_entry(&entry, &session.name, T0 + 2));

        let banner = desk
            .view(T0 + 2)
            .permission
            .expect("legacy unscoped permission banner");
        assert_eq!(banner.summary, "primary command");
        assert_eq!(banner.command.as_deref(), Some("make deploy"));
        assert_eq!(banner.cwd, "/repo/main");
        assert_eq!(banner.risk, Risk::High);
    }

    /// A demand that came from a heuristic has to be drawn as a guess.
    #[test]
    fn an_inferred_demand_is_marked_provisional_in_the_queue() {
        let (session, _, _) = session_with_agent("Guessy");
        let entry = AttentionEntry {
            id: AttentionId::new(),
            session_id: session.id.clone(),
            node_id: None,
            parent_node_id: None,
            subject_external_id: None,
            reason: AwaitingReason::Input,
            summary: Some("looks like a prompt".into()),
            confidence: Confidence::InferredHigh,
            created_ms: T0,
            updated_ms: T0,
            state: EntryState::Pending,
            priority_boost: 0,
            survives_owner_exit: false,
            demand_kind: Default::default(),
        };
        let mut desk = Desk::new();
        desk.apply_inbound(
            answer(Response::Sessions {
                sessions: vec![summary(&session, 1)],
            }),
            T0,
        );
        desk.apply_inbound(
            answer(Response::AttentionList {
                entries: vec![AttentionView::from_entry(&entry, "Guessy", T0)],
            }),
            T0,
        );
        let view = desk.view(T0);
        assert!(view.queue[0].provisional);
        assert!(
            view.permission.is_none(),
            "an input demand is not a permission and must not fill the permission banner"
        );
    }

    #[test]
    fn a_dragged_divider_becomes_a_fraction_of_the_parent_split() {
        let (session, pane_id, _) = session_with_agent("Resizable");
        let after = PaneId::from_stored("pane_resize_after");
        let mut desk = Desk::new();
        desk.apply_inbound(
            answer(Response::Sessions {
                sessions: vec![summary(&session, 0)],
            }),
            T0,
        );
        match sent(&desk.apply_view_action(
            ViewAction::ResizeDivider {
                before: pane_id.clone(),
                after: after.clone(),
                fraction: 0.125,
            },
            T0,
        ))
        .as_slice()
        {
            [Request::ResizeDivider {
                before: named,
                after: named_after,
                delta,
                ..
            }] => {
                assert_eq!(named, &pane_id);
                assert_eq!(named_after, &after);
                assert!((delta - 0.125).abs() < f32::EPSILON);
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn a_split_asks_the_daemon_and_renders_whatever_it_answers() {
        let (session, pane_id, _) = session_with_agent("Splitting");
        let mut desk = Desk::new();
        desk.apply_inbound(
            answer(Response::Sessions {
                sessions: vec![summary(&session, 0)],
            }),
            T0,
        );
        desk.apply_inbound(
            answer(Response::SessionDetails {
                details: Box::new(details(&session)),
            }),
            T0,
        );
        match sent(&desk.dispatch(Command::SplitVertical, T0)).as_slice() {
            [Request::SplitPane {
                direction,
                pane_id: named,
                ..
            }] => {
                assert_eq!(*direction, Direction::Vertical);
                assert_eq!(named, &pane_id);
            }
            other => panic!("got {other:?}"),
        }
    }

    /// A layout the daemon sends is what gets drawn, and panes that went away take their
    /// screens with them rather than being held for the life of the window.
    #[test]
    fn a_layout_push_replaces_the_arrangement_and_forgets_closed_panes() {
        let (session, pane_id, node_id) = session_with_agent("Rearranged");
        let mut desk = Desk::new();
        desk.apply_inbound(
            answer(Response::Sessions {
                sessions: vec![summary(&session, 0)],
            }),
            T0,
        );
        desk.apply_inbound(
            answer(Response::SessionDetails {
                details: Box::new(details(&session)),
            }),
            T0,
        );
        desk.apply_inbound(
            attached(&pane_id, &session.id, &node_id, Grid::blank(24, 80), 1),
            T0,
        );
        desk.refresh_screens();
        assert_eq!(desk.view(T0).panes.len(), 1);

        // A layout with a different pane entirely: the old screen is dropped and the new
        // pane is attached to.
        let replacement = Layout::single(Pane::new(PaneKind::Shell));
        let reactions = desk.apply_inbound(
            Inbound::Event(Box::new(ServerEvent::LayoutChanged {
                session_id: session.id.clone(),
                layout: replacement,
            })),
            T0,
        );
        desk.refresh_screens();
        assert!(
            desk.view(T0).panes.is_empty(),
            "the closed pane's screen must not be kept"
        );
        assert!(sent(&reactions)
            .iter()
            .any(|request| matches!(request, Request::AttachPane { .. })));
    }

    #[test]
    fn a_failed_request_is_reported_with_what_the_window_was_doing() {
        let mut desk = Desk::new();
        let reactions = desk.apply_inbound(
            Inbound::Failed {
                ask: Ask::Action("closing the pane"),
                error: turn_proto::ProtoError::new(
                    turn_proto::ErrorCode::Conflict,
                    "the last pane cannot be closed",
                ),
            },
            T0,
        );
        match reactions.as_slice() {
            [Reaction::Notice(message)] => {
                assert!(message.contains("closing the pane"), "got {message}");
                assert!(message.contains("last pane"), "got {message}");
            }
            other => panic!("got {other:?}"),
        }
        assert!(desk.view(T0).notice.is_some());
    }

    #[test]
    fn a_real_handshake_clears_only_the_companion_diagnostic() {
        let mut desk = Desk::new();
        desk.show_companion_notice("turnd failed while starting");
        desk.apply_inbound(
            Inbound::Status(ConnectionState::Connecting { attempt: 2 }),
            T0,
        );
        assert_eq!(
            desk.view(T0).notice.as_deref(),
            Some("turnd failed while starting")
        );

        desk.apply_inbound(connected(), T0);
        assert!(desk.view(T0).notice.is_none());

        desk.apply_inbound(
            Inbound::Notice(turn_proto::ProtoError::new(
                turn_proto::ErrorCode::Conflict,
                "a request still failed",
            )),
            T0,
        );
        desk.show_companion_notice("a later companion warning");
        desk.apply_inbound(connected(), T0);
        assert_eq!(
            desk.view(T0).notice.as_deref(),
            Some("a request still failed")
        );
    }

    #[test]
    fn a_write_lease_conflict_waits_for_an_explicit_safe_alternative() {
        let (mut owner, _, _) = session_with_agent("Fix climbing bugs");
        let checkout = turn_core::ids::CheckoutId::primary_for(&owner.workspace_id);
        owner.mode = turn_core::model::SessionMode::MainCheckout;
        owner.checkout_id = checkout.clone();

        let mut desk = Desk::new();
        desk.apply_inbound(
            answer(Response::Sessions {
                sessions: vec![summary(&owner, 0)],
            }),
            T0,
        );
        let coding = Template::coding(T0);
        let coding_id = coding.id.clone();
        desk.apply_inbound(
            answer(Response::Templates {
                templates: vec![TemplateSummary::from_template(&coding)],
            }),
            T0,
        );
        let requested = sent(&desk.dispatch(Command::QuickNewSession, T0));
        assert!(matches!(
            requested.as_slice(),
            [Request::CreateSessionFromTemplate { .. }]
        ));

        let lease = turn_core::model::WorkspaceWriteLease::active(
            owner.workspace_id.clone(),
            owner.id.clone(),
            checkout.clone(),
            T0,
        );
        let error = turn_proto::ProtoError::workspace_write_lease_conflict(
            ProtoErrorContext::WorkspaceWriteLeaseConflict {
                workspace_id: owner.workspace_id.clone(),
                checkout_id: checkout,
                requesting_session_id: None,
                lease: Box::new(lease),
                owner: Box::new(turn_proto::WriteLeaseOwnerView {
                    session_id: owner.id.clone(),
                    session_name: owner.name.clone(),
                    mode: owner.mode,
                    cwd: owner.cwd.clone(),
                    branch: owner.git_branch.clone(),
                    last_activity_ms: owner.last_activity_ms,
                }),
                alternatives: vec![
                    SessionConflictAlternative::FocusOwner,
                    SessionConflictAlternative::CreateReadOnly,
                    SessionConflictAlternative::CreateIsolatedWorktree,
                    SessionConflictAlternative::Cancel,
                ],
            },
        );
        desk.apply_inbound(
            Inbound::Failed {
                ask: Ask::CreateSession {
                    workspace_id: owner.workspace_id.clone(),
                },
                error,
            },
            T0,
        );
        assert!(desk.write_conflict().is_some());
        let safe =
            sent(&desk.resolve_write_conflict(SessionConflictAlternative::CreateReadOnly, T0));
        assert!(
            matches!(
                safe.as_slice(),
                [Request::CreateReadOnlySessionFromTemplate {
                    template_id,
                    name: Some(name),
                    cwd: None,
                    branch: None,
                    task: None,
                    ..
                }] if template_id == &coding_id && name == "Session 2"
            ),
            "the exact Template intent must reach the safe daemon API: {safe:?}"
        );
        assert!(desk.write_conflict().is_none());
    }

    #[test]
    fn a_template_conflict_keeps_daemon_owned_defaults_for_an_isolated_retry() {
        let (mut owner, _, _) = session_with_agent("Fix climbing bugs");
        let checkout = turn_core::ids::CheckoutId::primary_for(&owner.workspace_id);
        owner.mode = turn_core::model::SessionMode::MainCheckout;
        owner.checkout_id = checkout.clone();
        let coding = Template::coding(T0);
        let coding_id = coding.id.clone();

        let mut desk = Desk::new();
        desk.apply_inbound(
            answer(Response::Sessions {
                sessions: vec![summary(&owner, 0)],
            }),
            T0,
        );
        desk.apply_inbound(
            answer(Response::Templates {
                templates: vec![TemplateSummary::from_template(&coding)],
            }),
            T0,
        );
        let requested = sent(&desk.apply_view_action(
            ViewAction::CreateSessionFromTemplate {
                workspace_id: owner.workspace_id.clone(),
                template_id: coding_id.clone(),
                name: "Alternative movement approach".into(),
                task: Some("Try the movement rewrite".into()),
            },
            T0,
        ));
        assert!(matches!(
            requested.as_slice(),
            [Request::CreateSessionFromTemplate {
                name: Some(name),
                task: Some(task),
                ..
            }] if name == "Alternative movement approach" && task == "Try the movement rewrite"
        ));

        let lease = turn_core::model::WorkspaceWriteLease::active(
            owner.workspace_id.clone(),
            owner.id.clone(),
            checkout.clone(),
            T0,
        );
        let error = turn_proto::ProtoError::workspace_write_lease_conflict(
            ProtoErrorContext::WorkspaceWriteLeaseConflict {
                workspace_id: owner.workspace_id.clone(),
                checkout_id: checkout,
                requesting_session_id: None,
                lease: Box::new(lease),
                owner: Box::new(turn_proto::WriteLeaseOwnerView {
                    session_id: owner.id.clone(),
                    session_name: owner.name.clone(),
                    mode: owner.mode,
                    cwd: owner.cwd.clone(),
                    branch: owner.git_branch.clone(),
                    last_activity_ms: owner.last_activity_ms,
                }),
                alternatives: vec![SessionConflictAlternative::CreateIsolatedWorktree],
            },
        );
        desk.apply_inbound(
            Inbound::Failed {
                ask: Ask::CreateSession {
                    workspace_id: owner.workspace_id.clone(),
                },
                error,
            },
            T0,
        );

        let safe = sent(
            &desk.resolve_write_conflict(SessionConflictAlternative::CreateIsolatedWorktree, T0),
        );
        assert!(
            matches!(
                safe.as_slice(),
                [Request::CreateWorktreeSessionFromTemplate {
                    template_id,
                    name: Some(name),
                    cwd: None,
                    template_branch: None,
                    task: Some(task),
                    branch,
                    worktree_path: None,
                    ..
                }] if template_id == &coding_id
                    && name == "Alternative movement approach"
                    && task == "Try the movement rewrite"
                    && branch.starts_with("turn/alternative-movement-approac-")
            ),
            "the GUI must not rebuild Coding from TemplateSummary: {safe:?}"
        );
    }

    #[test]
    fn first_run_workspace_creation_is_a_typed_user_action() {
        let mut desk = Desk::new();
        desk.show_notice("an earlier failure");
        let requests = sent(&desk.apply_view_action(
            ViewAction::CreateWorkspace {
                name: "turn".into(),
                root: "/repo/turn".into(),
                continue_to_session: false,
            },
            T0,
        ));
        assert_eq!(
            requests,
            vec![Request::CreateWorkspace {
                name: "turn".into(),
                root: "/repo/turn".into(),
            }]
        );
        assert!(
            desk.view(T0).notice.is_none(),
            "retrying must clear a stale creation failure"
        );
    }

    #[test]
    fn the_created_workspace_is_added_selected_and_can_continue_to_a_session() {
        let workspace = Workspace::new("turn", "/repo/turn", T0);
        let workspace_id = workspace.id.clone();
        let mut desk = Desk::new();

        let reactions = desk.apply_inbound(
            Inbound::Answer {
                ask: Ask::CreateWorkspace {
                    continue_to_session: true,
                },
                response: Box::new(Response::Workspace {
                    workspace: WorkspaceSummary::from_workspace(&workspace, &[]),
                }),
            },
            T0,
        );

        assert!(desk.has_workspaces());
        assert!(reactions.iter().any(|reaction| matches!(
            reaction,
            Reaction::WorkspaceCreated {
                workspace_id: created,
                continue_to_session: true,
            } if created == &workspace_id
        )));
        assert!(reactions.iter().any(|reaction| matches!(
            reaction,
            Reaction::Send {
                request: Request::SelectTreeNode {
                    selected: Some(HierarchyKey::Workspace { workspace_id: selected }),
                    ..
                },
                ..
            } if selected == &workspace_id
        )));
    }

    #[test]
    fn the_visible_optimistic_workspace_is_the_target_for_new_and_quick_sessions() {
        let first = Workspace::new("first", "/repo/first", T0);
        let first_id = first.id.clone();
        let second = Workspace::new("second", "/repo/second", T0 + 1);
        let second_id = second.id.clone();
        let mut desk = Desk::new();
        desk.apply_inbound(
            answer(Response::Hierarchy {
                snapshot: Box::new(HierarchySnapshot {
                    revision: 1,
                    tree_state: TreeSurfaceState {
                        surface_id: "main-window".into(),
                        // The daemon still acknowledges A while the immediate-mode tree
                        // already paints B as selected.
                        selected: Some(HierarchyKey::workspace(first_id)),
                        expanded: Vec::new(),
                    },
                    workspaces: vec![
                        WorkspaceTreeView {
                            workspace: WorkspaceSummary::from_workspace(&first, &[]),
                            checkouts: Vec::new(),
                            write_lease: None,
                            sessions: Vec::new(),
                        },
                        WorkspaceTreeView {
                            workspace: WorkspaceSummary::from_workspace(&second, &[]),
                            checkouts: Vec::new(),
                            write_lease: None,
                            sessions: Vec::new(),
                        },
                    ],
                }),
            }),
            T0,
        );
        desk.set_navigation_hint(Some(HierarchyKey::workspace(second_id.clone())));
        let coding = TemplateSummary::from_template(&Template::coding(T0));
        desk.apply_inbound(
            answer(Response::Templates {
                templates: vec![coding],
            }),
            T0,
        );

        assert_eq!(desk.new_session_draft().unwrap().workspace_id, second_id);
        assert!(matches!(
            sent(&desk.dispatch(Command::QuickNewSession, T0)).as_slice(),
            [Request::CreateSessionFromTemplate { workspace_id, .. }]
                if workspace_id == &second_id
        ));
    }

    #[test]
    fn overlapping_creations_cannot_replace_the_pending_session_intent() {
        let (mut owner, _, _) = session_with_agent("Fix climbing bugs");
        let checkout = turn_core::ids::CheckoutId::primary_for(&owner.workspace_id);
        owner.mode = turn_core::model::SessionMode::MainCheckout;
        owner.checkout_id = checkout.clone();
        let coding = Template::coding(T0);
        let coding_id = coding.id.clone();

        let mut desk = Desk::new();
        desk.apply_inbound(
            answer(Response::Sessions {
                sessions: vec![summary(&owner, 0)],
            }),
            T0,
        );
        desk.apply_inbound(
            answer(Response::Templates {
                templates: vec![TemplateSummary::from_template(&coding)],
            }),
            T0,
        );

        let first = sent(&desk.apply_view_action(
            ViewAction::CreateSessionFromTemplate {
                workspace_id: owner.workspace_id.clone(),
                template_id: coding_id.clone(),
                name: "Original task".into(),
                task: Some("Keep this exact intent".into()),
            },
            T0,
        ));
        assert!(matches!(
            first.as_slice(),
            [Request::CreateSessionFromTemplate { name: Some(name), .. }]
                if name == "Original task"
        ));

        let other_workspace = WorkspaceId::from_stored("ws_other00001");
        let rejected = desk.apply_view_action(
            ViewAction::CreateSessionFromTemplate {
                workspace_id: other_workspace,
                template_id: turn_core::ids::TemplateId::from_stored("tpl_other0001"),
                name: "Replacement task".into(),
                task: Some("must not replace the first draft".into()),
            },
            T0 + 1,
        );
        assert!(sent(&rejected).is_empty());
        assert!(rejected
            .iter()
            .any(|reaction| matches!(reaction, Reaction::Notice(_))));

        let lease = turn_core::model::WorkspaceWriteLease::active(
            owner.workspace_id.clone(),
            owner.id.clone(),
            checkout.clone(),
            T0,
        );
        desk.apply_inbound(
            Inbound::Failed {
                ask: Ask::CreateSession {
                    workspace_id: owner.workspace_id.clone(),
                },
                error: turn_proto::ProtoError::workspace_write_lease_conflict(
                    ProtoErrorContext::WorkspaceWriteLeaseConflict {
                        workspace_id: owner.workspace_id.clone(),
                        checkout_id: checkout,
                        requesting_session_id: None,
                        lease: Box::new(lease),
                        owner: Box::new(turn_proto::WriteLeaseOwnerView {
                            session_id: owner.id.clone(),
                            session_name: owner.name.clone(),
                            mode: owner.mode,
                            cwd: owner.cwd.clone(),
                            branch: owner.git_branch.clone(),
                            last_activity_ms: owner.last_activity_ms,
                        }),
                        alternatives: vec![SessionConflictAlternative::CreateIsolatedWorktree],
                    },
                ),
            },
            T0 + 2,
        );
        assert!(desk.write_conflict().is_some());

        // A response to a different Session operation may update the list, but cannot
        // consume this creation lifecycle.
        desk.apply_inbound(
            Inbound::Answer {
                ask: Ask::Action("renaming a session"),
                response: Box::new(Response::Session {
                    session: Box::new(summary(&owner, 0)),
                }),
            },
            T0 + 3,
        );
        assert!(desk.write_conflict().is_some());

        let retry = sent(
            &desk
                .resolve_write_conflict(SessionConflictAlternative::CreateIsolatedWorktree, T0 + 4),
        );
        assert!(matches!(
            retry.as_slice(),
            [Request::CreateWorktreeSessionFromTemplate {
                workspace_id,
                template_id,
                name: Some(name),
                task: Some(task),
                ..
            }] if workspace_id == &owner.workspace_id
                && template_id == &coding_id
                && name == "Original task"
                && task == "Keep this exact intent"
        ));
    }

    #[test]
    fn reconnecting_releases_a_creation_sheet_with_a_retryable_error() {
        let mut desk = Desk::new();
        let requests = sent(&desk.apply_view_action(
            ViewAction::CreateWorkspace {
                name: "turn".into(),
                root: "/repo/turn".into(),
                continue_to_session: true,
            },
            T0,
        ));
        assert_eq!(requests.len(), 1);
        assert!(desk.creation_in_progress());

        let reactions = desk.apply_inbound(connected(), T0 + 1);
        assert!(!desk.creation_in_progress());
        assert!(reactions.iter().any(|reaction| matches!(
            reaction,
            Reaction::WorkspaceCreationFailed(message)
                if message.contains("reconnected") && message.contains("try again")
        )));
    }

    #[test]
    fn a_created_session_becomes_the_active_session_and_is_loaded() {
        let (session, _, _) = session_with_agent("Fix startup");
        let session_id = session.id.clone();
        let workspace_id = session.workspace_id.clone();
        let mut desk = Desk::new();

        let reactions = desk.apply_inbound(
            Inbound::Answer {
                ask: Ask::CreateSession {
                    workspace_id: workspace_id.clone(),
                },
                response: Box::new(Response::Session {
                    session: Box::new(summary(&session, 0)),
                }),
            },
            T0,
        );

        assert_eq!(desk.selected(), Some(&session_id));
        assert!(reactions.iter().any(|reaction| matches!(
            reaction,
            Reaction::Send {
                request: Request::GetSession { session_id: requested },
                ..
            } if requested == &session_id
        )));
        assert!(reactions.iter().any(|reaction| matches!(
            reaction,
            Reaction::SessionCreated { session_id: created } if created == &session_id
        )));
        assert!(reactions.iter().any(|reaction| matches!(
            reaction,
            Reaction::Send {
                request: Request::SetTreeExpanded {
                    key: HierarchyKey::Workspace { workspace_id: expanded },
                    expanded: true,
                    ..
                },
                ..
            } if expanded == &workspace_id
        )));
    }

    #[test]
    fn a_delayed_full_hierarchy_snapshot_cannot_rewind_navigation() {
        let mut desk = Desk::new();
        desk.apply_inbound(
            answer(Response::Hierarchy {
                snapshot: Box::new(HierarchySnapshot::empty("main-window", 8)),
            }),
            T0,
        );
        desk.apply_inbound(
            Inbound::Event(Box::new(ServerEvent::HierarchyChanged {
                snapshot: Box::new(HierarchySnapshot::empty("main-window", 7)),
            })),
            T0 + 1,
        );
        assert_eq!(desk.hierarchy().unwrap().revision, 8);

        // A response at the same revision may carry this surface's newer persisted
        // selection/expansion state, so resync answers are still accepted.
        let mut same = HierarchySnapshot::empty("main-window", 8);
        same.tree_state.selected = Some(HierarchyKey::workspace(workspace()));
        desk.apply_inbound(
            answer(Response::Hierarchy {
                snapshot: Box::new(same),
            }),
            T0 + 2,
        );
        assert!(desk.hierarchy().unwrap().tree_state.selected.is_some());
    }

    #[test]
    fn quick_new_uses_the_first_available_preset_without_a_hidden_coding_fallback() {
        let workspace = Workspace::new("turn", "/repo/turn", T0);
        let workspace_id = workspace.id.clone();
        let starter = TemplateSummary::from_template(&Template::two_shells(T0));
        let starter_id = starter.id.clone();
        let coding = TemplateSummary::from_template(&Template::coding(T0));
        let mut desk = Desk::new();
        desk.workspaces = vec![WorkspaceSummary::from_workspace(&workspace, &[])];
        desk.templates = vec![starter, coding];

        assert!(matches!(
            sent(&desk.dispatch(Command::QuickNewSession, T0)).as_slice(),
            [Request::CreateSessionFromTemplate {
                workspace_id: requested_workspace,
                template_id,
                ..
            }] if requested_workspace == &workspace_id && template_id == &starter_id
        ));
    }

    #[test]
    fn releasing_a_write_lease_keeps_its_fencing_generation() {
        let mut desk = Desk::new();
        let workspace_id = workspace();
        let lease_id = turn_core::ids::LeaseId::from_stored("lease_desk");
        let requests = sent(&desk.apply_view_action(
            ViewAction::ReleaseWorkspaceLease {
                workspace_id: workspace_id.clone(),
                lease_id: lease_id.clone(),
                expected_generation: 7,
            },
            T0,
        ));
        assert_eq!(
            requests,
            vec![Request::ReleaseWorkspaceWriteLease {
                workspace_id,
                lease_id,
                expected_generation: 7,
            }]
        );
    }

    #[test]
    fn an_isolated_alternative_gets_a_git_valid_non_colliding_branch_name() {
        assert_eq!(
            isolated_branch_name("Alternative movement approach", T0),
            "turn/alternative-movement-approac-1700000000000"
        );
        assert_eq!(isolated_branch_name("🔥🔥", 7), "turn/session-7");
    }

    /// A keystroke's failure must not put a banner on screen: one per character would be
    /// unusable.
    #[test]
    fn a_failed_keystroke_is_not_worth_a_banner() {
        let mut desk = Desk::new();
        let reactions = desk.apply_inbound(
            Inbound::Failed {
                ask: Ask::Stream,
                error: turn_proto::ProtoError::new(
                    turn_proto::ErrorCode::ProcessNotRunning,
                    "nothing to write to",
                ),
            },
            T0,
        );
        assert!(reactions.is_empty());
        assert!(desk.view(T0).notice.is_none());
    }

    #[test]
    fn preparing_context_handoff_sends_only_the_typed_prepare_request() {
        let session_id = SessionId::from_stored("sess_handoff001");
        let source_node_id = NodeId::from_stored("proc_source001");
        let target_node_id = NodeId::from_stored("proc_target001");
        let mut desk = Desk::new();

        let reactions = desk.apply_view_action(
            ViewAction::PrepareContextHandoff {
                session_id: session_id.clone(),
                source_node_id: source_node_id.clone(),
                target_node_id: target_node_id.clone(),
                instruction: Some("Concentrate on the failing invariant".into()),
            },
            T0,
        );

        match reactions.as_slice() {
            [Reaction::Send {
                ask:
                    Ask::PrepareContextHandoff {
                        session_id: asked_session_id,
                        source_node_id: asked_source_node_id,
                        target_node_id: asked_target_node_id,
                    },
                request:
                    Request::PrepareContextHandoff {
                        session_id: requested_session_id,
                        source_node_id: requested_source_node_id,
                        target_node_id: requested_target_node_id,
                        instruction,
                    },
            }] => {
                assert_eq!(asked_session_id, &session_id);
                assert_eq!(asked_source_node_id, &source_node_id);
                assert_eq!(asked_target_node_id, &target_node_id);
                assert_eq!(requested_session_id, &session_id);
                assert_eq!(requested_source_node_id, &source_node_id);
                assert_eq!(requested_target_node_id, &target_node_id);
                assert_eq!(
                    instruction.as_ref().map(ContextHandoffText::as_str),
                    Some("Concentrate on the failing invariant")
                );
            }
            other => panic!("got {other:?}"),
        }
        assert!(!sent(&reactions)
            .iter()
            .any(|request| matches!(request, Request::WritePty { .. })));
    }

    #[test]
    fn a_correlated_context_handoff_answer_produces_the_typed_reaction() {
        let session_id = SessionId::from_stored("sess_handoff002");
        let source_node_id = NodeId::from_stored("proc_source002");
        let target_node_id = NodeId::from_stored("proc_target002");
        let handoff = ContextHandoffView {
            handoff_id: HandoffId::from_stored("handoff_draft002"),
            session_id: session_id.clone(),
            source_node_id: source_node_id.clone(),
            target_node_id: target_node_id.clone(),
            source_label: "Reviewer".into(),
            target_label: "Implementer".into(),
            body: ContextHandoffText::new("[Turn context handoff]\nSafe fact"),
            preview_count: 1,
            redacted: true,
        };
        let mut desk = Desk::new();

        let reactions = desk.apply_inbound(
            Inbound::Answer {
                ask: Ask::PrepareContextHandoff {
                    session_id,
                    source_node_id,
                    target_node_id,
                },
                response: Box::new(Response::ContextHandoff {
                    handoff: Box::new(handoff.clone()),
                }),
            },
            T0,
        );

        match reactions.as_slice() {
            [Reaction::ContextHandoffPrepared(answered)] => assert_eq!(answered, &handoff),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn a_context_handoff_answer_with_different_entity_ids_is_rejected() {
        let session_id = SessionId::from_stored("sess_handoff003");
        let source_node_id = NodeId::from_stored("proc_source003");
        let target_node_id = NodeId::from_stored("proc_target003");
        let expected = ContextHandoffView {
            handoff_id: HandoffId::from_stored("handoff_draft003"),
            session_id: session_id.clone(),
            source_node_id: source_node_id.clone(),
            target_node_id: target_node_id.clone(),
            source_label: "Source".into(),
            target_label: "Target".into(),
            body: ContextHandoffText::new("bounded context"),
            preview_count: 1,
            redacted: false,
        };
        let mismatched = [
            ContextHandoffView {
                session_id: SessionId::from_stored("sess_wrong0001"),
                ..expected.clone()
            },
            ContextHandoffView {
                source_node_id: NodeId::from_stored("proc_wrong0001"),
                ..expected.clone()
            },
            ContextHandoffView {
                target_node_id: NodeId::from_stored("proc_wrong0002"),
                ..expected
            },
        ];

        for handoff in mismatched {
            let mut desk = Desk::new();
            let reactions = desk.apply_inbound(
                Inbound::Answer {
                    ask: Ask::PrepareContextHandoff {
                        session_id: session_id.clone(),
                        source_node_id: source_node_id.clone(),
                        target_node_id: target_node_id.clone(),
                    },
                    response: Box::new(Response::ContextHandoff {
                        handoff: Box::new(handoff),
                    }),
                },
                T0,
            );

            match reactions.as_slice() {
                [Reaction::ContextHandoffPrepareFailed {
                    session_id: failed_session_id,
                    source_node_id: failed_source_node_id,
                    target_node_id: failed_target_node_id,
                    message,
                }] => {
                    assert_eq!(failed_session_id, &session_id);
                    assert_eq!(failed_source_node_id, &source_node_id);
                    assert_eq!(failed_target_node_id, &target_node_id);
                    assert!(message.contains("different Agents"), "got {message}");
                }
                other => panic!("got {other:?}"),
            }
        }
    }

    #[test]
    fn delivering_context_handoff_never_writes_directly_to_the_pty() {
        let session_id = SessionId::from_stored("sess_handoff004");
        let handoff_id = HandoffId::from_stored("handoff_draft004");
        let mut desk = Desk::new();

        let reactions = desk.apply_view_action(
            ViewAction::DeliverContextHandoff {
                session_id: session_id.clone(),
                handoff_id: handoff_id.clone(),
            },
            T0,
        );

        match reactions.as_slice() {
            [Reaction::Send {
                ask:
                    Ask::DeliverContextHandoff {
                        session_id: asked_session_id,
                        handoff_id: asked_handoff_id,
                    },
                request:
                    Request::DeliverContextHandoff {
                        session_id: requested_session_id,
                        handoff_id: requested_handoff_id,
                    },
            }] => {
                assert_eq!(asked_session_id, &session_id);
                assert_eq!(asked_handoff_id, &handoff_id);
                assert_eq!(requested_session_id, &session_id);
                assert_eq!(requested_handoff_id, &handoff_id);
            }
            other => panic!("got {other:?}"),
        }
        assert!(!sent(&reactions)
            .iter()
            .any(|request| matches!(request, Request::WritePty { .. })));

        let acknowledgement = desk.apply_inbound(
            Inbound::Answer {
                ask: Ask::DeliverContextHandoff {
                    session_id,
                    handoff_id: handoff_id.clone(),
                },
                response: Box::new(Response::Ack),
            },
            T0 + 1,
        );
        assert!(matches!(
            acknowledgement.as_slice(),
            [Reaction::ContextHandoffDelivered { handoff_id: delivered }]
                if delivered == &handoff_id
        ));
    }

    #[test]
    fn context_handoff_failures_remain_typed_and_inline() {
        let session_id = SessionId::from_stored("sess_handoff005");
        let source_node_id = NodeId::from_stored("proc_source005");
        let target_node_id = NodeId::from_stored("proc_target005");
        let handoff_id = HandoffId::from_stored("handoff_draft005");
        let mut desk = Desk::new();

        let prepare_failure = desk.apply_inbound(
            Inbound::Failed {
                ask: Ask::PrepareContextHandoff {
                    session_id: session_id.clone(),
                    source_node_id: source_node_id.clone(),
                    target_node_id: target_node_id.clone(),
                },
                error: turn_proto::ProtoError::new(
                    turn_proto::ErrorCode::Conflict,
                    "the source changed",
                ),
            },
            T0,
        );
        assert!(matches!(
            prepare_failure.as_slice(),
            [Reaction::ContextHandoffPrepareFailed {
                session_id: failed_session_id,
                source_node_id: failed_source_node_id,
                target_node_id: failed_target_node_id,
                message,
            }] if failed_session_id == &session_id
                && failed_source_node_id == &source_node_id
                && failed_target_node_id == &target_node_id
                && message == "the source changed"
        ));

        let delivery_failure = desk.apply_inbound(
            Inbound::Failed {
                ask: Ask::DeliverContextHandoff {
                    session_id,
                    handoff_id: handoff_id.clone(),
                },
                error: turn_proto::ProtoError::new(
                    turn_proto::ErrorCode::Conflict,
                    "the destination is busy",
                ),
            },
            T0 + 1,
        );
        assert!(matches!(
            delivery_failure.as_slice(),
            [Reaction::ContextHandoffDeliveryFailed {
                handoff_id: failed_handoff_id,
                message,
            }] if failed_handoff_id == &handoff_id && message == "the destination is busy"
        ));
        assert!(desk.view(T0 + 1).notice.is_none());
    }

    #[test]
    fn reconnecting_invalidates_any_context_handoff_draft() {
        let mut desk = Desk::new();
        desk.apply_view_action(
            ViewAction::PrepareContextHandoff {
                session_id: SessionId::from_stored("sess_handoff006"),
                source_node_id: NodeId::from_stored("proc_source006"),
                target_node_id: NodeId::from_stored("proc_target006"),
                instruction: None,
            },
            T0,
        );

        let reconnecting = desk.apply_inbound(
            Inbound::Status(ConnectionState::Connecting { attempt: 2 }),
            T0 + 1,
        );
        assert!(matches!(
            reconnecting.as_slice(),
            [Reaction::ContextHandoffInvalidated]
        ));

        let reconnected = desk.apply_inbound(connected(), T0 + 2);
        assert_eq!(
            reconnected
                .iter()
                .filter(|reaction| matches!(reaction, Reaction::ContextHandoffInvalidated))
                .count(),
            1
        );
        let requests = sent(&reconnected);
        assert_eq!(
            requests.iter().map(Request::op).collect::<Vec<_>>(),
            vec!["get_hierarchy", "list_templates", "list_attention"]
        );
    }

    #[test]
    fn scrolling_a_pane_moves_its_viewport_and_typing_brings_it_back() {
        let (session, pane_id, node_id) = session_with_agent("Scrollable");
        let mut desk = Desk::new();
        desk.apply_inbound(
            answer(Response::Sessions {
                sessions: vec![summary(&session, 0)],
            }),
            T0,
        );
        desk.apply_inbound(
            answer(Response::SessionDetails {
                details: Box::new(details(&session)),
            }),
            T0,
        );
        let mut start = Grid::blank(4, 12);
        for (col, ch) in "row0".chars().enumerate() {
            if let Some(cell) = start.cell_mut(0, col as u16) {
                cell.text = ch.to_string();
            }
        }
        desk.apply_inbound(
            attached(&pane_id, &session.id, &node_id, start.clone(), 1),
            T0,
        );
        // Scroll one row off the top so there is history to look at.
        let mut next = Grid::blank(4, 12);
        for row in 1..4u16 {
            next.set_row(row - 1, start.row(row));
        }
        desk.apply_inbound(
            Inbound::Event(Box::new(ServerEvent::PaneScreen {
                session_id: session.id.clone(),
                pane_id: pane_id.clone(),
                node_id: None,
                seq: 1,
                update: ScreenUpdate::full(next),
            })),
            T0,
        );

        desk.apply_view_action(
            ViewAction::Pane {
                pane_id: pane_id.clone(),
                action: PaneAction::Scroll(1),
            },
            T0,
        );
        desk.refresh_screens();
        assert!(
            desk.view(T0).panes[0].scrolled,
            "the pane must be showing history"
        );

        desk.apply_view_action(
            ViewAction::Pane {
                pane_id,
                action: PaneAction::Write(b"x".to_vec()),
            },
            T0,
        );
        desk.refresh_screens();
        assert!(
            !desk.view(T0).panes[0].scrolled,
            "typing returns to the live screen, as in every terminal"
        );
    }

    #[test]
    fn a_command_needing_a_session_does_nothing_useful_before_one_exists() {
        let mut desk = Desk::new();
        for command in [
            Command::SplitHorizontal,
            Command::ClosePane,
            Command::ZoomPane,
            Command::CloseSession,
            Command::ArchiveSession,
            Command::InterruptProcess,
        ] {
            assert!(
                sent(&desk.dispatch(command, T0)).is_empty(),
                "{command:?} must not send a request with no session"
            );
        }
    }

    /// A late resync for a cached Pane in another Session must still name its owner;
    /// assuming the selected Session would make the daemon answer `not_found`.
    #[test]
    fn a_resync_names_the_session_the_pane_actually_belongs_to() {
        let (first, _, _) = session_with_agent("First");
        let (second, second_pane, second_node) = session_with_agent("Second");
        let mut desk = Desk::new();
        desk.apply_inbound(
            answer(Response::Sessions {
                sessions: vec![summary(&first, 0), summary(&second, 0)],
            }),
            T0,
        );
        desk.apply_inbound(
            answer(Response::SessionDetails {
                details: Box::new(details(&second)),
            }),
            T0,
        );
        desk.apply_inbound(
            attached(&second_pane, &second.id, &second_node, Grid::blank(4, 8), 1),
            T0,
        );

        // A screen update for that pane, out of sequence, arriving with the right session.
        let reactions = desk.apply_inbound(
            Inbound::Event(Box::new(ServerEvent::PaneScreen {
                session_id: second.id.clone(),
                pane_id: second_pane.clone(),
                node_id: None,
                seq: 40,
                update: ScreenUpdate::full(Grid::blank(4, 8)),
            })),
            T0,
        );
        match sent(&reactions).as_slice() {
            [Request::ResyncPane {
                session_id,
                pane_id,
            }] => {
                assert_eq!(session_id, &second.id);
                assert_eq!(pane_id, &second_pane);
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn a_desync_worth_logging_is_told_apart_from_one_that_repairs_itself() {
        assert!(!is_worth_reporting(&Desync::Missed {
            expected: 1,
            got: 4
        }));
        assert!(is_worth_reporting(&Desync::Malformed("a bad run".into())));
    }
}
