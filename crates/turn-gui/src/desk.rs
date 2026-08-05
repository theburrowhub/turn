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
//! There is no path from a permission to a write: answering an agent is
//! [`Reaction::Send`] carrying `write_pty` with bytes the user typed. There is no path
//! from a heuristic to a focus change: focus moves only when [`turn_core::Effect::Focus`]
//! arrives, and `focus_deferred` and `focus_denied` are dropped by
//! [`crate::announce::Announcement`]. And nothing relaunches: `relaunch_node` is only
//! ever sent from an explicit command.

use std::collections::{BTreeMap, HashMap, HashSet};

use turn_core::attention::{AttentionPolicy, Effect};
use turn_core::ids::{NodeId, PaneId, SessionId, WorkspaceId};
use turn_core::model::{ActivityPreview, Direction, Layout, PaneKind, PreviewVisibility};
use turn_proto::cells::Grid;
use turn_proto::{
    AttentionView, CloseDisposition, FocusTarget, HierarchyKey, HierarchySnapshot, NewPane,
    NodePaneCapability, NodePaneView, ProtoErrorContext, PtySize, Request, Response,
    SessionConflictAlternative, SessionSummary, TemplateSummary, TerminalBytes, TreeNodeView,
    WorkspaceSummary,
};

use crate::announce::Announcement;
use crate::keymap::Command;
use crate::panes::{self, Arrangement};
use crate::terminal::feed::{Desync, PaneFeed};
use crate::terminal::PaneAction;
use crate::transport::{Ask, ConnectionState, Inbound};
use crate::view::{
    HierarchyAction, Overview, PaneContent, PendingPermission, QueueItem, SessionRow,
    TemporaryPaneContent, TurnView, ViewAction,
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
}

/// The default geometry a pane is attached at, before it has been laid out.
///
/// Replaced by the real size on the first frame, from the rectangle the pane actually
/// occupies. It exists because `attach_pane` needs a size and the answer is not known
/// until something has been drawn.
const INITIAL_SIZE: PtySize = PtySize { rows: 24, cols: 80 };

/// The safe parts of a main-checkout creation request that may be reused only after
/// the user chooses a typed lease-conflict alternative.
#[derive(Debug, Clone)]
struct PendingSessionDraft {
    workspace_id: WorkspaceId,
    name: String,
    cwd: Option<String>,
    panes: Option<Vec<NewPane>>,
    note: Option<String>,
    tags: Vec<String>,
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
    /// The sole persistent navigation projection. Flat collections below are
    /// compatibility indexes for commands and pane ownership, never a second
    /// navigation model.
    hierarchy: Option<HierarchySnapshot>,
    surface_id: String,
    preview_history: HashMap<NodeId, Vec<ActivityPreview>>,
    temporary_pane: Option<NodePaneView>,
    write_conflict: Option<ProtoErrorContext>,
    pending_session: Option<PendingSessionDraft>,
    workspaces: Vec<WorkspaceSummary>,
    templates: Vec<TemplateSummary>,
    /// In the daemon's own order, re-sorted locally with the daemon's own ranking after
    /// a push so a state change moves a row without a round trip.
    sessions: Vec<SessionSummary>,
    selected: Option<SessionId>,
    /// A layout per session the window has fetched.
    ///
    /// Per session rather than only for the selected one, because the overview needs a
    /// pane to attach to in every session — and because it makes the pane-to-session
    /// map below possible, without which a resync for a pane in another session would be
    /// addressed to the wrong one.
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
    overview_open: bool,
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
            hierarchy: None,
            surface_id: "main-window".to_string(),
            preview_history: HashMap::new(),
            temporary_pane: None,
            write_conflict: None,
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
            overview_open: false,
        }
    }

    pub fn connection(&self) -> &ConnectionState {
        &self.connection
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

    pub fn overview_open(&self) -> bool {
        self.overview_open
    }

    /// The workspace a new session would go in.
    ///
    /// The selected session's, or the first there is. `None` when the daemon has not
    /// answered yet, which is why every command that needs one checks.
    fn current_workspace(&self) -> Option<WorkspaceId> {
        self.selected_summary()
            .map(|summary| summary.workspace_id.clone())
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
            .or_else(|| {
                self.templates
                    .iter()
                    .find(|template| template.name == "Coding")
            })
            .or_else(|| self.templates.first())
    }

    fn selected_summary(&self) -> Option<&SessionSummary> {
        let id = self.selected.as_ref()?;
        self.sessions.iter().find(|summary| &summary.id == id)
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
        match message {
            Inbound::Status(state) => self.apply_status(state),
            Inbound::Event(event) => self.apply_event(*event, now_ms),
            Inbound::Answer { ask, response } => self.apply_answer(ask, *response),
            Inbound::Failed { ask, error } => {
                let lease_conflict = error.context.as_deref().and_then(|context| {
                    matches!(
                        context,
                        ProtoErrorContext::WorkspaceWriteLeaseConflict { .. }
                    )
                    .then(|| context.clone())
                });
                if let Some(context) = lease_conflict {
                    self.write_conflict = Some(context);
                } else if matches!(
                    &ask,
                    Ask::Action("starting a session")
                        | Ask::Action("starting a session from a template")
                ) {
                    self.pending_session = None;
                }
                if ask.is_worth_reporting() {
                    let message = format!("{}: {}", ask.describing(), error.message);
                    self.notice = Some(message.clone());
                    return vec![Reaction::Notice(message)];
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
            return Vec::new();
        }
        self.feeds.clear();
        self.attaching.clear();
        self.pty_sizes.clear();
        self.layouts.clear();
        self.trees.clear();
        self.policies.clear();
        self.pane_owner.clear();
        self.hierarchy = None;
        self.preview_history.clear();
        self.temporary_pane = None;
        self.write_conflict = None;
        self.pending_session = None;
        self.workspaces.clear();
        self.sessions.clear();
        self.selected = None;
        vec![
            Reaction::Send {
                ask: Ask::Hierarchy,
                request: Request::GetHierarchy {
                    surface_id: self.surface_id.clone(),
                    include_archived: false,
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
        ]
    }

    fn apply_answer(&mut self, ask: Ask, response: Response) -> Vec<Reaction> {
        match (ask, response) {
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
            (_, Response::Workspace { workspace }) => {
                self.workspaces.retain(|known| known.id != workspace.id);
                self.workspaces.push(workspace);
                vec![self.hierarchy_request()]
            }
            (_, Response::Templates { templates }) => {
                self.templates = templates;
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
            (_, Response::Session { session }) => {
                self.write_conflict = None;
                self.pending_session = None;
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
            (_, Response::Attached { attachment }) => {
                self.attaching.remove(&attachment.pane_id);
                self.feeds
                    .insert(attachment.pane_id.clone(), PaneFeed::attach(&attachment));
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
                include_archived: false,
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
            return Vec::new();
        };
        let needs_details = self.layouts.get(&active).is_none();
        self.selected = Some(active.clone());
        if needs_details {
            vec![Reaction::Send {
                ask: Ask::Details(active.clone()),
                request: Request::GetSession { session_id: active },
            }]
        } else {
            self.attach_wanted()
        }
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
                self.upsert_session(session.clone());
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
                Vec::new()
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
                    return Vec::new();
                }
                // Nothing here has been relaunched. Saying what was found, and what
                // could be started again, is the whole of the window's job: the user
                // answers with a relaunch or does not.
                let lost = panes.iter().filter(|pane| pane.can_relaunch).count();
                let message = format!(
                    "{session_id}: restored as {state:?}. {lost} pane(s) could be started again — \
                     nothing has been"
                );
                self.notice = Some(message.clone());
                vec![Reaction::Notice(message)]
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
                    ask: Ask::Action("focusing an existing Pane for Attention"),
                    request: Request::FocusPaneForNode {
                        surface_id: self.surface_id.clone(),
                        session_id: session_id.clone(),
                        node_id: node_id.clone(),
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

    /// Which session a pane belongs to.
    ///
    /// Looked up rather than assumed to be the selected one: with the overview open the
    /// window holds panes from every session, and a resync addressed to the wrong session
    /// would be refused with `not_found`.
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
    /// Every terminal pane of the session on screen, and — while the overview is open —
    /// one pane from each other session, which is what a thumbnail is a picture of. One
    /// rather than all: the overview shows a session, not its layout, and thirty
    /// sessions of every pane would be a great deal of screen for a postage stamp.
    fn wanted_panes(&self) -> Vec<(SessionId, PaneId)> {
        let mut wanted: Vec<(SessionId, PaneId)> = Vec::new();
        if let Some((session_id, layout)) = self
            .selected
            .as_ref()
            .and_then(|id| self.layouts.get(id).map(|layout| (id.clone(), layout)))
        {
            for pane in layout.panes() {
                if pane.kind.is_terminal() {
                    wanted.push((session_id.clone(), pane.id.clone()));
                }
            }
        }
        if self.overview_open {
            for (session_id, layout) in &self.layouts {
                if Some(session_id) == self.selected.as_ref() {
                    continue;
                }
                if let Some(pane) = layout
                    .panes()
                    .into_iter()
                    .find(|pane| pane.kind.is_terminal())
                {
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
        // Screens for panes nothing wants any more are dropped, which for a window not
        // showing the overview is every pane of the session just left.
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
                Vec::new()
            }
            SessionConflictAlternative::FocusOwner => {
                self.write_conflict = None;
                self.pending_session = None;
                self.select(owner.session_id)
            }
            SessionConflictAlternative::CreateReadOnly => {
                let draft = self.pending_session.take().unwrap_or(PendingSessionDraft {
                    workspace_id,
                    name: "Read-only review".into(),
                    cwd: None,
                    panes: None,
                    note: None,
                    tags: Vec::new(),
                });
                self.write_conflict = None;
                vec![Reaction::Send {
                    ask: Ask::Action("creating a read-only Session"),
                    request: Request::CreateReadOnlySession {
                        workspace_id: draft.workspace_id,
                        name: draft.name,
                        cwd: draft.cwd,
                        panes: draft.panes,
                        note: draft.note,
                        tags: draft.tags,
                    },
                }]
            }
            SessionConflictAlternative::CreateIsolatedWorktree => {
                let draft = self.pending_session.take().unwrap_or(PendingSessionDraft {
                    workspace_id,
                    name: "Isolated worktree".into(),
                    cwd: None,
                    panes: None,
                    note: None,
                    tags: Vec::new(),
                });
                let branch = isolated_branch_name(&draft.name, now_ms);
                self.write_conflict = None;
                vec![Reaction::Send {
                    ask: Ask::Action("creating an isolated worktree Session"),
                    request: Request::CreateWorktreeSession {
                        workspace_id: draft.workspace_id,
                        name: draft.name,
                        branch,
                        worktree_path: None,
                        panes: draft.panes,
                        note: draft.note,
                        tags: draft.tags,
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
            Command::ToggleSessionOverview => {
                self.overview_open = !self.overview_open;
                if !self.overview_open {
                    // Screens taken only for thumbnails are released, so an overview that
                    // was opened once does not hold thirty grids for the rest of the day.
                    let wanted: HashSet<PaneId> = self
                        .wanted_panes()
                        .into_iter()
                        .map(|(_, pane)| pane)
                        .collect();
                    self.feeds.retain(|pane, _| wanted.contains(pane));
                    self.attaching.retain(|pane| wanted.contains(pane));
                    return Vec::new();
                }
                // A session with no layout yet has no pane to attach to, so the overview
                // asks for the ones it is missing.
                let mut reactions: Vec<Reaction> = self
                    .sessions
                    .iter()
                    .filter(|summary| !self.layouts.contains_key(&summary.id))
                    .map(|summary| Reaction::Send {
                        ask: Ask::Details(summary.id.clone()),
                        request: Request::GetSession {
                            session_id: summary.id.clone(),
                        },
                    })
                    .collect();
                reactions.extend(self.attach_wanted());
                reactions
            }
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
            Command::QuickNewSession => match self.current_workspace() {
                Some(workspace_id) => {
                    let Some(template_id) = self
                        .preferred_template(&workspace_id)
                        .map(|template| template.id.clone())
                    else {
                        return vec![Reaction::Notice("no template to start from yet".into())];
                    };
                    let draft = PendingSessionDraft {
                        workspace_id: workspace_id.clone(),
                        name: format!("Session {}", self.sessions.len() + 1),
                        cwd: None,
                        panes: None,
                        note: None,
                        tags: Vec::new(),
                    };
                    self.pending_session = Some(draft.clone());
                    vec![Reaction::Send {
                        ask: Ask::Action("starting a session"),
                        request: Request::CreateSessionFromTemplate {
                            workspace_id,
                            template_id,
                            name: Some(draft.name),
                            cwd: None,
                            branch: None,
                            task: None,
                        },
                    }]
                }
                None => vec![Reaction::Notice(
                    "no workspace yet — the daemon has not answered".into(),
                )],
            },
            Command::NewSession => match self.current_workspace() {
                Some(workspace_id) => {
                    let Some(template_id) = self
                        .preferred_template(&workspace_id)
                        .map(|template| template.id.clone())
                    else {
                        return vec![Reaction::Notice("no template to start from yet".into())];
                    };
                    // A typed lease conflict may require creating a safe alternative.
                    // The template response is daemon-owned, so the fallback keeps the
                    // requested name but starts with no inferred commands.
                    self.pending_session = Some(PendingSessionDraft {
                        workspace_id: workspace_id.clone(),
                        name: format!("Session {}", self.sessions.len() + 1),
                        cwd: None,
                        panes: None,
                        note: None,
                        tags: Vec::new(),
                    });
                    vec![Reaction::Send {
                        ask: Ask::Action("starting a session from a template"),
                        request: Request::CreateSessionFromTemplate {
                            workspace_id,
                            template_id,
                            name: None,
                            cwd: None,
                            branch: None,
                            task: None,
                        },
                    }]
                }
                None => vec![Reaction::Notice("create a workspace first".into())],
            },
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
            Command::ArchiveSession => match session {
                Some(session_id) => vec![Reaction::Send {
                    ask: Ask::Action("archiving the session"),
                    request: Request::ArchiveSession {
                        session_id,
                        archived: true,
                    },
                }],
                None => Vec::new(),
            },
            Command::CloseSession => match session {
                Some(session_id) => vec![Reaction::Send {
                    ask: Ask::Action("closing the session"),
                    request: Request::CloseSession {
                        session_id,
                        // The processes keep running. "Close" is ambiguous and the safe
                        // reading is the one that cannot destroy work; stopping them is
                        // a separate, named command.
                        disposition: CloseDisposition::KeepProcesses,
                    },
                }],
                None => Vec::new(),
            },
            Command::SplitHorizontal | Command::SplitVertical => {
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
            Command::LaunchAgent | Command::LaunchShell | Command::LaunchTui => {
                let Some((session_id, pane_id)) = session.zip(pane) else {
                    return Vec::new();
                };
                let kind = match command {
                    Command::LaunchAgent => PaneKind::Agent,
                    Command::LaunchTui => PaneKind::Tui,
                    _ => PaneKind::Shell,
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
            | Command::ToggleAgentTree
            | Command::ToggleEventLog
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
            ViewAction::CreateWorkspace { name, root } => vec![Reaction::Send {
                ask: Ask::Action("creating a workspace"),
                request: Request::CreateWorkspace { name, root },
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
            ViewAction::ResizeDivider { pane_id, fraction } => match self.selected.clone() {
                Some(session_id) => vec![Reaction::Send {
                    ask: Ask::Action("resizing the pane"),
                    request: Request::ResizePane {
                        session_id,
                        pane_id,
                        delta: fraction,
                    },
                }],
                None => Vec::new(),
            },
            ViewAction::CloseOverlay => Vec::new(),
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
                // The only path to a pty. There is no approve request, and this is why:
                // answering an agent is the human typing.
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

        // One screen per session for the overview, from the pane the window attached to
        // for exactly that purpose. The *live* screen, not the viewport: the overview
        // should show what a session is doing now, not where somebody left a scrollbar.
        let overview_screens: Vec<(SessionId, &Grid)> = self
            .sessions
            .iter()
            .filter_map(|summary| {
                let layout = self.layouts.get(&summary.id)?;
                let pane = layout
                    .panes()
                    .into_iter()
                    .find(|pane| pane.kind.is_terminal())?;
                let feed = self.feeds.get(&pane.id)?;
                Some((summary.id.clone(), feed.live_screen()))
            })
            .collect();

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
            sessions,
            selected: self.selected.clone(),
            layout: self.layout().cloned(),
            panes,
            temporary_pane,
            overview_screens,
            permission: self.permission_banner(now_ms),
            queue: self.queue.iter().map(queue_item).collect(),
            connection: Some(self.connection.clone()),
            notice: self.notice.clone(),
            write_conflict: self.write_conflict(),
            overview: Overview {
                open: self.overview_open,
            },
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
        let agent = summary.primary_agent.as_ref()?;
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
        HierarchyKey::Session { session_id } => Some(session_id.clone()),
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
        Pane, PendingPermission as CorePermission, ProcessNode, Session, Template, Workspace,
    };
    use turn_core::state::{AwaitingReason, Lifecycle, Turn};
    use turn_core::Effect;
    use turn_proto::{
        AttentionView, PaneAttachment, PaneStream, ScreenUpdate, ServerEvent, TerminalBytes,
        Welcome,
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

    fn attachment(
        pane_id: &PaneId,
        session_id: &SessionId,
        grid: Grid,
        next_seq: u64,
    ) -> PaneAttachment {
        PaneAttachment {
            session_id: session_id.clone(),
            pane_id: pane_id.clone(),
            node_id: None,
            stream: PaneStream::Cells,
            screen: Some(Box::new(grid)),
            replay: TerminalBytes::new(Vec::new()),
            size: PtySize::new(24, 80),
            scrollback_truncated: false,
            bytes_seen: 0,
            next_seq,
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

    /// The whole point of the sequence number, from the window's side: a missed update
    /// produces a resync rather than a screen neither end believes in.
    #[test]
    fn a_missed_screen_update_asks_for_the_whole_screen_again() {
        let (session, pane_id, _) = session_with_agent("Busy");
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
            answer(Response::Attached {
                attachment: Box::new(attachment(&pane_id, &session.id, Grid::blank(24, 80), 1)),
            }),
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

    /// Answering an agent is the human typing, and this is the only path to a pty.
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

    /// "Close" is ambiguous, and the reading that cannot destroy work is the right one.
    #[test]
    fn closing_a_session_keeps_the_processes_running() {
        let (session, _, _) = session_with_agent("Keep my work");
        let mut desk = Desk::new();
        desk.apply_inbound(
            answer(Response::Sessions {
                sessions: vec![summary(&session, 0)],
            }),
            T0,
        );
        match sent(&desk.dispatch(Command::CloseSession, T0)).as_slice() {
            [Request::CloseSession { disposition, .. }] => {
                assert_eq!(*disposition, CloseDisposition::KeepProcesses);
            }
            other => panic!("got {other:?}"),
        }
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
                    node_id: None,
                    lifecycle: Lifecycle::Lost,
                    can_relaunch: true,
                    command: Some("cargo watch -x test".into()),
                }],
            })),
            T0,
        );
        assert!(
            sent(&reactions).is_empty(),
            "a restore reports and offers; it must not start anything"
        );
        assert!(
            reactions
                .iter()
                .any(|reaction| matches!(reaction, Reaction::Notice(_))),
            "and the user must be told"
        );

        for command in Command::ALL {
            for request in sent(&desk.dispatch(*command, T0)) {
                assert_ne!(request.op(), "relaunch_node", "{command:?}");
            }
        }
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
            reason: AwaitingReason::Permission,
            summary: Some("run rm -rf build".into()),
            confidence: Confidence::Explicit,
            created_ms: T0,
            updated_ms: T0,
            state: EntryState::Pending,
            priority_boost: 0,
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

    /// A demand that came from a heuristic has to be drawn as a guess.
    #[test]
    fn an_inferred_demand_is_marked_provisional_in_the_queue() {
        let (session, _, _) = session_with_agent("Guessy");
        let entry = AttentionEntry {
            id: AttentionId::new(),
            session_id: session.id.clone(),
            node_id: None,
            reason: AwaitingReason::Input,
            summary: Some("looks like a prompt".into()),
            confidence: Confidence::InferredHigh,
            created_ms: T0,
            updated_ms: T0,
            state: EntryState::Pending,
            priority_boost: 0,
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
        let mut desk = Desk::new();
        desk.apply_inbound(
            answer(Response::Sessions {
                sessions: vec![summary(&session, 0)],
            }),
            T0,
        );
        match sent(&desk.apply_view_action(
            ViewAction::ResizeDivider {
                pane_id: pane_id.clone(),
                fraction: 0.125,
            },
            T0,
        ))
        .as_slice()
        {
            [Request::ResizePane {
                pane_id: named,
                delta,
                ..
            }] => {
                assert_eq!(named, &pane_id);
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
        let (session, pane_id, _) = session_with_agent("Rearranged");
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
            answer(Response::Attached {
                attachment: Box::new(attachment(&pane_id, &session.id, Grid::blank(24, 80), 1)),
            }),
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
        desk.apply_inbound(
            answer(Response::Templates {
                templates: vec![TemplateSummary::from_template(&Template::coding(T0))],
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
                ask: Ask::Action("starting a session"),
                error,
            },
            T0,
        );
        assert!(desk.write_conflict().is_some());
        assert!(
            sent(&desk.resolve_write_conflict(SessionConflictAlternative::CreateReadOnly, T0))
                .iter()
                .any(|request| matches!(request, Request::CreateReadOnlySession { .. })),
            "the failed main Session is not retried; the chosen read-only API is used"
        );
        assert!(desk.write_conflict().is_none());
    }

    #[test]
    fn first_run_workspace_creation_is_a_typed_user_action() {
        let mut desk = Desk::new();
        let requests = sent(&desk.apply_view_action(
            ViewAction::CreateWorkspace {
                name: "turn".into(),
                root: "/repo/turn".into(),
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
    fn quick_new_prefers_coding_over_alphabetical_blank() {
        let workspace = Workspace::new("turn", "/repo/turn", T0);
        let workspace_id = workspace.id.clone();
        let blank = TemplateSummary::from_template(&Template::blank(T0));
        let coding = TemplateSummary::from_template(&Template::coding(T0));
        let coding_id = coding.id.clone();
        let mut desk = Desk::new();
        desk.workspaces = vec![WorkspaceSummary::from_workspace(&workspace, &[])];
        desk.templates = vec![blank, coding];

        assert!(matches!(
            sent(&desk.dispatch(Command::QuickNewSession, T0)).as_slice(),
            [Request::CreateSessionFromTemplate {
                workspace_id: requested_workspace,
                template_id,
                ..
            }] if requested_workspace == &workspace_id && template_id == &coding_id
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
    fn scrolling_a_pane_moves_its_viewport_and_typing_brings_it_back() {
        let (session, pane_id, _) = session_with_agent("Scrollable");
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
            answer(Response::Attached {
                attachment: Box::new(attachment(&pane_id, &session.id, start.clone(), 1)),
            }),
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

    #[test]
    fn the_overview_with_no_sessions_costs_no_request() {
        let mut desk = Desk::new();
        assert!(!desk.overview_open());
        assert!(sent(&desk.dispatch(Command::ToggleSessionOverview, T0)).is_empty());
        assert!(desk.overview_open());
        desk.dispatch(Command::ToggleSessionOverview, T0);
        assert!(!desk.overview_open());
    }

    /// A thumbnail is a picture of a session, so the overview needs a screen from every
    /// session — which means one attachment each, not one for the session on screen.
    #[test]
    fn opening_the_overview_asks_for_one_screen_from_every_session() {
        let (first, first_pane, _) = session_with_agent("First");
        let (second, second_pane, _) = session_with_agent("Second");
        let mut desk = Desk::new();
        desk.apply_inbound(
            answer(Response::Sessions {
                sessions: vec![summary(&first, 0), summary(&second, 0)],
            }),
            T0,
        );
        // The selected session's detail arrives, so only its pane is attached.
        let selected = desk.selected().cloned().expect("one is selected");
        let (chosen, other, other_pane) = if selected == first.id {
            (&first, &second, second_pane.clone())
        } else {
            (&second, &first, first_pane.clone())
        };
        desk.apply_inbound(
            answer(Response::SessionDetails {
                details: Box::new(details(chosen)),
            }),
            T0,
        );

        // Opening the overview asks for the layout of the session it does not have.
        let opening = desk.dispatch(Command::ToggleSessionOverview, T0);
        assert!(
            sent(&opening).iter().any(|request| matches!(
                request,
                Request::GetSession { session_id } if session_id == &other.id
            )),
            "the overview must fetch the sessions it has no pane for: {:?}",
            sent(&opening)
        );

        // And when that layout arrives, one of its panes is attached.
        let arriving = desk.apply_inbound(
            answer(Response::SessionDetails {
                details: Box::new(details(other)),
            }),
            T0,
        );
        assert!(
            sent(&arriving).iter().any(|request| matches!(
                request,
                Request::AttachPane { pane_id, .. } if pane_id == &other_pane
            )),
            "got {:?}",
            sent(&arriving)
        );

        desk.apply_inbound(
            answer(Response::Attached {
                attachment: Box::new(attachment(
                    &other_pane,
                    &other.id,
                    Grid::from_lines(&["other session"], 20),
                    1,
                )),
            }),
            T0,
        );
        desk.refresh_screens();
        let view = desk.view(T0);
        assert!(
            view.overview_screens
                .iter()
                .any(|(session, grid)| session == &other.id && grid.row_text(0) == "other session"),
            "the overview must have a screen for a session that is not on screen"
        );

        // Closing it releases the screens taken only for thumbnails.
        desk.dispatch(Command::ToggleSessionOverview, T0);
        desk.refresh_screens();
        assert!(
            !desk
                .view(T0)
                .overview_screens
                .iter()
                .any(|(session, _)| session == &other.id),
            "an overview opened once must not hold thirty screens for the rest of the day"
        );
    }

    /// A resync for a pane in another session — which the overview makes possible — has to
    /// name that session, or the daemon answers `not_found`.
    #[test]
    fn a_resync_names_the_session_the_pane_actually_belongs_to() {
        let (first, _, _) = session_with_agent("First");
        let (second, second_pane, _) = session_with_agent("Second");
        let mut desk = Desk::new();
        desk.apply_inbound(
            answer(Response::Sessions {
                sessions: vec![summary(&first, 0), summary(&second, 0)],
            }),
            T0,
        );
        desk.dispatch(Command::ToggleSessionOverview, T0);
        desk.apply_inbound(
            answer(Response::SessionDetails {
                details: Box::new(details(&second)),
            }),
            T0,
        );
        desk.apply_inbound(
            answer(Response::Attached {
                attachment: Box::new(attachment(&second_pane, &second.id, Grid::blank(4, 8), 1)),
            }),
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
