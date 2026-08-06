//! Server pushes: everything the daemon tells the UI without being asked.
//!
//! The request/response half of the protocol handles what the user does. This half
//! handles what the *agents* do, which is the interesting half — the whole point
//! of the product is that thirty processes are getting on with things while the
//! user looks at one of them.
//!
//! Pushes are addressed but not correlated: they carry no request id because no
//! request caused them. A client processes them in arrival order and treats each
//! as the current truth about what it names.

use serde::{Deserialize, Serialize};
use turn_core::attention::Effect;
use turn_core::event::TurnEvent;
use turn_core::ids::{NodeId, PaneId, SessionId, WorkspaceId};
use turn_core::model::{
    ActivityPreview, Layout, PaneNodeBinding, RestoreState, WorkspaceWriteLease,
};
use turn_core::state::{DisplayState, Lifecycle, Turn};

use crate::bytes::TerminalBytes;
use crate::geometry::PtySize;
use crate::screen::ScreenUpdate;
use crate::view::{AttentionView, HierarchySnapshot, SessionSummary, TreeNodeView};

/// What happened to one pane's process when a session was restored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PaneRestoreOutcome {
    pub pane_id: PaneId,
    /// The durable Process Node whose runtime is being described. Relaunch is
    /// node-addressed, so an outcome without this identity could not be acted on.
    pub node_id: NodeId,
    /// What became of it. Current daemon restart produces `Orphaned` (alive but
    /// out of reach) or `Lost` (cannot be found). `Reconnected` is reserved for a
    /// backend that can prove PTY reattachment; the MVP does not claim it.
    pub lifecycle: Lifecycle,
    /// Set when Turn could offer to start this pane again. It is an offer: the
    /// user answers it with [`Request::RelaunchNode`](crate::Request::RelaunchNode)
    /// or does not, and nothing happens until they do.
    pub can_relaunch: bool,
    /// What the pane would run if the user accepted. It is descriptive; the
    /// authoritative relaunch target remains `node_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Whether starting this pane again would use the Session's checkout write
    /// authority.
    ///
    /// An agent, or any command the pane names, would; opening the user's own shell
    /// would not. A UI that is waiting for the user to confirm write access can
    /// therefore keep offering the panes that write nothing instead of blocking the
    /// whole Session — including the terminal they need in order to go and stop the
    /// process the confirmation is about.
    ///
    /// Defaults to `true` when absent, so an older peer's payload is read as the
    /// gated case rather than as permission.
    #[serde(default = "crate::events::needs_checkout_write_default")]
    pub needs_checkout_write: bool,
}

/// A missing `needs_checkout_write` means "assume it does": the field only ever
/// unlocks something, so its absence must not.
fn needs_checkout_write_default() -> bool {
    true
}

/// A push from the daemon.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum ServerEvent {
    /// What changed on the screen of a pane the client attached to.
    ///
    /// The default stream for a terminal pane. The daemon parses the pty once and
    /// sends the result, so there is no second VT emulator to disagree with it.
    ///
    /// `seq` is per-attachment and increases by one per update. A client that sees a
    /// jump has missed one, and applying a row diff on top of a stale screen would
    /// leave the two disagreeing — so it asks for
    /// [`Request::ResyncPane`](crate::Request::ResyncPane). It does not have to: the
    /// daemon notices its own dropped frame and makes the next update a whole screen.
    /// Both repairs exist because they fail differently — the daemon's needs the pane
    /// to produce output again, and the client's does not.
    PaneScreen {
        session_id: SessionId,
        pane_id: PaneId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_id: Option<NodeId>,
        seq: u64,
        update: ScreenUpdate,
    },

    /// Raw output from a pane attached as [`PaneStream::Bytes`](crate::PaneStream).
    ///
    /// `seq` is per-attachment and increments by one per frame, so a client can
    /// detect a gap without comparing byte counts. The daemon chunks a large read
    /// into several of these rather than emitting one frame over the line limit.
    PaneOutput {
        session_id: SessionId,
        pane_id: PaneId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_id: Option<NodeId>,
        seq: u64,
        data: TerminalBytes,
    },

    /// Output was produced that the byte-stream client did not receive.
    ///
    /// The daemon's output channel is deliberately bounded — buffering an
    /// unbounded amount for a slow client is a memory leak that looks like a
    /// feature — so a client that falls far behind loses frames. Saying so lets
    /// the UI re-attach and replay rather than render a terminal that silently
    /// missed a screenful.
    PaneOutputGap {
        session_id: SessionId,
        pane_id: PaneId,
        /// Frames lost between `resume_seq - dropped` and `resume_seq`.
        dropped: u64,
        resume_seq: u64,
    },

    /// A node's state changed on either axis.
    ///
    /// Both axes plus the derived projection are sent together. The projection is
    /// included even though it is a pure function of the other two, because a
    /// client that derived it itself would be a second implementation of the one
    /// rule this product cannot afford to get wrong.
    NodeStateChanged {
        session_id: SessionId,
        node_id: NodeId,
        lifecycle: Lifecycle,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn: Option<Turn>,
        display_state: DisplayState,
        /// The event that caused the change, when one did. `None` for a change
        /// Turn made itself — a user correction, a supervisor sweep.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caused_by: Option<Box<TurnEvent>>,
    },

    /// A session's summary changed. Sent whole rather than as a diff: a summary is
    /// small, and a client applying diffs to a stale copy is a class of bug that
    /// shows up as a sidebar that disagrees with the terminal.
    /// Boxed for the same reason as in [`crate::Response`]: the hot push is
    /// [`ServerEvent::PaneOutput`], a handful of ids and a byte buffer, and it
    /// must not be padded out to the size of a whole session summary just because
    /// they share an enum.
    SessionStateChanged { session: Box<SessionSummary> },

    /// A session was archived, closed or otherwise left the list.
    SessionRemoved {
        session_id: SessionId,
        workspace_id: WorkspaceId,
    },

    /// A normalised event, for the event log panel and for a client that wants the
    /// raw stream rather than the projections built from it.
    ///
    /// The event carries its own [`Confidence`](turn_core::event::Confidence) and
    /// [`EventSource`](turn_core::event::EventSource), which is how a UI can render
    /// a heuristic's opinion as provisional.
    /// The field is `turn_event` rather than `event`, which would collide with the
    /// envelope's own discriminator.
    TurnEventEmitted { turn_event: TurnEvent },

    /// The attention manager decided something. Passed through unchanged.
    AttentionEffect { effect: Effect },

    /// The queue changed shape, for the attention panel.
    AttentionQueueChanged { entries: Vec<AttentionView> },

    /// The process tree changed — most often because a subagent appeared or
    /// finished, which the tools report explicitly rather than leaving us to infer.
    ///
    /// The whole tree is sent, in draw order, for the same reason as
    /// `SessionStateChanged`: it is a handful of rows, and re-parenting a partial
    /// copy is where invented relationships would creep in.
    TreeChanged {
        session_id: SessionId,
        nodes: Vec<TreeNodeView>,
    },

    /// Full replacement of the one navigation projection. The embedded
    /// `surface_id` keeps expansion/selection private to that window. Structural
    /// revision gaps are repaired with `get_hierarchy`, never guessed locally.
    HierarchyChanged { snapshot: Box<HierarchySnapshot> },

    /// Coalesced replacement of one node's compact semantic preview. The
    /// hierarchy revision lets a client reject an update for a node it has not
    /// yet learned about and request a full snapshot.
    ActivityPreviewChanged {
        hierarchy_revision: u64,
        session_id: SessionId,
        node_id: NodeId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        preview: Option<ActivityPreview>,
    },

    /// Complete binding set for one node, not an add/remove delta. Pane closure
    /// does not imply Process termination.
    PaneBindingsChanged {
        hierarchy_revision: u64,
        session_id: SessionId,
        node_id: NodeId,
        bindings: Vec<PaneNodeBinding>,
    },

    /// Current lease replacement. `None` means no active lease; reconciliation
    /// state still comes from the Workspace branch in a hierarchy snapshot.
    WorkspaceWriteLeaseChanged {
        hierarchy_revision: u64,
        workspace_id: WorkspaceId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lease: Option<WorkspaceWriteLease>,
    },

    /// The pane arrangement changed for a reason the client did not ask for — a
    /// second client split a pane, or a pane's process ended and its pane closed.
    LayoutChanged {
        session_id: SessionId,
        layout: Layout,
    },

    /// A pty was resized by something other than this client, so the renderer must
    /// follow. Happens when two clients show the same session.
    PtyResized {
        session_id: SessionId,
        node_id: NodeId,
        size: PtySize,
    },

    /// What survived a restart, pane by pane.
    ///
    /// Pushed rather than answered, because a restore happens when the daemon
    /// decides — on its own start, or when it re-adopts processes — and the UI may
    /// not have asked anything yet. Nothing here has been relaunched: entries with
    /// `can_relaunch` are offers awaiting the user.
    RestoreResult {
        session_id: SessionId,
        state: RestoreState,
        /// Whether the user must be told, rather than left to notice a dead pane.
        needs_explanation: bool,
        panes: Vec<PaneRestoreOutcome>,
    },
}

impl ServerEvent {
    /// The stable `event` tag.
    pub fn event_name(&self) -> &'static str {
        match self {
            ServerEvent::PaneScreen { .. } => "pane_screen",
            ServerEvent::PaneOutput { .. } => "pane_output",
            ServerEvent::PaneOutputGap { .. } => "pane_output_gap",
            ServerEvent::NodeStateChanged { .. } => "node_state_changed",
            ServerEvent::SessionStateChanged { .. } => "session_state_changed",
            ServerEvent::SessionRemoved { .. } => "session_removed",
            ServerEvent::TurnEventEmitted { .. } => "turn_event_emitted",
            ServerEvent::AttentionEffect { .. } => "attention_effect",
            ServerEvent::AttentionQueueChanged { .. } => "attention_queue_changed",
            ServerEvent::TreeChanged { .. } => "tree_changed",
            ServerEvent::HierarchyChanged { .. } => "hierarchy_changed",
            ServerEvent::ActivityPreviewChanged { .. } => "activity_preview_changed",
            ServerEvent::PaneBindingsChanged { .. } => "pane_bindings_changed",
            ServerEvent::WorkspaceWriteLeaseChanged { .. } => "workspace_write_lease_changed",
            ServerEvent::LayoutChanged { .. } => "layout_changed",
            ServerEvent::PtyResized { .. } => "pty_resized",
            ServerEvent::RestoreResult { .. } => "restore_result",
        }
    }

    /// The session a push concerns, when it concerns one. Lets a client route to
    /// the right view without a match arm per event.
    pub fn session_id(&self) -> Option<&SessionId> {
        match self {
            ServerEvent::PaneScreen { session_id, .. }
            | ServerEvent::PaneOutput { session_id, .. }
            | ServerEvent::PaneOutputGap { session_id, .. }
            | ServerEvent::NodeStateChanged { session_id, .. }
            | ServerEvent::SessionRemoved { session_id, .. }
            | ServerEvent::TreeChanged { session_id, .. }
            | ServerEvent::ActivityPreviewChanged { session_id, .. }
            | ServerEvent::PaneBindingsChanged { session_id, .. }
            | ServerEvent::LayoutChanged { session_id, .. }
            | ServerEvent::PtyResized { session_id, .. }
            | ServerEvent::RestoreResult { session_id, .. } => Some(session_id),
            ServerEvent::SessionStateChanged { session } => Some(&session.id),
            ServerEvent::TurnEventEmitted { turn_event } => Some(&turn_event.session_id),
            ServerEvent::AttentionEffect { effect } => Some(effect_session(effect)),
            ServerEvent::AttentionQueueChanged { .. } => None,
            ServerEvent::HierarchyChanged { .. }
            | ServerEvent::WorkspaceWriteLeaseChanged { .. } => None,
        }
    }

    /// Whether this push is high-volume terminal traffic.
    ///
    /// A client may want to route it through a different path from state
    /// changes — straight into the renderer, without going through a state store
    /// that would re-render the world for every keystroke echoed back. True for a
    /// screen update as much as for bytes: it is the same traffic in a different
    /// representation, and it arrives just as often.
    pub fn is_output(&self) -> bool {
        matches!(
            self,
            ServerEvent::PaneScreen { .. }
                | ServerEvent::PaneOutput { .. }
                | ServerEvent::PaneOutputGap { .. }
        )
    }
}

/// The session an effect targets. Every [`Effect`] variant names one.
fn effect_session(effect: &Effect) -> &SessionId {
    match effect {
        Effect::Badge { session_id, .. }
        | Effect::Highlight { session_id }
        | Effect::PlaySound { session_id, .. }
        | Effect::Notify { session_id, .. }
        | Effect::Enqueued { session_id, .. }
        | Effect::Focus { session_id, .. }
        | Effect::FocusDeferred { session_id, .. }
        | Effect::FocusDenied { session_id, .. }
        | Effect::RunCustom { session_id, .. }
        | Effect::Cleared { session_id } => session_id,
    }
}

#[cfg(test)]
pub(crate) mod tests;
