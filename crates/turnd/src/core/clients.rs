//! Attached clients, and the rules for pushing to them.
//!
//! Three things here are deliberate and easy to get wrong.
//!
//! **A push is never allowed to block the core task.** Every send is a `try_send`
//! into a bounded channel. One client on a saturated socket must not stall the
//! daemon that thirty agents are reporting to, so a full channel costs that client
//! frames and nothing else.
//!
//! **Dropped output is admitted, not hidden.** A client that falls behind gets a
//! [`ServerEvent::PaneOutputGap`] as soon as there is room, which tells it to
//! re-attach and replay. The alternative — growing the queue until it fits — is a
//! memory leak that presents as a feature until the day it is not.
//!
//! **A dropped state push is repaired, not admitted.** Output is a stream, so the
//! only truthful thing to say about a lost frame is that it was lost. State is not:
//! every state push carries the whole of what it names, so a client that lost one
//! can simply be told again. Anything else leaves a UI rendering a `YOUR TURN` that
//! is no longer true, with no way back short of restarting it.

use super::command::ClientId;
use super::Core;
use std::collections::HashMap;
use tokio::sync::mpsc;
use turn_core::ids::{NodeId, PaneId, SessionId};
use turn_proto::{PaneStream, ServerEvent, ServerFrame, ServerMessage};

/// What identifies one of a client's attachments.
///
/// A pane id is only unique inside its session, so a pane alone is not an identity.
/// Keying by it means any two sessions that ever share a pane id — a duplicated
/// layout, a template instantiated twice through a path that forgot to re-mint —
/// silently share one attachment, and attaching to the second steals the first's
/// output. Carrying the session in the key makes that impossible rather than
/// depending on every caller remembering.
pub type AttachmentKey = (SessionId, PaneId);

/// What a client still has to be told about one session.
///
/// Recorded part by part rather than as "something was lost", so a repair is the one
/// push that answers it. Clearing the debt before re-sending and letting each push
/// record its own failure again is what makes a client with almost no room still make
/// progress: whatever fits is delivered and stops being owed, and the rest is retried.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SessionDebt {
    /// The sidebar row: state, badge count, restore explanation.
    pub summary: bool,
    /// Every node's lifecycle and turn, which also answers a lost node state change.
    pub tree: bool,
    pub layout: bool,
    /// The geometry of the ptys this client has attached in the session.
    pub sizes: bool,
}

impl SessionDebt {
    fn merge(&mut self, other: SessionDebt) {
        self.summary |= other.summary;
        self.tree |= other.tree;
        self.layout |= other.layout;
        self.sizes |= other.sizes;
    }

    fn summary() -> Self {
        Self {
            summary: true,
            ..Self::default()
        }
    }

    fn tree() -> Self {
        Self {
            tree: true,
            ..Self::default()
        }
    }

    fn layout() -> Self {
        Self {
            layout: true,
            ..Self::default()
        }
    }

    fn sizes() -> Self {
        Self {
            sizes: true,
            ..Self::default()
        }
    }
}

/// What re-sending would have to say to make up for losing one push.
///
/// The mapping is possible at all because every state push carries the whole of what it
/// names. Two kinds of push are deliberately unrepairable and both say so here.
fn repair_for(event: &ServerEvent) -> (Option<(SessionId, SessionDebt)>, bool, bool) {
    let session = |id: &SessionId, debt: SessionDebt| (Some((id.clone(), debt)), false, false);
    match event {
        // Output has its own admission path, which says exactly how many frames went
        // missing and lets the client replay from the pty's own buffer. A screen update
        // repairs itself in the same spirit and more cheaply: the attachment remembers
        // that it owes a whole screen, and the next update carries one — see
        // `super::screens`.
        ServerEvent::PaneScreen { .. }
        | ServerEvent::PaneOutput { .. }
        | ServerEvent::PaneOutputGap { .. } => (None, false, false),
        // History, not state. There is no "current value" of an event that already
        // happened, and the state it changed is re-sent by the pushes beside it.
        ServerEvent::TurnEventEmitted { .. } => (None, false, false),
        ServerEvent::SessionStateChanged { session: summary } => {
            session(&summary.id, SessionDebt::summary())
        }
        ServerEvent::SessionRemoved { session_id, .. } => {
            session(session_id, SessionDebt::summary())
        }
        ServerEvent::NodeStateChanged { session_id, .. }
        | ServerEvent::TreeChanged { session_id, .. } => session(session_id, SessionDebt::tree()),
        ServerEvent::LayoutChanged { session_id, .. } => session(session_id, SessionDebt::layout()),
        ServerEvent::PtyResized { session_id, .. } => session(session_id, SessionDebt::sizes()),
        // The restore explanation lives on the summary, so re-sending that is what
        // stops a user from being left to notice a dead pane on their own.
        ServerEvent::RestoreResult { session_id, .. } => {
            session(session_id, SessionDebt::summary())
        }
        // An effect is a moment, not a state: a sound that did not play cannot be
        // played late, and re-issuing a focus jump would move the user for something
        // that happened minutes ago — the one thing this daemon must never do. What is
        // repairable is what the effect changed: the badge count on the row and the
        // queue it came from.
        ServerEvent::AttentionEffect { .. } => (
            event
                .session_id()
                .cloned()
                .map(|id| (id, SessionDebt::summary())),
            true,
            false,
        ),
        ServerEvent::AttentionQueueChanged { .. } => (None, true, false),
        ServerEvent::HierarchyChanged { .. }
        | ServerEvent::ActivityPreviewChanged { .. }
        | ServerEvent::PaneBindingsChanged { .. }
        | ServerEvent::WorkspaceWriteLeaseChanged { .. } => (None, false, true),
    }
}

/// What one client is owed, taken out of the map so the re-send can borrow the core.
struct Owed {
    client: ClientId,
    sessions: Vec<(SessionId, SessionDebt)>,
    queue: bool,
    hierarchy: bool,
}

/// One pane a client is watching.
#[derive(Debug)]
pub struct Attachment {
    /// The process behind the pane, if it has one. A pane can be attached with no
    /// process — an empty slot after a partial restore, or one of Turn's own views.
    pub node_id: Option<NodeId>,
    /// Cells or bytes. Chosen at attach and fixed for the life of the attachment: a
    /// client that wants the other representation attaches again, which is also how it
    /// gets a fresh screen or replay to start from.
    pub stream: PaneStream,
    /// The `seq` the next frame for this attachment will carry, in whichever stream it
    /// asked for.
    pub next_seq: u64,
    /// Output frames this attachment has lost, not yet admitted to the client. Only
    /// meaningful for a byte stream, where the lost bytes cannot be reconstructed.
    pub owed_gap: u64,
    /// Set when a screen update was dropped, so the next one carries the whole screen.
    ///
    /// The cells equivalent of `owed_gap`, and cheaper than admitting a gap: the screen
    /// is rebuilt from the pty's own buffer every time, so there is nothing to recover
    /// — only a client to bring back into step.
    pub owes_full_screen: bool,
}

/// A connected UI.
pub struct Client {
    pub frames: mpsc::Sender<ServerFrame>,
    /// The protocol version this connection negotiated. Frames are stamped with it
    /// rather than with the daemon's newest, so a rollout window means something.
    pub agreed_version: u32,
    pub attachments: HashMap<AttachmentKey, Attachment>,
    /// Stable window identity supplied by `get_hierarchy`.
    pub surface_id: Option<String>,
    /// Frames dropped because this client was not draining. Surfaced in logs; a
    /// non-zero value means a UI that cannot keep up.
    pub dropped_frames: u64,
    /// What this client lost a state push for, and has not been told since.
    pub owed_state: HashMap<SessionId, SessionDebt>,
    /// Whether it also lost an attention queue push, which names no session.
    pub owed_queue: bool,
    /// Any dropped hierarchy-visible replacement is repaired with one full
    /// revisioned snapshot, never by replaying structural deltas.
    pub owed_hierarchy: bool,
}

impl Client {
    pub fn new(agreed_version: u32, frames: mpsc::Sender<ServerFrame>) -> Self {
        Self {
            frames,
            agreed_version,
            attachments: HashMap::new(),
            surface_id: None,
            dropped_frames: 0,
            owed_state: HashMap::new(),
            owed_queue: false,
            owed_hierarchy: false,
        }
    }

    /// Sends one event, recording what has to be said again if the frame is lost.
    fn push(&mut self, event: ServerEvent) -> bool {
        let (session, queue, hierarchy) = repair_for(&event);
        if self.send(ServerMessage::Event { event }) {
            return true;
        }
        if let Some((id, debt)) = session {
            self.owed_state.entry(id).or_default().merge(debt);
        }
        if queue {
            self.owed_queue = true;
        }
        if hierarchy && self.surface_id.is_some() {
            self.owed_hierarchy = true;
        }
        false
    }

    /// Sends one screen update. Visible to [`super::screens`], which owns the
    /// sequence numbering and the repair that follows a dropped frame.
    pub(crate) fn push_screen(&mut self, event: ServerEvent) -> bool {
        self.push(event)
    }

    /// Sends one frame, or counts it as lost.
    fn send(&mut self, message: ServerMessage) -> bool {
        let frame = ServerFrame {
            v: self.agreed_version,
            message,
        };
        match self.frames.try_send(frame) {
            Ok(()) => true,
            Err(_) => {
                self.dropped_frames += 1;
                false
            }
        }
    }

    /// Whether this client is owed anything.
    pub fn is_behind(&self) -> bool {
        self.owed_queue || self.owed_hierarchy || !self.owed_state.is_empty()
    }
}

impl Core {
    /// Pushes an event to every attached client.
    pub(crate) fn push_all(&mut self, event: ServerEvent) {
        for (id, client) in self.clients.iter_mut() {
            if !client.push(event.clone()) {
                tracing::debug!(%id, event = event.event_name(), "dropped a push: client is behind");
            }
        }
    }

    /// Pushes an event to every client except the one that caused it.
    ///
    /// The originator already has the answer in its response; sending it the push as
    /// well would make a client that renders both do the work twice, and — for a
    /// layout — briefly render an arrangement it is about to be told again.
    pub(crate) fn push_others(&mut self, except: ClientId, event: ServerEvent) {
        for (id, client) in self.clients.iter_mut() {
            if *id == except {
                continue;
            }
            client.push(event.clone());
        }
    }

    /// Pushes to one client.
    pub(crate) fn push_to(&mut self, client: ClientId, event: ServerEvent) {
        if let Some(client) = self.clients.get_mut(&client) {
            client.push(event);
        }
    }

    /// Registers a client and brings it up to date.
    ///
    /// The catch-up matters: a daemon restores on start-up, long before any UI
    /// exists, so the restore report has to be replayed to whoever connects next.
    /// Without this the user would open Turn to a pane that is quietly dead.
    pub(crate) fn client_opened(
        &mut self,
        client: ClientId,
        agreed_version: u32,
        frames: mpsc::Sender<ServerFrame>,
    ) {
        self.clients
            .insert(client, Client::new(agreed_version, frames));
        tracing::info!(%client, clients = self.clients.len(), "client attached");

        for report in self.restore_reports.clone() {
            self.push_to(client, report);
        }
        let now = turn_core::now_ms();
        let entries = self.attention_views(now);
        if !entries.is_empty() {
            self.push_to(client, ServerEvent::AttentionQueueChanged { entries });
        }
    }

    /// Drops a client's attachments. Its processes keep running: that is the whole
    /// point of the daemon, and closing a window is not an instruction to stop work.
    pub(crate) fn client_closed(&mut self, client: ClientId) {
        let Some(gone) = self.clients.remove(&client) else {
            return;
        };
        let abandoned_surface = gone.surface_id.as_deref().filter(|surface_id| {
            !self
                .clients
                .values()
                .any(|remaining| remaining.surface_id.as_deref() == Some(*surface_id))
        });
        if let Some(surface_id) = abandoned_surface {
            match self
                .store
                .hierarchy()
                .clear_temporary_bindings_for_surface(surface_id)
            {
                Ok(0) => {}
                Ok(pruned) => {
                    self.bump_hierarchy();
                    tracing::debug!(surface_id, pruned, "pruned abandoned temporary panes");
                }
                Err(error) => {
                    tracing::warn!(surface_id, %error, "could not prune abandoned temporary panes");
                }
            }
        }
        tracing::info!(
            %client,
            panes = gone.attachments.len(),
            dropped_frames = gone.dropped_frames,
            "client detached"
        );
        for attachment in gone.attachments.values() {
            if let Some(node) = &attachment.node_id {
                self.stop_pump_if_unwatched(node);
            }
        }
    }

    /// Whether any client is still watching a node.
    pub(crate) fn is_watched(&self, node: &NodeId) -> bool {
        self.clients.values().any(|client| {
            client
                .attachments
                .values()
                .any(|a| a.node_id.as_ref() == Some(node))
        })
    }

    /// Forgets every client's attachment to one pane of one session.
    ///
    /// Called when the pane itself goes away, which is not a per-client decision: the
    /// pane a second window was showing does not survive the first window closing it.
    pub(crate) fn detach_everyone(&mut self, session_id: &SessionId, pane_id: &PaneId) {
        let key = (session_id.clone(), pane_id.clone());
        for client in self.clients.values_mut() {
            client.attachments.remove(&key);
        }
    }

    /// Tells clients that lost a state push what is true now.
    ///
    /// The repair is to say it again rather than to keep the frame that was dropped:
    /// what a client that fell behind needs is not the frame it missed but the state it
    /// should be in, and every state push carries the whole of what it names.
    ///
    /// The debt is cleared first and each push records its own failure again, so a
    /// client with room for one frame is repaired one part per tick instead of being
    /// sent the same first frame forever. The summary goes first because it is the row
    /// that says `YOUR TURN`.
    pub(crate) fn resync_clients(&mut self, now_ms: i64) {
        let behind: Vec<Owed> = self
            .clients
            .iter()
            .filter(|(_, client)| client.is_behind())
            .map(|(id, client)| Owed {
                client: *id,
                sessions: client
                    .owed_state
                    .iter()
                    .map(|(session, debt)| (session.clone(), *debt))
                    .collect(),
                queue: client.owed_queue,
                hierarchy: client.owed_hierarchy,
            })
            .collect();

        for Owed {
            client: id,
            sessions,
            queue,
            hierarchy,
        } in behind
        {
            if let Some(client) = self.clients.get_mut(&id) {
                client.owed_state.clear();
                client.owed_queue = false;
                client.owed_hierarchy = false;
            }
            tracing::debug!(
                client = %id, sessions = sessions.len(), queue, hierarchy,
                "re-sending state to a client that fell behind"
            );
            for (session_id, debt) in sessions {
                self.resync_session(id, &session_id, debt, now_ms);
            }
            if queue {
                let entries = self.attention_views(now_ms);
                self.push_to(id, ServerEvent::AttentionQueueChanged { entries });
            }
            if hierarchy {
                self.push_hierarchy_to(id, now_ms);
            }
        }
    }

    /// Re-sends the parts of one session's state a client is owed.
    fn resync_session(
        &mut self,
        client: ClientId,
        session_id: &SessionId,
        debt: SessionDebt,
        now_ms: i64,
    ) {
        let Some(session) = self.sessions.get(session_id) else {
            // Gone entirely. There is nothing truthful to say about it here, and a
            // client that asks after it gets a `not_found` which says exactly that.
            return;
        };
        if session.is_archived() {
            // The push it lost was the row leaving the list, so re-sending the summary
            // would put it back.
            let event = ServerEvent::SessionRemoved {
                session_id: session_id.clone(),
                workspace_id: session.workspace_id.clone(),
            };
            self.push_to(client, event);
            return;
        }

        if debt.summary {
            if let Some(summary) = self.session_summary(session_id, now_ms) {
                self.push_to(
                    client,
                    ServerEvent::SessionStateChanged {
                        session: Box::new(summary),
                    },
                );
            }
        }
        if debt.tree {
            let nodes = self.tree_views(session_id, now_ms);
            self.push_to(
                client,
                ServerEvent::TreeChanged {
                    session_id: session_id.clone(),
                    nodes,
                },
            );
        }
        if debt.layout {
            if let Some(session) = self.sessions.get(session_id) {
                let layout = session.layout.clone();
                self.push_to(
                    client,
                    ServerEvent::LayoutChanged {
                        session_id: session_id.clone(),
                        layout,
                    },
                );
            }
        }
        if debt.sizes {
            self.resync_sizes(client, session_id);
        }
    }

    /// Re-sends the geometry of the ptys a client has attached in one session.
    ///
    /// A renderer that missed a resize draws at a width the process is no longer
    /// drawing for, which looks like a corrupted terminal rather than a lost frame.
    fn resync_sizes(&mut self, client: ClientId, session_id: &SessionId) {
        let Some(entry) = self.clients.get(&client) else {
            return;
        };
        let nodes: Vec<NodeId> = entry
            .attachments
            .iter()
            .filter(|((session, _), _)| session == session_id)
            .filter_map(|(_, attachment)| attachment.node_id.clone())
            .collect();
        for node in nodes {
            let Some(process) = self.processes.get(&node) else {
                continue;
            };
            let size = turn_proto::PtySize::new(process.size.rows, process.size.cols);
            self.push_to(
                client,
                ServerEvent::PtyResized {
                    session_id: session_id.clone(),
                    node_id: node,
                    size,
                },
            );
        }
    }

    /// Delivers a coalesced read to everyone watching the node it came from.
    ///
    /// Output for a node nobody is attached to is not pushed at all — it has already
    /// reached that pane's buffer, which is what a later attach reads from. This is the
    /// prioritisation the brief asks for: an unwatched build scrolling past costs the
    /// socket nothing.
    ///
    /// Two representations, one read: whoever asked for cells is sent what changed on
    /// the parsed screen, and whoever asked for bytes is sent the bytes. A pane with no
    /// cells attachment never has a grid built for it, and a pane with no byte
    /// attachment never pays for base64.
    pub(crate) fn deliver_output(&mut self, node: &NodeId, data: Vec<u8>, dropped: u64) {
        self.deliver_screen(node);
        self.deliver_bytes(node, data, dropped);
    }

    /// The byte stream: raw output for the attachments that asked for it.
    fn deliver_bytes(&mut self, node: &NodeId, data: Vec<u8>, dropped: u64) {
        let mut targets: Vec<(ClientId, AttachmentKey)> = Vec::new();
        for (id, client) in self.clients.iter() {
            for (key, attachment) in client.attachments.iter() {
                if attachment.node_id.as_ref() == Some(node) && attachment.stream.is_bytes() {
                    targets.push((*id, key.clone()));
                }
            }
        }
        if targets.is_empty() {
            // Nothing to encode. Base64 over a `cargo build` firehose is not work to do
            // for nobody.
            return;
        }
        let chunks =
            turn_proto::TerminalBytes::new(data).chunks(turn_proto::MAX_OUTPUT_CHUNK_BYTES);

        for (client_id, key) in targets {
            let (session, pane) = key.clone();
            let Some(client) = self.clients.get_mut(&client_id) else {
                continue;
            };
            let Some(attachment) = client.attachments.get_mut(&key) else {
                continue;
            };
            attachment.owed_gap += dropped;

            if attachment.owed_gap > 0 {
                let gap = ServerEvent::PaneOutputGap {
                    session_id: session.clone(),
                    pane_id: pane.clone(),
                    dropped: attachment.owed_gap,
                    resume_seq: attachment.next_seq,
                };
                let owed = attachment.owed_gap;
                if client.push(gap) {
                    if let Some(attachment) = client.attachments.get_mut(&key) {
                        attachment.owed_gap -= owed;
                    }
                } else {
                    // No room even for the admission. Keep owing it; the client will
                    // be told as soon as it drains.
                    continue;
                }
            }

            for chunk in &chunks {
                let Some(attachment) = client.attachments.get_mut(&key) else {
                    break;
                };
                let seq = attachment.next_seq;
                attachment.next_seq += 1;
                let event = ServerEvent::PaneOutput {
                    session_id: session.clone(),
                    pane_id: pane.clone(),
                    node_id: Some(node.clone()),
                    seq,
                    data: chunk.clone(),
                };
                if !client.push(event) {
                    // The frame is gone. Own up to it rather than letting the client
                    // render a terminal that silently missed a screenful.
                    if let Some(attachment) = client.attachments.get_mut(&key) {
                        attachment.owed_gap += 1;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::core::testing::Harness;
    use turn_core::ids::{NodeId, PaneId, SessionId};
    use turn_proto::{PtySize, Request, ServerEvent, ServerMessage};

    const NOW: i64 = 1_775_000_000_000;

    /// Drains whatever a client has already been sent.
    fn drain(
        frames: &mut tokio::sync::mpsc::Receiver<turn_proto::ServerFrame>,
    ) -> Vec<ServerEvent> {
        let mut out = Vec::new();
        while let Ok(frame) = frames.try_recv() {
            if let ServerMessage::Event { event } = frame.message {
                out.push(event);
            }
        }
        out
    }

    /// Two sessions can hold panes with the same id — a duplicated layout, a template
    /// instantiated through a path that reused ids — and attaching to one must not
    /// silently take over the other's stream. The user would see one pane go dead and
    /// the other show somebody else's output.
    #[tokio::test]
    async fn two_sessions_that_share_a_pane_id_never_share_an_attachment() {
        let mut harness = Harness::new().await;
        let pane = PaneId::from_stored("pane_shared");
        let first = SessionId::from_stored("sess_first");
        let second = SessionId::from_stored("sess_second");
        let first_node = NodeId::from_stored("proc_first");
        let second_node = NodeId::from_stored("proc_second");

        for (session, node) in [(&first, &first_node), (&second, &second_node)] {
            harness.add_session(session.clone(), pane.clone(), NOW);
            let layout = &mut harness
                .core
                .sessions
                .get_mut(session)
                .expect("the session")
                .layout;
            layout.get_mut(&pane).expect("the pane").node_id = Some(node.clone());
        }

        let (client, mut frames) = harness.add_client(64);
        for session in [&first, &second] {
            harness
                .core
                .dispatch(
                    client,
                    Request::AttachPane {
                        session_id: session.clone(),
                        pane_id: pane.clone(),
                        size: PtySize::new(24, 80),
                        // Bytes, because this test is about which attachment output
                        // reaches; the panes here have no live pty to build a screen
                        // from.
                        stream: turn_proto::PaneStream::Bytes,
                    },
                    NOW,
                )
                .expect("attaching to a pane with no live pty is allowed");
        }

        let attachments = &harness
            .core
            .clients
            .get(&client)
            .expect("the client")
            .attachments;
        assert_eq!(
            attachments.len(),
            2,
            "the second attach took over the first: {attachments:#?}"
        );
        assert_eq!(
            attachments[&(first.clone(), pane.clone())].node_id.as_ref(),
            Some(&first_node)
        );
        assert_eq!(
            attachments[&(second.clone(), pane.clone())]
                .node_id
                .as_ref(),
            Some(&second_node)
        );

        // And the output goes to the pane it belongs to, labelled with its own session.
        drain(&mut frames);
        harness
            .core
            .deliver_output(&first_node, b"hello".to_vec(), 0);
        let addressed: Vec<SessionId> = drain(&mut frames)
            .into_iter()
            .filter_map(|event| match event {
                ServerEvent::PaneOutput { session_id, .. } => Some(session_id),
                _ => None,
            })
            .collect();
        assert_eq!(addressed, vec![first]);
    }

    /// A client that briefly stalls loses a state frame. Output admits the loss and is
    /// replayed on re-attach; state has no such path, so without a resync the UI renders
    /// a session that is out of date for as long as nothing else happens in it.
    #[tokio::test]
    async fn a_client_that_lost_a_state_push_is_told_the_truth_again() {
        let mut harness = Harness::new().await;
        let session = SessionId::from_stored("sess_behind");
        harness.add_session(session.clone(), PaneId::new(), NOW);
        // One frame of room, so the next push is lost.
        let (client, mut frames) = harness.add_client(1);

        harness.core.push_session_state(&session, NOW);
        harness
            .core
            .sessions
            .get_mut(&session)
            .expect("the session")
            .name = "what is actually true".to_string();
        harness.core.push_session_state(&session, NOW);

        let behind = harness.core.clients.get(&client).expect("the client");
        assert_eq!(behind.dropped_frames, 1);
        assert!(behind.is_behind(), "a lost state push must be owed back");

        // The client starts draining again. Nothing more happens in the session, so the
        // only way it can learn what it missed is the daemon saying it again. One frame
        // of room means the repair takes a few ticks, and it has to finish.
        drain(&mut frames);
        let mut corrected = None;
        for _ in 0..6 {
            harness.core.resync_clients(NOW);
            for event in drain(&mut frames) {
                if let ServerEvent::SessionStateChanged { session: summary } = event {
                    if summary.id == session {
                        corrected = Some(summary.name.clone());
                    }
                }
            }
            if !harness
                .core
                .clients
                .get(&client)
                .expect("the client")
                .is_behind()
            {
                break;
            }
        }
        assert_eq!(
            corrected.as_deref(),
            Some("what is actually true"),
            "the client was never told the state it should be in"
        );
        assert!(
            !harness
                .core
                .clients
                .get(&client)
                .expect("the client")
                .is_behind(),
            "a repaired client stops being owed anything"
        );
    }
}
