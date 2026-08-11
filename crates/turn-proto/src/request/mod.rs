//! Requests: everything the UI can ask the daemon to do.
//!
//! One flat enum rather than a group per subsystem. It is longer to read but it
//! makes two things true that matter more: the complete surface of the daemon is
//! visible in one place, and `Request::expected_result` can name the response for
//! every single operation — which is what keeps `docs/PROTOCOL.md` honest, since a
//! test checks that mapping against the response catalogue.
//!
//! Three rules are enforced by the *shape* of the requests, not by the daemon
//! remembering to check them:
//!
//! * There is no request that approves an agent's permission. Answering a
//!   permission prompt is typing into the agent's terminal, which is
//!   [`Request::WritePty`] — an explicit act by the human. Turn cannot approve on
//!   the user's behalf because the protocol gives it no way to say so. A context
//!   handoff is structurally refused while any interaction is pending.
//! * There is no request that runs a command Turn inferred from output. A process
//!   starts from a template, a pane definition or an explicit relaunch, all of
//!   which the user chose.
//! * [`Request::RelaunchNode`] exists and nothing else restarts anything. Restore
//!   offers; the user decides.

use serde::{Deserialize, Serialize};
use turn_core::attention::UserContext;
use turn_core::ids::{
    AttentionId, CheckoutId, HandoffId, LeaseId, NodeId, PaneId, SessionId, TemplateId, WorkspaceId,
};
use turn_core::model::{
    Direction, DropZone, Layout, LayoutPreset, PaneGeometry, PaneKind, PanePlacement,
    PreviewVisibility, RelationshipKind, RestoreBehaviour, Template, TreeFilter,
    TreeVisibilityMode,
};
use turn_core::settings::Scope as SettingsScope;
use turn_core::state::{Lifecycle, Turn};

use crate::bytes::TerminalBytes;
use crate::geometry::PtySize;
use crate::screen::PaneStream;
use crate::search::SearchQuery;
use crate::view::{ContextHandoffMode, ContextHandoffText, HierarchyKey};

/// A client-supplied correlation id.
///
/// Opaque to the daemon, which echoes it back untouched. Client-supplied rather
/// than server-assigned so the UI can hold a pending-request map keyed by
/// something it already has, without waiting for a round trip to learn the key.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RequestId(String);

impl RequestId {
    pub fn new(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for RequestId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// What to do with the processes of something being closed.
///
/// No default, on purpose. "Close" is ambiguous — the whole point of the daemon is
/// that processes outlive the UI — and a daemon guessing here would either kill
/// work the user wanted kept or leak processes the user thought were gone. The
/// client has to say which it means.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloseDisposition {
    /// Detach only. Processes keep running under the daemon and the session can be
    /// reopened later.
    KeepProcesses,
    /// Ask the processes to stop, the way closing a terminal would.
    Terminate,
    /// Stop them without asking. For a process that ignored `Terminate`.
    Kill,
}

/// Where focus should move.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FocusTarget {
    Pane {
        pane_id: PaneId,
    },
    /// Cycle forward, wrapping. Drives the cycle-panes shortcut.
    Next,
    Previous,
}

/// A pane to create, before it has an identity.
///
/// Ids are minted by the daemon rather than accepted from the client: the daemon
/// is the only writer of state, and a client that mints its own ids can collide
/// with a second client attached to the same daemon.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct NewPane {
    pub kind: PaneKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The command to run. `None` leaves the pane empty until something is put in
    /// it — which is also what makes a pane a placeholder rather than a launch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<(String, String)>,
    #[serde(default)]
    pub restore: RestoreBehaviour,
}

impl NewPane {
    pub fn new(kind: PaneKind) -> Self {
        Self {
            kind,
            title: None,
            command: None,
            args: Vec::new(),
            cwd: None,
            env: Vec::new(),
            restore: RestoreBehaviour::default(),
        }
    }

    pub fn with_command(mut self, command: impl Into<String>) -> Self {
        self.command = Some(command.into());
        self
    }
}

/// Everything the UI can ask for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    // ---------------------------------------------------------------- workspaces
    ListWorkspaces {
        #[serde(default)]
        include_archived: bool,
    },
    CreateWorkspace {
        name: String,
        root: String,
    },
    RenameWorkspace {
        workspace_id: WorkspaceId,
        name: String,
    },
    /// Archives or unarchives. One request with a flag rather than two, so the
    /// undo path is the same code as the do path.
    ArchiveWorkspace {
        workspace_id: WorkspaceId,
        archived: bool,
    },
    /// Copies a workspace's settings — env, defaults, policy — with no sessions.
    DuplicateWorkspace {
        workspace_id: WorkspaceId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    CloseWorkspace {
        workspace_id: WorkspaceId,
        disposition: CloseDisposition,
    },
    /// Removes a Workspace and its Sessions from Turn for good.
    ///
    /// The third and last of the three verbs a Workspace row offers, and the only one that
    /// does not come back. `ArchiveWorkspace` hides it and stops nothing; `CloseWorkspace`
    /// stops its work and leaves the record; this one stops its work, releases its write
    /// lease and then **forgets** it: the Workspace row, every Session under it, their
    /// layouts, their process trees, their event log, their attention entries and their
    /// per-window tree state.
    ///
    /// What it does not do is touch the user's disk. The checkout is a directory they chose
    /// and Turn does not own it: no file is removed, no branch and no worktree is deleted. A
    /// caller must say so in the words it puts in front of the user, because "delete" without
    /// that sentence is a question about their work rather than about Turn's record of it.
    ///
    /// Deleting something already gone answers `Ack` rather than `not_found`, so a retry after
    /// a lost reply is not an error.
    DeleteWorkspace {
        workspace_id: WorkspaceId,
        /// How to stop whatever is still running under it.
        ///
        /// Required rather than assumed: the difference between `Terminate` and `Kill` is the
        /// difference between letting an agent finish writing a file and not, and a delete is
        /// the last moment anyone can choose.
        disposition: CloseDisposition,
    },

    // ----------------------------------------------------------- unified tree
    /// The complete Workspace -> Session -> Process projection for one window.
    /// Also serves as the resync operation after a revision gap.
    GetHierarchy {
        surface_id: String,
        #[serde(default)]
        include_archived: bool,
    },
    /// Fetches the optional contextual panel for exactly one hierarchy row.
    /// Logs and configuration stay out of the always-live tree snapshot.
    GetInspector {
        key: HierarchyKey,
    },
    /// Persists one expansion decision without broadcasting it to other windows.
    SetTreeExpanded {
        surface_id: String,
        key: HierarchyKey,
        expanded: bool,
    },
    /// Persists selection for one window. Selection does not focus a Pane or
    /// resolve Attention. `None` clears a stale selection.
    SelectTreeNode {
        surface_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selected: Option<HierarchyKey>,
    },
    /// Changes every expandable row in one durable transaction.
    SetTreeExpandedAll {
        surface_id: String,
        expanded: bool,
    },
    /// Persists the surface-wide filter, density and viewport anchor.
    SetTreePresentation {
        surface_id: String,
        #[serde(default)]
        filters: Vec<TreeFilter>,
        #[serde(default)]
        visibility_mode: TreeVisibilityMode,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scroll_anchor: Option<HierarchyKey>,
    },
    /// Places one row before a sibling. `None` moves it to the end.
    MoveTreeNode {
        surface_id: String,
        key: HierarchyKey,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        before: Option<HierarchyKey>,
    },
    /// A durable user name for an Agent. The declared integration name remains
    /// in provenance and can still be inspected.
    RenameNode {
        session_id: SessionId,
        node_id: NodeId,
        name: String,
    },
    /// Replaces a parent edge with an explicit user correction. `None` makes the
    /// node a Session root and requires `Unknown` as the relationship kind.
    CorrectRelationship {
        session_id: SessionId,
        node_id: NodeId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_node_id: Option<NodeId>,
        relationship_kind: RelationshipKind,
    },

    // --------------------------------------------------------- checkout lease
    GetWorkspaceWriteLease {
        workspace_id: WorkspaceId,
    },
    /// Promotes an existing eligible Session only when the daemon can acquire
    /// the lease atomically. A conflict returns typed owner/alternative context.
    AcquireWorkspaceWriteLease {
        workspace_id: WorkspaceId,
        session_id: SessionId,
        checkout_id: CheckoutId,
    },
    ReleaseWorkspaceWriteLease {
        workspace_id: WorkspaceId,
        lease_id: LeaseId,
        /// Fencing token returned by acquisition. A stale client may not release
        /// a newer owner's generation even if it retained the lease id.
        expected_generation: u64,
    },

    // ------------------------------------------------------------------ sessions
    ListSessions {
        /// `None` lists every workspace's sessions, for the global view.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace_id: Option<WorkspaceId>,
        #[serde(default)]
        include_archived: bool,
    },
    /// Creates a main-checkout Session and acquires its exclusive write lease in
    /// the same daemon transaction, before initialisation or Process launch. An
    /// existing owner is a typed conflict, never a silent second writer.
    CreateSession {
        workspace_id: WorkspaceId,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        /// The panes to start with. `None` gives a single shell.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        panes: Option<Vec<NewPane>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tags: Vec<String>,
    },
    /// The explicit safe alternative when the primary checkout already has a
    /// writer. The daemon applies its technical write guard before launching.
    CreateReadOnlySession {
        workspace_id: WorkspaceId,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        panes: Option<Vec<NewPane>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tags: Vec<String>,
    },
    /// Resolves a primary-checkout lease conflict without flattening the
    /// original Template into client-supplied panes. The daemon reloads and
    /// instantiates the authoritative Template and launches it only after the
    /// platform read-only guard is active.
    CreateReadOnlySessionFromTemplate {
        workspace_id: WorkspaceId,
        template_id: TemplateId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        branch: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task: Option<String>,
    },
    /// The explicit concurrent-writer alternative. The daemon creates and
    /// records the checkout; it never reuses a caller-provided path silently.
    CreateWorktreeSession {
        workspace_id: WorkspaceId,
        name: String,
        branch: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        worktree_path: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        panes: Option<Vec<NewPane>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tags: Vec<String>,
    },
    /// Resolves a primary-checkout lease conflict by instantiating the original
    /// Template in a daemon-created isolated checkout. `template_branch` is the
    /// value from the failed request used for name rendering; `branch` is the
    /// isolated Git branch that will actually be created.
    CreateWorktreeSessionFromTemplate {
        workspace_id: WorkspaceId,
        template_id: TemplateId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        template_branch: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task: Option<String>,
        branch: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        worktree_path: Option<String>,
    },
    CreateSessionFromTemplate {
        workspace_id: WorkspaceId,
        template_id: TemplateId,
        /// Overrides the template's name pattern.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        /// Fills `{branch}` in the template's name pattern.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        branch: Option<String>,
        /// Fills `{task}`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task: Option<String>,
    },
    RenameSession {
        session_id: SessionId,
        name: String,
    },
    ArchiveSession {
        session_id: SessionId,
        archived: bool,
    },
    /// Same shape and settings, new identity, no live processes.
    DuplicateSession {
        session_id: SessionId,
    },
    CloseSession {
        session_id: SessionId,
        disposition: CloseDisposition,
    },
    /// Removes a Session from Turn for good.
    ///
    /// `ArchiveSession` is the reversible one and `CloseSession` stops the work; this stops
    /// the work, releases the write lease the Session holds and then forgets it — its layout,
    /// its process tree, its history, its attention entries, its scratch directory and its
    /// per-window tree state. Nothing on the user's disk is touched: the checkout, the branch
    /// and any worktree are theirs, not Turn's.
    ///
    /// Deleting something already gone answers `Ack`, so a retry after a lost reply is not an
    /// error.
    DeleteSession {
        session_id: SessionId,
        disposition: CloseDisposition,
    },
    /// Full detail: summary, layout, process tree and policy.
    GetSession {
        session_id: SessionId,
    },
    /// Just the process tree, for a client refreshing the agent panel without
    /// pulling the layout it already has.
    GetProcessTree {
        session_id: SessionId,
    },
    /// Bounded stable/redacted facts for the Quick Preview overlay. Never raw
    /// terminal bytes, terminal grids, scrollback or conversation history.
    GetPreviewHistory {
        session_id: SessionId,
        node_id: NodeId,
        /// The daemon clamps this to its protocol/store limit (currently 20).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u16>,
    },
    /// Changes whether the compact activity preview for one Process is exposed.
    /// Hiding affects both the hierarchy projection and Quick Preview history;
    /// raw terminal ownership and Process lifetime are unchanged.
    SetPreviewVisibility {
        session_id: SessionId,
        node_id: NodeId,
        visibility: PreviewVisibility,
    },
    /// Builds a bounded, redacted draft from one Agent's stable semantic activity.
    /// It creates an ephemeral, client-bound capability but touches no PTY; nothing
    /// reaches the destination until a separate [`Request::DeliverContextHandoff`].
    PrepareContextHandoff {
        session_id: SessionId,
        source_node_id: NodeId,
        target_node_id: NodeId,
        #[serde(default)]
        mode: ContextHandoffMode,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        instruction: Option<ContextHandoffText>,
    },
    /// Types the exact reviewed handoff into a live Agent PTY. The daemon revalidates
    /// both endpoints and the retained exact body without modifying it.
    DeliverContextHandoff {
        session_id: SessionId,
        /// Capability returned by `prepare_context_handoff`. The daemon retains the
        /// exact reviewed body, so a retry cannot alter or duplicate the transfer.
        handoff_id: HandoffId,
    },

    // ----------------------------------------------------------------- templates
    ListTemplates,
    /// Loads the complete editable definition only when an editor asks for it. The ordinary
    /// list remains a bounded, non-secret summary suitable for pickers.
    GetTemplate {
        template_id: TemplateId,
    },
    /// Creates a reusable layout before any Session exists. The daemon strips
    /// runtime bindings, validates the bounded tree and stores its own Template.
    CreateLayoutTemplate {
        name: String,
        layout: Box<Layout>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
    /// Creates the complete definition produced by the Template editor. Client-supplied id,
    /// creation time and built-in ownership are ignored by the daemon.
    CreateTemplate {
        template: Box<Template>,
    },
    /// Captures a session's current pane arrangement as a reusable template.
    /// Process bindings are stripped; a template describes what to start.
    SaveLayoutAsTemplate {
        session_id: SessionId,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hotkey: Option<String>,
    },
    /// Replaces one user-owned Template with the complete draft shown in the editor. The
    /// daemon preserves identity/creation time and refuses attempts to modify a built-in.
    UpdateTemplate {
        template_id: TemplateId,
        template: Box<Template>,
    },
    /// Copies a Template without carrying any live process identity.
    DuplicateTemplate {
        template_id: TemplateId,
        name: String,
    },
    /// Deletes a user-owned Template. Existing Sessions deliberately keep their own Layout.
    DeleteTemplate {
        template_id: TemplateId,
    },
    /// Chooses the Template preselected for one Workspace. `None` restores the Global/fallback
    /// choice rather than copying that choice into every Workspace.
    SetWorkspaceDefaultTemplate {
        workspace_id: WorkspaceId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        template_id: Option<TemplateId>,
    },
    /// Applies a Template to a stopped Session. A running Session is refused so changing a
    /// layout can never become implicit process termination.
    ApplyTemplateToSession {
        session_id: SessionId,
        template_id: TemplateId,
    },

    // --------------------------------------------------------------------- panes
    SplitPane {
        session_id: SessionId,
        pane_id: PaneId,
        direction: Direction,
        pane: NewPane,
    },
    /// Creates a user-defined Pane at one explicit place. Unlike opening an
    /// existing node, this may materialise the supplied command.
    CreatePane {
        session_id: SessionId,
        target_pane_id: PaneId,
        placement: PanePlacement,
        pane: NewPane,
    },
    ClosePane {
        session_id: SessionId,
        pane_id: PaneId,
        disposition: CloseDisposition,
    },
    /// Moves the divider. `delta` is a fraction of the parent split, positive to
    /// grow this pane, and is clamped so no pane can be resized out of existence.
    ResizePane {
        session_id: SessionId,
        pane_id: PaneId,
        delta: f32,
    },
    /// Moves one exact divider. Both adjacent Pane ids are required because a
    /// divider may separate nested subtrees rather than direct leaf siblings.
    ResizeDivider {
        session_id: SessionId,
        before: PaneId,
        after: PaneId,
        delta: f32,
    },
    /// Double-click behaviour: equal shares for the split owning this divider.
    EqualizeDivider {
        session_id: SessionId,
        before: PaneId,
        after: PaneId,
    },
    /// Applies a closed, geometry-only shape to the existing Panes.
    ApplyLayoutPreset {
        session_id: SessionId,
        preset: LayoutPreset,
    },
    FocusPane {
        session_id: SessionId,
        target: FocusTarget,
    },
    /// Moves an existing pane so that it sits beside another one.
    ///
    /// The operation behind dragging a pane onto a drop zone, and the one that lets a
    /// layout change *shape*: a row can become a column, and a pane can leave one
    /// split for another. `zone` says which side of `target` the moved pane lands on,
    /// or `centre` to exchange the two.
    ///
    /// It starts and stops nothing. A pane moving is a view change, and the runtime
    /// behind it never learns it happened — which is what makes rearranging a session
    /// full of running agents safe.
    RelocatePane {
        session_id: SessionId,
        moved: PaneId,
        target: PaneId,
        zone: DropZone,
    },
    /// The exchange-in-place case of [`Request::RelocatePane`], which is
    /// `zone: "centre"`.
    ///
    /// Kept because it is a shipped wire name and removing it would break a client
    /// that already speaks it for no gain. It is not a second implementation: the
    /// daemon answers it through the same relocation, so there is one behaviour with
    /// two spellings rather than two behaviours that can drift apart. New clients
    /// should send `relocate_pane`.
    SwapPanes {
        session_id: SessionId,
        a: PaneId,
        b: PaneId,
    },
    /// Toggles. The layout tree is untouched, so un-zooming restores the exact
    /// previous geometry.
    ZoomPane {
        session_id: SessionId,
        pane_id: PaneId,
    },
    /// Creates a surface-scoped binding without mutating the saved Layout or the
    /// Process lifetime. A semantic-only node returns Preview/Details capability.
    OpenNodeAsTemporaryPane {
        surface_id: String,
        session_id: SessionId,
        node_id: NodeId,
    },
    /// Explicitly opens a Process/Agent view and chooses whether it replaces,
    /// splits or remains temporary. Only the temporary choice is surface-scoped.
    OpenNodeAsPane {
        surface_id: String,
        session_id: SessionId,
        node_id: NodeId,
        target_pane_id: PaneId,
        placement: PanePlacement,
    },
    /// Makes the currently visible temporary view durable without restarting or
    /// reparenting the Process behind it.
    PromoteTemporaryPane {
        surface_id: String,
        session_id: SessionId,
        pane_id: PaneId,
        target_pane_id: PaneId,
        placement: PanePlacement,
    },
    /// Adds a second view of the same Pane/Process beside the original.
    DuplicatePane {
        session_id: SessionId,
        pane_id: PaneId,
    },
    /// Changes only the view renderer. Process identity and lifetime are stable.
    ChangePaneKind {
        session_id: SessionId,
        pane_id: PaneId,
        kind: PaneKind,
    },
    /// Renders a Pane as a persistent floating window while retaining its dock
    /// position in the split tree.
    FloatPane {
        session_id: SessionId,
        pane_id: PaneId,
        geometry: PaneGeometry,
    },
    DockPane {
        session_id: SessionId,
        pane_id: PaneId,
    },
    SetFloatingPaneGeometry {
        session_id: SessionId,
        pane_id: PaneId,
        geometry: PaneGeometry,
    },
    /// Chooses an existing binding for this surface. It never opens one
    /// implicitly; an empty focus result is a normal outcome.
    FocusPaneForNode {
        surface_id: String,
        session_id: SessionId,
        node_id: NodeId,
    },
    /// Focuses the Pane through which one exact semantic attention subject can
    /// actually be answered. The subject remains selected and attributed to
    /// itself; the daemon may route input to a trusted runtime-owning ancestor
    /// when the subject has neither a Pane nor an attachable runtime of its own.
    /// It never creates a Pane implicitly.
    FocusPaneForAttention {
        surface_id: String,
        session_id: SessionId,
        subject_node_id: NodeId,
    },

    /// Subscribes to a pane's screen and returns the current one.
    ///
    /// This is the request that makes process survival visible: the daemon has
    /// been holding the pty all along, and attaching hands over the screen it has
    /// been keeping, so a restarted UI looks exactly as it did.
    AttachPane {
        session_id: SessionId,
        pane_id: PaneId,
        /// The size the client will render at. Applied to the pty before the screen
        /// is taken, so what comes back matches the client's geometry. Refused with
        /// `invalid_argument` when `rows * cols` exceeds the `max_screen_cells` the
        /// handshake announced.
        size: PtySize,
        /// Cells or bytes. Absent means cells, which is what a renderer without its
        /// own terminal emulator needs and what the daemon can supply for nothing,
        /// having already parsed the screen.
        #[serde(default)]
        stream: PaneStream,
    },
    /// Asks for a pane's whole screen again, after missing an update.
    ///
    /// The recovery path for a cells attachment: `seq` on
    /// [`ServerEvent::PaneScreen`](crate::ServerEvent::PaneScreen) increases by one
    /// per update, so a client that sees a jump knows it has missed one and that
    /// applying the next row diff to its stale copy would leave the two screens
    /// disagreeing. This is not the only repair — the daemon notices its own dropped
    /// frame and makes the next update a whole screen — but it is the one a client
    /// can use immediately rather than waiting for the pane to produce output.
    ResyncPane {
        session_id: SessionId,
        pane_id: PaneId,
    },
    /// Fetches the pixels of one inline image a pane's screen refers to.
    ///
    /// Images are the one thing in this protocol that is *pulled* rather than pushed, and
    /// the reason is bandwidth. A screen carries only the small table saying which slot
    /// holds which [`ImageId`](crate::ImageId); a payload is up to four mebibytes of RGBA
    /// and would otherwise be resent every time the picture scrolled. So a client fetches
    /// each id once, caches it, and a re-attaching client asks only for the ids it does
    /// not already hold.
    ///
    /// Answered with `not_found` when the pane's store no longer has that id: an image
    /// that scrolled out of the daemon's bounded store is gone, and the honest answer is
    /// to say so rather than to hand back a different picture.
    PaneImage {
        session_id: SessionId,
        pane_id: PaneId,
        image_id: crate::images::ImageId,
    },
    /// Stops the output stream for a pane. The process keeps running.
    DetachPane {
        session_id: SessionId,
        pane_id: PaneId,
    },
    /// A screen-shaped window of a pane's history, as cells.
    ///
    /// The scrollback belongs to the daemon, because the daemon is the only thing that
    /// has it: a client is sent the *screen*, and a pane that printed five hundred lines
    /// between two coalesced updates never sent the four hundred and eighty in the
    /// middle. Reading history is therefore a request rather than something a client
    /// reconstructs from what it happened to watch.
    ///
    /// `offset` is rows above the top of the live screen and is clamped to what the
    /// daemon still holds, so scrolling past the beginning shows the oldest window rather
    /// than failing. The answer says which offset it actually is and how deep the record
    /// goes.
    GetPaneHistory {
        session_id: SessionId,
        pane_id: PaneId,
        #[serde(default)]
        offset: usize,
    },
    /// Searches everything the daemon retains for a pane: the history, then the live
    /// screen.
    ///
    /// Answered by the daemon for the same reason as [`Request::GetPaneHistory`]. The
    /// query is bounded ([`SearchQuery`]) and so is the answer, which says when a cap
    /// stopped it rather than implying it counted every match.
    SearchPane {
        session_id: SessionId,
        pane_id: PaneId,
        query: SearchQuery,
    },

    // ----------------------------------------------------------------------- pty
    /// Keystrokes or pasted text. Addressed to the node, not the pane: the pty
    /// belongs to the process, and one process may be shown in more than one place.
    WritePty {
        session_id: SessionId,
        node_id: NodeId,
        data: TerminalBytes,
    },
    ResizePty {
        session_id: SessionId,
        node_id: NodeId,
        size: PtySize,
    },

    // -------------------------------------------------------------- node control
    /// Sends the interrupt character through the tty, so it reaches the whole
    /// foreground process group rather than only the process we spawned.
    InterruptNode {
        session_id: SessionId,
        node_id: NodeId,
    },
    TerminateNode {
        session_id: SessionId,
        node_id: NodeId,
    },
    KillNode {
        session_id: SessionId,
        node_id: NodeId,
    },
    /// Starts a process again, because the user asked. Turn never relaunches on
    /// its own — not on restore, not after a crash — so this request is the only
    /// path back and it always originates with a human.
    RelaunchNode {
        session_id: SessionId,
        node_id: NodeId,
        /// Resume the agent's previous conversation where the tool supports it,
        /// rather than starting a fresh one.
        #[serde(default)]
        resume: bool,
    },

    // ----------------------------------------------------------------- attention
    /// Peeks at the demand the user should handle next, without acting on it.
    NextAttention,
    /// The whole queue, for the attention panel.
    ListAttention {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<SessionId>,
    },
    /// Jumps to a demand and marks it acknowledged. `None` means the next one.
    ///
    /// This is a user-initiated move, so it bypasses the focus governor's guards —
    /// pressing the shortcut is consent.
    GotoAttention {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        attention_id: Option<AttentionId>,
    },
    /// Marks as seen without resolving. It stays in the queue, ranked lower.
    AcknowledgeAttention {
        attention_id: AttentionId,
    },
    SnoozeAttention {
        attention_id: AttentionId,
        until_ms: i64,
    },
    /// Changes one queued demand's explicit ranking adjustment. The daemon persists the
    /// new value before publishing the reordered queue.
    SetAttentionPriority {
        attention_id: AttentionId,
        priority_boost: i16,
    },
    DismissAttention {
        attention_id: AttentionId,
    },
    /// Silences a session until a deadline. `None` unmutes. A muted session still
    /// badges, so nothing is lost — only quietened.
    MuteSession {
        session_id: SessionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        until_ms: Option<i64>,
    },
    /// The user fixing a state Turn got wrong.
    ///
    /// The resulting event is recorded with
    /// [`EventSource::UserCorrection`](turn_core::event::EventSource::UserCorrection)
    /// at explicit confidence, because on the question of what is actually
    /// happening in their terminal the human outranks every heuristic.
    CorrectState {
        session_id: SessionId,
        node_id: NodeId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lifecycle: Option<Lifecycle>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn: Option<Turn>,
        /// Why, in the user's words. Kept for working out which rule misfired.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },

    // ------------------------------------------------------------ user behaviour
    /// Tells the daemon what the user is doing, which is what the focus governor
    /// needs to decide whether it may move them.
    ///
    /// Sent on a change, not on a timer: the interesting transitions are the first
    /// keystroke of a burst, the window losing focus, and a modal opening. The
    /// daemon derives "is typing" from `last_keystroke_ms` rather than trusting a
    /// boolean the client might forget to clear.
    UpdateUserActivity {
        context: UserContext,
    },

    // ------------------------------------------------------------------- settings
    /// Every preference in force for one Session, with where each value came from.
    ///
    /// Asked per Session rather than globally because that is the question a settings
    /// surface has: the answer for a Session in one Workspace is not the answer for a
    /// Session in another, and the levels that make the difference are the daemon's to
    /// assemble. `session_id` absent means "the Global level alone", which is what the
    /// preferences sheet shows before any Session is selected.
    GetSettings {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<SessionId>,
    },
    /// Records one preference at one level.
    ///
    /// The level is explicit and never inferred from what is selected. "Set the font size"
    /// is four different acts depending on whether it means this Session, this Workspace,
    /// this Template or everywhere, and a request that guessed would be the one that
    /// silently edited the wrong one.
    SetSetting {
        scope: SettingsScope,
        /// The Workspace, Template or Session the level belongs to. Ignored for the Global
        /// level, which has exactly one owner.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        owner_id: Option<String>,
        key: String,
        value: serde_json::Value,
    },
    /// Removes one level's opinion, so the level below is in force again.
    ///
    /// "Reset to inherited". A removal rather than a write of the inherited value: writing it
    /// would freeze today's inherited answer as tomorrow's override.
    ResetSetting {
        scope: SettingsScope,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        owner_id: Option<String>,
        key: String,
    },
}

impl Request {
    /// The stable wire name of the operation.
    pub fn op(&self) -> &'static str {
        match self {
            Request::ListWorkspaces { .. } => "list_workspaces",
            Request::CreateWorkspace { .. } => "create_workspace",
            Request::RenameWorkspace { .. } => "rename_workspace",
            Request::ArchiveWorkspace { .. } => "archive_workspace",
            Request::DuplicateWorkspace { .. } => "duplicate_workspace",
            Request::CloseWorkspace { .. } => "close_workspace",
            Request::DeleteWorkspace { .. } => "delete_workspace",
            Request::GetHierarchy { .. } => "get_hierarchy",
            Request::GetInspector { .. } => "get_inspector",
            Request::SetTreeExpanded { .. } => "set_tree_expanded",
            Request::SelectTreeNode { .. } => "select_tree_node",
            Request::SetTreeExpandedAll { .. } => "set_tree_expanded_all",
            Request::SetTreePresentation { .. } => "set_tree_presentation",
            Request::MoveTreeNode { .. } => "move_tree_node",
            Request::RenameNode { .. } => "rename_node",
            Request::CorrectRelationship { .. } => "correct_relationship",
            Request::GetWorkspaceWriteLease { .. } => "get_workspace_write_lease",
            Request::AcquireWorkspaceWriteLease { .. } => "acquire_workspace_write_lease",
            Request::ReleaseWorkspaceWriteLease { .. } => "release_workspace_write_lease",
            Request::ListSessions { .. } => "list_sessions",
            Request::CreateSession { .. } => "create_session",
            Request::CreateReadOnlySession { .. } => "create_read_only_session",
            Request::CreateReadOnlySessionFromTemplate { .. } => {
                "create_read_only_session_from_template"
            }
            Request::CreateWorktreeSession { .. } => "create_worktree_session",
            Request::CreateWorktreeSessionFromTemplate { .. } => {
                "create_worktree_session_from_template"
            }
            Request::CreateSessionFromTemplate { .. } => "create_session_from_template",
            Request::RenameSession { .. } => "rename_session",
            Request::ArchiveSession { .. } => "archive_session",
            Request::DuplicateSession { .. } => "duplicate_session",
            Request::CloseSession { .. } => "close_session",
            Request::DeleteSession { .. } => "delete_session",
            Request::GetSession { .. } => "get_session",
            Request::GetProcessTree { .. } => "get_process_tree",
            Request::GetPreviewHistory { .. } => "get_preview_history",
            Request::SetPreviewVisibility { .. } => "set_preview_visibility",
            Request::PrepareContextHandoff { .. } => "prepare_context_handoff",
            Request::DeliverContextHandoff { .. } => "deliver_context_handoff",
            Request::ListTemplates => "list_templates",
            Request::GetTemplate { .. } => "get_template",
            Request::CreateLayoutTemplate { .. } => "create_layout_template",
            Request::CreateTemplate { .. } => "create_template",
            Request::SaveLayoutAsTemplate { .. } => "save_layout_as_template",
            Request::UpdateTemplate { .. } => "update_template",
            Request::DuplicateTemplate { .. } => "duplicate_template",
            Request::DeleteTemplate { .. } => "delete_template",
            Request::SetWorkspaceDefaultTemplate { .. } => "set_workspace_default_template",
            Request::ApplyTemplateToSession { .. } => "apply_template_to_session",
            Request::SplitPane { .. } => "split_pane",
            Request::CreatePane { .. } => "create_pane",
            Request::ClosePane { .. } => "close_pane",
            Request::ResizePane { .. } => "resize_pane",
            Request::ResizeDivider { .. } => "resize_divider",
            Request::EqualizeDivider { .. } => "equalize_divider",
            Request::ApplyLayoutPreset { .. } => "apply_layout_preset",
            Request::FocusPane { .. } => "focus_pane",
            Request::RelocatePane { .. } => "relocate_pane",
            Request::SwapPanes { .. } => "swap_panes",
            Request::ZoomPane { .. } => "zoom_pane",
            Request::OpenNodeAsTemporaryPane { .. } => "open_node_as_temporary_pane",
            Request::OpenNodeAsPane { .. } => "open_node_as_pane",
            Request::PromoteTemporaryPane { .. } => "promote_temporary_pane",
            Request::DuplicatePane { .. } => "duplicate_pane",
            Request::ChangePaneKind { .. } => "change_pane_kind",
            Request::FloatPane { .. } => "float_pane",
            Request::DockPane { .. } => "dock_pane",
            Request::SetFloatingPaneGeometry { .. } => "set_floating_pane_geometry",
            Request::FocusPaneForNode { .. } => "focus_pane_for_node",
            Request::FocusPaneForAttention { .. } => "focus_pane_for_attention",
            Request::AttachPane { .. } => "attach_pane",
            Request::ResyncPane { .. } => "resync_pane",
            Request::PaneImage { .. } => "pane_image",
            Request::DetachPane { .. } => "detach_pane",
            Request::GetPaneHistory { .. } => "get_pane_history",
            Request::SearchPane { .. } => "search_pane",
            Request::WritePty { .. } => "write_pty",
            Request::ResizePty { .. } => "resize_pty",
            Request::InterruptNode { .. } => "interrupt_node",
            Request::TerminateNode { .. } => "terminate_node",
            Request::KillNode { .. } => "kill_node",
            Request::RelaunchNode { .. } => "relaunch_node",
            Request::NextAttention => "next_attention",
            Request::ListAttention { .. } => "list_attention",
            Request::GotoAttention { .. } => "goto_attention",
            Request::AcknowledgeAttention { .. } => "acknowledge_attention",
            Request::SnoozeAttention { .. } => "snooze_attention",
            Request::SetAttentionPriority { .. } => "set_attention_priority",
            Request::DismissAttention { .. } => "dismiss_attention",
            Request::MuteSession { .. } => "mute_session",
            Request::CorrectState { .. } => "correct_state",
            Request::UpdateUserActivity { .. } => "update_user_activity",
            Request::GetSettings { .. } => "get_settings",
            Request::SetSetting { .. } => "set_setting",
            Request::ResetSetting { .. } => "reset_setting",
        }
    }

    /// The `result` tag of the response this request produces on success.
    ///
    /// The typed half of the contract. A client can assert on it, and a test in
    /// this crate checks every name here exists in the response catalogue — so the
    /// documented pairing cannot drift from the code.
    pub fn expected_result(&self) -> &'static str {
        match self {
            Request::ListWorkspaces { .. } => "workspaces",
            Request::CreateWorkspace { .. }
            | Request::RenameWorkspace { .. }
            | Request::ArchiveWorkspace { .. }
            | Request::DuplicateWorkspace { .. } => "workspace",
            Request::CloseWorkspace { .. } | Request::DeleteWorkspace { .. } => "closed",

            Request::GetHierarchy { .. } => "hierarchy",
            Request::GetInspector { .. } => "inspector",
            Request::SetTreeExpanded { .. }
            | Request::SelectTreeNode { .. }
            | Request::SetTreeExpandedAll { .. }
            | Request::SetTreePresentation { .. }
            | Request::MoveTreeNode { .. } => "tree_state",
            Request::RenameNode { .. } | Request::CorrectRelationship { .. } => "node",
            Request::GetWorkspaceWriteLease { .. }
            | Request::AcquireWorkspaceWriteLease { .. }
            | Request::ReleaseWorkspaceWriteLease { .. } => "workspace_write_lease",

            Request::ListSessions { .. } => "sessions",
            Request::CreateSession { .. }
            | Request::CreateReadOnlySession { .. }
            | Request::CreateReadOnlySessionFromTemplate { .. }
            | Request::CreateWorktreeSession { .. }
            | Request::CreateWorktreeSessionFromTemplate { .. }
            | Request::CreateSessionFromTemplate { .. }
            | Request::RenameSession { .. }
            | Request::ArchiveSession { .. }
            | Request::DuplicateSession { .. } => "session",
            Request::CloseSession { .. } | Request::DeleteSession { .. } => "closed",
            Request::GetSession { .. } => "session_details",
            Request::GetProcessTree { .. } => "tree",
            Request::GetPreviewHistory { .. } => "preview_history",
            Request::SetPreviewVisibility { .. } => "ack",
            Request::PrepareContextHandoff { .. } => "context_handoff",
            Request::DeliverContextHandoff { .. } => "ack",

            Request::ListTemplates => "templates",
            Request::GetTemplate { .. } => "template_details",
            Request::CreateLayoutTemplate { .. }
            | Request::CreateTemplate { .. }
            | Request::SaveLayoutAsTemplate { .. }
            | Request::UpdateTemplate { .. }
            | Request::DuplicateTemplate { .. } => "template",
            Request::DeleteTemplate { .. } => "templates",
            Request::SetWorkspaceDefaultTemplate { .. } => "workspace",
            Request::ApplyTemplateToSession { .. } => "session",

            // Every pane operation answers with the layout it produced, so the UI
            // re-renders from the daemon's version rather than its own optimistic
            // guess at what a split does.
            Request::SplitPane { .. }
            | Request::CreatePane { .. }
            | Request::ClosePane { .. }
            | Request::ResizePane { .. }
            | Request::ResizeDivider { .. }
            | Request::EqualizeDivider { .. }
            | Request::ApplyLayoutPreset { .. }
            | Request::FocusPane { .. }
            | Request::RelocatePane { .. }
            | Request::SwapPanes { .. }
            | Request::ZoomPane { .. }
            | Request::OpenNodeAsPane { .. }
            | Request::PromoteTemporaryPane { .. }
            | Request::DuplicatePane { .. }
            | Request::ChangePaneKind { .. }
            | Request::FloatPane { .. }
            | Request::DockPane { .. }
            | Request::SetFloatingPaneGeometry { .. } => "layout",
            Request::OpenNodeAsTemporaryPane { .. } => "node_pane",
            Request::FocusPaneForNode { .. } | Request::FocusPaneForAttention { .. } => {
                "pane_focus"
            }
            Request::AttachPane { .. } => "attached",
            Request::ResyncPane { .. } => "screen",
            Request::PaneImage { .. } => "pane_image",
            Request::DetachPane { .. } => "ack",
            Request::GetPaneHistory { .. } => "pane_history",
            Request::SearchPane { .. } => "pane_matches",

            Request::WritePty { .. } | Request::ResizePty { .. } => "ack",

            Request::InterruptNode { .. }
            | Request::TerminateNode { .. }
            | Request::KillNode { .. } => "ack",
            Request::RelaunchNode { .. } => "node",

            Request::NextAttention => "attention",
            Request::ListAttention { .. } => "attention_list",
            Request::GotoAttention { .. } => "effects",
            Request::AcknowledgeAttention { .. }
            | Request::SnoozeAttention { .. }
            | Request::SetAttentionPriority { .. }
            | Request::DismissAttention { .. }
            | Request::MuteSession { .. } => "ack",
            Request::CorrectState { .. } => "node",

            // The governor may release a deferred focus jump the moment the user
            // stops typing, so even this returns effects rather than an ack.
            Request::UpdateUserActivity { .. } => "effects",

            // A write answers with the whole resolved set rather than an ack, for the same
            // reason a pane operation answers with the layout: one change can move what is
            // in force for several keys at once — a Session override removed reveals a
            // Workspace value — and a client that patched its own copy would be a second
            // resolver able to disagree with the daemon's.
            Request::GetSettings { .. }
            | Request::SetSetting { .. }
            | Request::ResetSetting { .. } => "settings",
        }
    }

    /// Whether this request changes daemon state.
    ///
    /// Useful for a client that wants to replay reads after a reconnect without
    /// re-running writes, and for the daemon's own logging.
    pub fn is_mutating(&self) -> bool {
        !matches!(
            self,
            Request::ListWorkspaces { .. }
                | Request::GetHierarchy { .. }
                | Request::GetInspector { .. }
                | Request::GetWorkspaceWriteLease { .. }
                | Request::ListSessions { .. }
                | Request::GetSession { .. }
                | Request::GetProcessTree { .. }
                | Request::GetPreviewHistory { .. }
                | Request::ListTemplates
                | Request::GetTemplate { .. }
                | Request::NextAttention
                | Request::ListAttention { .. }
                // Asking for the screen again changes nothing about it: the daemon
                // hands over what it already has.
                | Request::ResyncPane { .. }
                // Nor does asking for the pixels of a picture already on that screen.
                | Request::PaneImage { .. }
                // Reading history and searching it are reads. Neither moves the pane's
                // own viewport: the daemon restores the offset it borrowed, so one
                // client's search cannot scroll another client's screen.
                | Request::GetPaneHistory { .. }
                | Request::SearchPane { .. }
                | Request::GetSettings { .. }
        )
    }

    /// The session a request concerns, when it concerns exactly one.
    ///
    /// Lets the daemon route a request to the right session actor without a match
    /// arm per operation.
    pub fn session_id(&self) -> Option<&SessionId> {
        match self {
            Request::RenameSession { session_id, .. }
            | Request::ArchiveSession { session_id, .. }
            | Request::DuplicateSession { session_id }
            | Request::CloseSession { session_id, .. }
            | Request::GetSession { session_id }
            | Request::GetProcessTree { session_id }
            | Request::GetPreviewHistory { session_id, .. }
            | Request::SetPreviewVisibility { session_id, .. }
            | Request::PrepareContextHandoff { session_id, .. }
            | Request::DeliverContextHandoff { session_id, .. }
            | Request::SaveLayoutAsTemplate { session_id, .. }
            | Request::ApplyTemplateToSession { session_id, .. }
            | Request::SplitPane { session_id, .. }
            | Request::CreatePane { session_id, .. }
            | Request::ClosePane { session_id, .. }
            | Request::ResizePane { session_id, .. }
            | Request::ResizeDivider { session_id, .. }
            | Request::EqualizeDivider { session_id, .. }
            | Request::ApplyLayoutPreset { session_id, .. }
            | Request::FocusPane { session_id, .. }
            | Request::RelocatePane { session_id, .. }
            | Request::SwapPanes { session_id, .. }
            | Request::ZoomPane { session_id, .. }
            | Request::OpenNodeAsTemporaryPane { session_id, .. }
            | Request::OpenNodeAsPane { session_id, .. }
            | Request::PromoteTemporaryPane { session_id, .. }
            | Request::DuplicatePane { session_id, .. }
            | Request::ChangePaneKind { session_id, .. }
            | Request::FloatPane { session_id, .. }
            | Request::DockPane { session_id, .. }
            | Request::SetFloatingPaneGeometry { session_id, .. }
            | Request::FocusPaneForNode { session_id, .. }
            | Request::FocusPaneForAttention { session_id, .. }
            | Request::AttachPane { session_id, .. }
            | Request::ResyncPane { session_id, .. }
            | Request::PaneImage { session_id, .. }
            | Request::DetachPane { session_id, .. }
            | Request::GetPaneHistory { session_id, .. }
            | Request::SearchPane { session_id, .. }
            | Request::WritePty { session_id, .. }
            | Request::ResizePty { session_id, .. }
            | Request::InterruptNode { session_id, .. }
            | Request::TerminateNode { session_id, .. }
            | Request::KillNode { session_id, .. }
            | Request::RelaunchNode { session_id, .. }
            | Request::MuteSession { session_id, .. }
            | Request::CorrectState { session_id, .. } => Some(session_id),
            Request::RenameNode { session_id, .. }
            | Request::CorrectRelationship { session_id, .. } => Some(session_id),
            Request::AcquireWorkspaceWriteLease { session_id, .. } => Some(session_id),
            Request::ListAttention { session_id } => session_id.as_ref(),
            _ => None,
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;
