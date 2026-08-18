//! The window's side of the daemon connection.
//!
//! egui is immediate-mode and single-threaded; the protocol is asynchronous. So the
//! socket lives on its own thread with its own `tokio` runtime, and the two sides
//! meet at exactly one place: a channel of [`Inbound`] into the UI thread and a
//! channel of requests back out. Nothing is shared but those two channels, which is
//! why no part of the drawing code ever takes a lock.
//!
//! The daemon being absent is a normal state, not an error. Turn's whole premise is
//! that the processes belong to the daemon rather than to the window, so a window
//! whose daemon is momentarily gone has not lost anything — it has lost its *view* of
//! things that are still running. That is why the supervisor never gives up, and why
//! there is exactly one case where it does stop: a refused handshake, which no amount
//! of retrying can talk round.
//!
//! ## Waking the window
//!
//! A repaint costs a frame's worth of CPU, and an idle desk of thirty sessions must
//! cost nothing. So the transport does not ask for repaints on a timer; it calls the
//! [`Waker`] it was given whenever something actually arrived. That is the mechanism
//! behind the performance criterion in [`crate::repaint`].

pub mod backoff;
pub mod link;
pub mod socket;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc as sync_mpsc;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc as tokio_mpsc;
use turn_core::ids::{CheckoutId, HandoffId, NodeId, PaneId, SessionId, TemplateId, WorkspaceId};
use turn_proto::{
    CloseDisposition, HierarchyKey, ProtoError, Request, RequestId, Response, ServerEvent,
};

pub use backoff::{ConnectionState, DaemonIdentity};
pub use link::LinkError;

/// UI intents waiting for the connection thread. A stalled daemon must cost a
/// retryable request failure, never an ever-growing allocation on the render thread.
pub const OUTBOUND_INTENT_CAPACITY: usize = 256;

/// Requests written to a live socket but not yet answered. Once this fills, the
/// connection thread stops draining intents until replies make room, propagating
/// bounded backpressure to [`OUTBOUND_INTENT_CAPACITY`].
pub const PENDING_REQUEST_CAPACITY: usize = 512;

/// Decoded daemon messages waiting for the next frame. At the protocol maximum this
/// is also a hard memory bound; normal screen diffs are much smaller and are drained
/// together once per frame.
pub const INBOUND_MESSAGE_CAPACITY: usize = 64;

/// Something the window can be told to do when a frame arrives.
///
/// A trait object rather than an `egui::Context` so the transport can be tested with
/// no window in sight, and so nothing in here depends on the renderer.
pub type Waker = Arc<dyn Fn() + Send + Sync>;

/// Why the window asked, so an answer can be routed and a failure can be explained.
///
/// Responses are correlated by id, but an id alone does not say what to do with the
/// answer — and an error frame carries no hint at all about which part of the window
/// was waiting. Carrying the intent alongside the id is what lets a failure read
/// "could not split the pane" rather than "error".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ask {
    Hierarchy,
    Inspector(HierarchyKey),
    /// One optimistic tree selection. Keeping the expected key in the intent lets a
    /// rejection or a conflicting daemon answer release exactly that navigation hint
    /// without disturbing a newer click already in flight.
    SelectTree {
        surface_id: String,
        selected: Option<HierarchyKey>,
    },
    Workspaces,
    Sessions,
    Details(SessionId),
    Templates,
    TemplateDetails(TemplateId),
    /// Reading the preferences in force. Carries no id: the answer names the Session it was
    /// resolved for, and a reply that arrived after the selection moved is recognised by that
    /// rather than by remembering what was asked.
    Settings,
    /// Writing one preference at one level, named so a refusal can say which act failed —
    /// "setting the font size at the Workspace level" rather than "a request failed".
    WriteSetting {
        key: String,
    },
    AttentionQueue,
    Preview {
        session_id: SessionId,
        node_id: NodeId,
    },
    PrepareContextHandoff {
        session_id: SessionId,
        source_node_id: NodeId,
        target_node_id: NodeId,
    },
    DeliverContextHandoff {
        session_id: SessionId,
        handoff_id: HandoffId,
    },
    /// Resolve keyboard focus for an exact semantic Attention subject. The
    /// response may name a different Pane-owning runtime node, but may never
    /// change which tree node the demand belongs to.
    AttentionFocus {
        session_id: SessionId,
        subject_node_id: NodeId,
    },
    NodePane {
        session_id: SessionId,
        node_id: NodeId,
        intent_id: u64,
    },
    /// A creation response must close or preserve the correct sheet; a generic label
    /// cannot safely correlate that lifecycle.
    CreateWorkspace {
        continue_to_session: bool,
    },
    CreateSession {
        workspace_id: WorkspaceId,
    },
    CreateTemplate,
    ApplyTemplate(SessionId),
    Attach {
        session_id: SessionId,
        pane_id: PaneId,
        node_id: Option<NodeId>,
        intent_id: u64,
    },
    /// One runtime-scoped geometry request. Unlike keystrokes, delivery and the Ack
    /// matter: the model keeps the latest desired size dirty until this exact tuple is
    /// acknowledged, so queue saturation cannot strand a TUI at an old geometry.
    Resize {
        session_id: SessionId,
        runtime_id: NodeId,
        pane_id: PaneId,
        size: turn_proto::PtySize,
        /// Window-local generation for this runtime geometry write. An external resize can
        /// overtake an older Ack, including an ABA back to the same dimensions; tuple equality
        /// alone cannot distinguish those two intents.
        intent_id: u64,
    },
    /// A whole-screen repair is correlated to the attachment identity the window still
    /// owns. Late answers for another Session/runtime cannot overwrite a rebound feed.
    Resync {
        session_id: SessionId,
        pane_id: PaneId,
        runtime_id: Option<NodeId>,
        attachment_id: u64,
    },
    RelaunchNode {
        session_id: SessionId,
        node_id: NodeId,
    },
    RestoreLeaseAcquire {
        workspace_id: WorkspaceId,
        session_id: SessionId,
        checkout_id: CheckoutId,
    },
    CloseSession {
        session_id: SessionId,
        disposition: CloseDisposition,
    },
    CloseWorkspace {
        workspace_id: WorkspaceId,
        disposition: CloseDisposition,
    },
    /// A Session removed from Turn for good. Its own variant rather than [`Ask::Action`]
    /// because the acknowledgement has work to do — the row has to leave the tree — and
    /// because an error here has to name the right verb.
    DeleteSession {
        session_id: SessionId,
    },
    DeleteWorkspace {
        workspace_id: WorkspaceId,
    },
    /// An archive or a restore. Distinct from [`Ask::Action`] because its
    /// acknowledgement has work to do: whether the row still belongs in the tree
    /// depends on the window's archived preference, and only the daemon can answer
    /// what the tree contains under that preference.
    ArchiveSession {
        archived: bool,
    },
    ArchiveWorkspace {
        archived: bool,
    },
    /// A change the user asked for. The label is what an error message names.
    Action(&'static str),
    /// Activity reporting. Its answer is a list of effects; a failure is not worth
    /// telling the user about, because they did not ask for anything.
    Activity,
    /// Keystrokes and resizes. Too frequent to report individually.
    Stream,
}

impl Ask {
    /// Whether a failure here deserves the user's attention.
    pub fn is_worth_reporting(&self) -> bool {
        !matches!(self, Ask::Activity | Ask::Stream)
    }

    /// What the window was doing, for an error message.
    pub fn describing(&self) -> &str {
        match self {
            Ask::Hierarchy => "loading the workspace hierarchy",
            Ask::Inspector(_) => "loading contextual details",
            Ask::SelectTree { .. } => "selecting a node in the workspace tree",
            Ask::Workspaces => "loading workspaces",
            Ask::Sessions => "loading sessions",
            Ask::Details(_) => "loading a session",
            Ask::Templates => "loading templates",
            Ask::TemplateDetails(_) => "loading the template editor",
            Ask::Settings => "loading preferences",
            Ask::WriteSetting { .. } => "saving the preference",
            Ask::AttentionQueue => "loading the attention queue",
            Ask::Preview { .. } => "loading an activity preview",
            Ask::PrepareContextHandoff { .. } => "preparing an Agent context handoff",
            Ask::DeliverContextHandoff { .. } => "delivering an Agent context handoff",
            Ask::AttentionFocus { .. } => "focusing the pane that can answer attention",
            Ask::NodePane { .. } => "opening a node as a pane",
            Ask::CreateWorkspace { .. } => "creating a workspace",
            Ask::CreateSession { .. } => "creating a session",
            Ask::CreateTemplate => "saving the layout preset",
            Ask::ApplyTemplate(_) => "applying the layout preset",
            Ask::Attach { .. } => "attaching to a pane",
            Ask::Resize { .. } => "resizing a terminal pane",
            Ask::Resync { .. } => "resynchronising a terminal pane",
            Ask::RelaunchNode { .. } => "starting the restored pane",
            Ask::RestoreLeaseAcquire { .. } => "acquiring exclusive write access",
            Ask::CloseSession { .. } => "ending the session",
            Ask::CloseWorkspace { .. } => "stopping every session in the workspace",
            Ask::DeleteSession { .. } => "deleting the session",
            Ask::DeleteWorkspace { .. } => "deleting the workspace",
            Ask::ArchiveSession { archived: true } => "archiving the session",
            Ask::ArchiveSession { archived: false } => "restoring the session",
            Ask::ArchiveWorkspace { archived: true } => "archiving the workspace",
            Ask::ArchiveWorkspace { archived: false } => "restoring the workspace",
            Ask::Action(label) => label,
            Ask::Activity => "reporting activity",
            Ask::Stream => "sending input to the terminal",
        }
    }
}

/// What reaches the UI thread.
#[derive(Debug)]
pub enum Inbound {
    /// The connection changed state.
    Status(ConnectionState),
    /// An unsolicited push.
    Event(Box<ServerEvent>),
    /// An answer, with the intent that produced it.
    Answer { ask: Ask, response: Box<Response> },
    /// A request failed. The intent says which part of the window to tell.
    Failed { ask: Ask, error: ProtoError },
    /// A failure belonging to no request, or a frame we could not decode. Shown in
    /// the status line and logged; never fatal.
    Notice(ProtoError),
}

/// The synchronous result of crossing the GUI-to-transport boundary. A rejected
/// request is returned to the caller rather than squeezed through the already-full
/// daemon-to-GUI queue, so Desk can always release its matching pending intent.
#[derive(Debug)]
#[must_use = "a rejected request must be returned to Desk so its pending intent can settle"]
pub enum SendOutcome {
    Queued,
    Rejected(Inbound),
}

/// One request, on its way out.
struct Outbound {
    /// The live socket generation this intent was created against. A request from an
    /// older generation may never cross a later daemon connection.
    generation: u64,
    ask: Ask,
    request: Request,
}

/// The window's handle on the daemon.
pub struct DaemonLink {
    socket: PathBuf,
    outbound: tokio_mpsc::Sender<Outbound>,
    inbound: sync_mpsc::Receiver<Inbound>,
    /// Zero while disconnected; otherwise the generation accepted by `serve`.
    connection_generation: Arc<AtomicU64>,
    /// Kept so the runtime thread is not detached; dropping the link ends it.
    _thread: std::thread::JoinHandle<()>,
}

#[cfg(test)]
pub(crate) struct SaturatedTestPeer {
    outbound: tokio_mpsc::Receiver<Outbound>,
    inbound: sync_mpsc::SyncSender<Inbound>,
}

#[cfg(test)]
impl SaturatedTestPeer {
    pub(crate) fn assert_inbound_full(&self) {
        assert!(matches!(
            self.inbound.try_send(Inbound::Notice(ProtoError::new(
                turn_proto::ErrorCode::Unavailable,
                "over capacity",
            ))),
            Err(sync_mpsc::TrySendError::Full(_))
        ));
    }

    pub(crate) fn release_one_outbound(&mut self) {
        self.outbound
            .try_recv()
            .expect("the saturated test queue has one filler to release");
    }

    pub(crate) fn drain_sent(&mut self) -> Vec<(Ask, Request)> {
        let mut sent = Vec::new();
        while let Ok(outbound) = self.outbound.try_recv() {
            sent.push((outbound.ask, outbound.request));
        }
        sent
    }
}

impl DaemonLink {
    #[cfg(test)]
    pub(crate) fn saturated_for_test() -> (Self, SaturatedTestPeer) {
        let (outbound, outbound_receiver) =
            tokio_mpsc::channel::<Outbound>(OUTBOUND_INTENT_CAPACITY);
        for _ in 0..OUTBOUND_INTENT_CAPACITY {
            outbound
                .try_send(Outbound {
                    generation: 1,
                    ask: Ask::Activity,
                    request: Request::ListWorkspaces {
                        include_archived: false,
                    },
                })
                .expect("the test fills the bounded outbound queue exactly");
        }
        let (inbound_sender, inbound) =
            sync_mpsc::sync_channel::<Inbound>(INBOUND_MESSAGE_CAPACITY);
        for index in 0..INBOUND_MESSAGE_CAPACITY {
            inbound_sender
                .try_send(Inbound::Notice(ProtoError::new(
                    turn_proto::ErrorCode::Unavailable,
                    format!("synthetic inbound {index}"),
                )))
                .expect("the test fills the bounded inbound queue exactly");
        }
        let link = DaemonLink {
            socket: PathBuf::from("/tmp/turn-saturated-test.sock"),
            outbound,
            inbound,
            connection_generation: Arc::new(AtomicU64::new(1)),
            _thread: std::thread::spawn(|| {}),
        };
        (
            link,
            SaturatedTestPeer {
                outbound: outbound_receiver,
                inbound: inbound_sender,
            },
        )
    }

    /// Starts the connection on its own thread and returns at once.
    ///
    /// Deliberately does not wait for a connection: the window has to be able to draw
    /// "no daemon" before there is one, because that is the state a user sees when
    /// they open Turn before `turnd` has finished binding.
    pub fn spawn(socket: PathBuf, client_version: impl Into<String>, wake: Waker) -> DaemonLink {
        let client_version = client_version.into();
        let (outbound, outbound_rx) = tokio_mpsc::channel::<Outbound>(OUTBOUND_INTENT_CAPACITY);
        let (inbound_tx, inbound) = sync_mpsc::sync_channel::<Inbound>(INBOUND_MESSAGE_CAPACITY);
        let path = socket.clone();
        let connection_generation = Arc::new(AtomicU64::new(0));
        let supervisor_generation = Arc::clone(&connection_generation);

        let thread = std::thread::Builder::new()
            .name("turn-daemon-link".to_string())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        // Reported rather than panicked: a window that says why it
                        // cannot connect is better than one that vanishes.
                        let _ =
                            inbound_tx.try_send(Inbound::Status(ConnectionState::Disconnected {
                                message: format!("could not start the connection thread: {error}"),
                                retrying: false,
                            }));
                        wake();
                        return;
                    }
                };
                runtime.block_on(supervise(
                    path,
                    client_version,
                    outbound_rx,
                    inbound_tx,
                    wake,
                    supervisor_generation,
                ));
            });

        let thread = match thread {
            Ok(handle) => handle,
            Err(error) => {
                tracing::error!(%error, "could not start the daemon connection thread");
                // A thread that never started still needs a handle-shaped value. One
                // that does nothing is honest: `drain` will report the failure that
                // was already queued, and every `send` will be dropped.
                std::thread::spawn(|| {})
            }
        };

        DaemonLink {
            socket,
            outbound,
            inbound,
            connection_generation,
            _thread: thread,
        }
    }

    pub fn socket(&self) -> &std::path::Path {
        &self.socket
    }

    /// Queues a request for the connection that is live *now*. Never blocks and never
    /// waits for a future daemon.
    ///
    /// A window that queued requests across a reconnect would replay a `close_pane`
    /// against a layout the daemon rebuilt from disk. The window re-fetches on
    /// reconnect instead, which is the only correct recovery, so dropping a request
    /// sent while disconnected is what makes that happen.
    pub fn send(&self, ask: Ask, request: Request) -> SendOutcome {
        let generation = self.connection_generation.load(Ordering::Acquire);
        if generation == 0 {
            return self.reject_offline(ask);
        }
        match self.outbound.try_send(Outbound {
            generation,
            ask,
            request,
        }) {
            Ok(()) => SendOutcome::Queued,
            Err(tokio_mpsc::error::TrySendError::Full(outbound)) => {
                self.reject_saturated(outbound.ask)
            }
            Err(tokio_mpsc::error::TrySendError::Closed(outbound)) => {
                self.reject_offline(outbound.ask)
            }
        }
    }

    fn reject_offline(&self, ask: Ask) -> SendOutcome {
        tracing::debug!(
            intent = ask.describing(),
            "a request was dropped without a live daemon connection"
        );
        if !ask.is_worth_reporting() {
            return SendOutcome::Queued;
        }
        let error = disconnected_request_error(&ask);
        SendOutcome::Rejected(Inbound::Failed { ask, error })
    }

    fn reject_saturated(&self, ask: Ask) -> SendOutcome {
        tracing::warn!(
            intent = ask.describing(),
            capacity = OUTBOUND_INTENT_CAPACITY,
            "the GUI-to-daemon queue reached its fixed capacity"
        );
        if !ask.is_worth_reporting() {
            return SendOutcome::Queued;
        }
        let error = ProtoError::new(
            turn_proto::ErrorCode::RateLimited,
            format!(
                "Turn is still responsive, but its daemon connection is not accepting requests fast enough to {}. Try again",
                ask.describing()
            ),
        );
        SendOutcome::Rejected(Inbound::Failed { ask, error })
    }

    /// Everything that arrived since the last call.
    ///
    /// Drained once per frame rather than handled as it arrives, because applying
    /// forty pane updates and then drawing once is the whole difference between a
    /// window that keeps up with a build and one that does not.
    pub fn drain(&self) -> Vec<Inbound> {
        self.inbound.try_iter().collect()
    }
}

/// Connects, serves the connection until it ends, and does it again.
///
/// Returns only when the daemon refuses this build for a non-authentication reason,
/// because that is the failure a retry cannot fix. An auth refusal re-reads the
/// generation token and follows normal backoff.
async fn supervise(
    socket: PathBuf,
    client_version: String,
    mut outbound: tokio_mpsc::Receiver<Outbound>,
    inbound: sync_mpsc::SyncSender<Inbound>,
    wake: Waker,
    connection_generation: Arc<AtomicU64>,
) {
    let mut identity = DaemonIdentity::new();
    let mut attempt: u32 = 0;
    let mut consecutive_auth_refusals: u32 = 0;
    let mut last_status = ConnectionState::Starting;
    let mut generation: u64 = 0;

    loop {
        attempt = attempt.saturating_add(1);
        let delay = backoff::retry_delay_ms(attempt);
        if delay > 0 {
            tokio::time::sleep(Duration::from_millis(delay)).await;
        }
        if !publish(
            &inbound,
            &wake,
            &mut last_status,
            ConnectionState::Connecting { attempt },
        ) {
            return;
        }

        let (mut connection, welcome) = match link::connect(&socket, &client_version).await {
            Ok(parts) => parts,
            Err(LinkError::Refused(error)) if error.code != turn_proto::ErrorCode::Unauthorized => {
                tracing::error!(message = %error.message, "the daemon refused this build");
                publish(
                    &inbound,
                    &wake,
                    &mut last_status,
                    backoff::incompatible(&error),
                );
                return;
            }
            Err(error) => {
                tracing::debug!(%error, "could not reach the daemon; will retry");
                let authentication_refused = matches!(
                    &error,
                    LinkError::Refused(error)
                        if error.code == turn_proto::ErrorCode::Unauthorized
                );
                if authentication_refused {
                    consecutive_auth_refusals = consecutive_auth_refusals.saturating_add(1);
                } else {
                    consecutive_auth_refusals = 0;
                }
                // `retrying` is the error's own verdict rather than a constant: a
                // socket that is not there yet is worth waiting for, and a frame this
                // build could not encode is not.
                let retrying = error.is_retryable();
                let protocol_error = error.to_proto_error();
                let message = if consecutive_auth_refusals >= 3 {
                    format!(
                        "Turn repeatedly reached the daemon but could not authenticate. \
                         Its capability file may be stale or corrupt; retrying. Last response: {}",
                        protocol_error.message
                    )
                } else {
                    protocol_error.message
                };
                let published = publish(
                    &inbound,
                    &wake,
                    &mut last_status,
                    ConnectionState::Disconnected { message, retrying },
                );
                if !published || !retrying {
                    return;
                }
                continue;
            }
        };

        consecutive_auth_refusals = 0;

        tracing::info!(
            daemon_pid = welcome.daemon_pid,
            daemon_version = %welcome.daemon_version,
            protocol = connection.agreed_version(),
            "connected to turnd"
        );
        generation = generation.saturating_add(1).max(1);
        connection_generation.store(generation, Ordering::Release);
        if !publish(
            &inbound,
            &wake,
            &mut last_status,
            identity.observe(&welcome),
        ) {
            connection_generation.store(0, Ordering::Release);
            return;
        }

        let opened = tokio::time::Instant::now();
        let ended_cleanly =
            serve(&mut connection, &mut outbound, &inbound, &wake, generation).await;
        // Close the gate before publishing Disconnected. An intent racing the socket
        // teardown either fails on that socket or carries this old generation and is
        // rejected by the next one; it can never be replayed there.
        connection_generation.store(0, Ordering::Release);

        // The backoff is only forgiven by a connection that lasted. A daemon that
        // handshakes and dies is a crash loop, and starting the next attempt from
        // zero would reconnect to it as fast as the kernel allows — a spinning core
        // and a status line rewriting itself hundreds of times a second.
        let lived_ms = opened.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        attempt = backoff::attempt_after_connection(attempt, lived_ms);

        if !ended_cleanly {
            // The UI has gone. Nothing left to serve.
            return;
        }
        if !publish(
            &inbound,
            &wake,
            &mut last_status,
            ConnectionState::Disconnected {
                message: "The Turn daemon connection ended. Your processes keep running; \
                          reconnecting"
                    .to_string(),
                retrying: true,
            },
        ) {
            return;
        }
    }
}

/// Pumps one live connection until it ends.
///
/// Returns false when the *window* has gone, which is the one reason not to reconnect.
async fn serve(
    connection: &mut link::Connection,
    outbound: &mut tokio_mpsc::Receiver<Outbound>,
    inbound: &sync_mpsc::SyncSender<Inbound>,
    wake: &Waker,
    generation: u64,
) -> bool {
    let mut pending: std::collections::HashMap<RequestId, Ask> = std::collections::HashMap::new();
    let mut next_id: u64 = 1;

    loop {
        tokio::select! {
            frame = connection.next_frame() => {
                let Some(frame) = frame else { break };
                let message = match frame {
                    link::Frame::Response { id, response } => {
                        match pending.remove(&id) {
                            Some(ask) => Inbound::Answer { ask, response },
                            None => {
                                tracing::debug!(%id, "an answer arrived for a request nobody is waiting on");
                                continue;
                            }
                        }
                    }
                    link::Frame::Error { id, error } => match id.and_then(|id| pending.remove(&id)) {
                        Some(ask) => Inbound::Failed { ask, error },
                        None => Inbound::Notice(error),
                    },
                    link::Frame::Event(event) => Inbound::Event(event),
                    link::Frame::Undecodable(error) => Inbound::Notice(error),
                };
                match deliver_inbound(inbound, wake, message) {
                    InboundDelivery::Delivered => {}
                    InboundDelivery::Full => {
                        tracing::warn!(
                            capacity = INBOUND_MESSAGE_CAPACITY,
                            "the daemon-to-GUI queue reached capacity; reconnecting for a fresh projection"
                        );
                        break;
                    }
                    InboundDelivery::Closed => return false,
                }
            }
            request = outbound.recv(), if pending.len() < PENDING_REQUEST_CAPACITY => {
                let Some(Outbound { generation: request_generation, ask, request }) = request else {
                    // The window dropped its handle.
                    return false;
                };
                if request_generation != generation {
                    if ask.is_worth_reporting() {
                        let error = disconnected_request_error(&ask);
                        match deliver_inbound(inbound, wake, Inbound::Failed { ask, error }) {
                            InboundDelivery::Delivered => {}
                            InboundDelivery::Full => break,
                            InboundDelivery::Closed => return false,
                        }
                    }
                    continue;
                }
                let id = RequestId::new(format!("r-{next_id}"));
                next_id = next_id.saturating_add(1);
                match connection.send(id.clone(), request).await {
                    Ok(()) => {
                        pending.insert(id, ask);
                    }
                    Err(error) => {
                        let failure = Inbound::Failed { ask, error: error.to_proto_error() };
                        match deliver_inbound(inbound, wake, failure) {
                            InboundDelivery::Delivered => {}
                            InboundDelivery::Full => break,
                            InboundDelivery::Closed => return false,
                        }
                        if !error.is_retryable() {
                            continue;
                        }
                        break;
                    }
                }
            }
        }
    }

    // Nothing may be left pending. A request that never settles presents to the user
    // as a frozen window, and the honest answer is that the connection went away.
    for (_, ask) in pending.drain() {
        if !ask.is_worth_reporting() {
            continue;
        }
        let error = ProtoError::new(
            turn_proto::ErrorCode::Unavailable,
            format!(
                "The daemon connection ended while {}. Your processes keep running",
                ask.describing()
            ),
        );
        match deliver_inbound(inbound, wake, Inbound::Failed { ask, error }) {
            InboundDelivery::Delivered => {}
            InboundDelivery::Full => break,
            InboundDelivery::Closed => return false,
        }
    }
    true
}

fn disconnected_request_error(ask: &Ask) -> ProtoError {
    ProtoError::new(
        turn_proto::ErrorCode::Unavailable,
        format!(
            "Turn is not connected to its daemon, so it did not send the request for {}. Try again after it reconnects",
            ask.describing()
        ),
    )
}

/// Publishes a state change, and only a change.
///
/// Re-announcing "connecting, attempt 4" every four seconds would make the status
/// line flicker for no new information. Returns false when the window has gone.
fn publish(
    inbound: &sync_mpsc::SyncSender<Inbound>,
    wake: &Waker,
    last: &mut ConnectionState,
    state: ConnectionState,
) -> bool {
    if *last == state {
        return true;
    }
    match deliver_inbound(inbound, wake, Inbound::Status(state.clone())) {
        InboundDelivery::Delivered => {
            *last = state;
            true
        }
        InboundDelivery::Full => {
            tracing::warn!(
                capacity = INBOUND_MESSAGE_CAPACITY,
                "the GUI status queue is full; retaining the previous published state"
            );
            true
        }
        InboundDelivery::Closed => false,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InboundDelivery {
    Delivered,
    Full,
    Closed,
}

fn deliver_inbound(
    inbound: &sync_mpsc::SyncSender<Inbound>,
    wake: &Waker,
    message: Inbound,
) -> InboundDelivery {
    match inbound.try_send(message) {
        Ok(()) => {
            wake();
            InboundDelivery::Delivered
        }
        Err(sync_mpsc::TrySendError::Full(_)) => InboundDelivery::Full,
        Err(sync_mpsc::TrySendError::Disconnected(_)) => InboundDelivery::Closed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    use tokio::net::UnixListener;
    use turn_proto::{ClientMessage, ServerFrame, Welcome};

    fn remove_socket_files(path: &Path) {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(turn_proto::ipc_auth_token_path(path));
    }

    fn socket_path(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "turn-gui-supervise-{}-{}.sock",
            name,
            std::process::id()
        ));
        remove_socket_files(&path);
        std::fs::write(turn_proto::ipc_auth_token_path(&path), "b".repeat(64)).unwrap();
        path
    }

    fn counting_waker() -> (Waker, Arc<AtomicUsize>) {
        let count = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&count);
        (
            Arc::new(move || {
                counter.fetch_add(1, Ordering::SeqCst);
            }),
            count,
        )
    }

    #[test]
    fn both_gui_boundary_queues_have_hard_capacity() {
        let (inbound, inbound_receiver) =
            sync_mpsc::sync_channel::<Inbound>(INBOUND_MESSAGE_CAPACITY);
        for index in 0..INBOUND_MESSAGE_CAPACITY {
            inbound
                .try_send(Inbound::Notice(ProtoError::new(
                    turn_proto::ErrorCode::Unavailable,
                    format!("synthetic inbound {index}"),
                )))
                .unwrap();
        }
        assert!(matches!(
            inbound.try_send(Inbound::Notice(ProtoError::new(
                turn_proto::ErrorCode::Unavailable,
                "over capacity"
            ))),
            Err(sync_mpsc::TrySendError::Full(_))
        ));

        let (outbound, _outbound_receiver) =
            tokio_mpsc::channel::<Outbound>(OUTBOUND_INTENT_CAPACITY);
        for _ in 0..OUTBOUND_INTENT_CAPACITY {
            outbound
                .try_send(Outbound {
                    generation: 1,
                    ask: Ask::Activity,
                    request: Request::ListWorkspaces {
                        include_archived: false,
                    },
                })
                .unwrap();
        }
        assert!(matches!(
            outbound.try_send(Outbound {
                generation: 1,
                ask: Ask::Activity,
                request: Request::ListWorkspaces {
                    include_archived: false,
                },
            }),
            Err(tokio_mpsc::error::TrySendError::Full(_))
        ));

        let session_id = SessionId::from_stored("sess_saturated_resize");
        let runtime_id = NodeId::from_stored("proc_saturated_resize");
        let pane_id = PaneId::from_stored("pane_saturated_resize");
        let size = turn_proto::PtySize::new(60, 200);
        let link = DaemonLink {
            socket: PathBuf::from("/tmp/turn-saturated-test.sock"),
            outbound,
            inbound: inbound_receiver,
            connection_generation: Arc::new(AtomicU64::new(1)),
            _thread: std::thread::spawn(|| {}),
        };
        let outcome = link.send(
            Ask::Resize {
                session_id: session_id.clone(),
                runtime_id: runtime_id.clone(),
                pane_id: pane_id.clone(),
                size,
                intent_id: 1,
            },
            Request::ResizePty {
                session_id,
                node_id: runtime_id,
                size,
            },
        );
        assert!(matches!(
            outcome,
            SendOutcome::Rejected(Inbound::Failed {
                ask: Ask::Resize { pane_id: rejected, .. },
                error,
            }) if rejected == pane_id && error.code == turn_proto::ErrorCode::RateLimited
        ));
    }

    /// Collects statuses until one matches, or gives up.
    fn wait_for<F>(link: &DaemonLink, mut matches: F) -> Vec<Inbound>
    where
        F: FnMut(&Inbound) -> bool,
    {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut seen = Vec::new();
        while std::time::Instant::now() < deadline {
            for message in link.drain() {
                let done = matches(&message);
                seen.push(message);
                if done {
                    return seen;
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        seen
    }

    #[test]
    fn a_window_with_no_daemon_is_told_so_rather_than_left_blank() {
        let (wake, wakes) = counting_waker();
        let path = socket_path("absent");
        let link = DaemonLink::spawn(path.clone(), "0.1.0", wake);
        let seen = wait_for(&link, |message| {
            matches!(
                message,
                Inbound::Status(ConnectionState::Disconnected { retrying: true, .. })
            )
        });
        assert!(
            seen.iter().any(|m| matches!(
                m,
                Inbound::Status(ConnectionState::Disconnected { retrying: true, .. })
            )),
            "the user must be told there is no daemon; saw {seen:?}"
        );
        assert!(
            wakes.load(Ordering::SeqCst) > 0,
            "a status change has to wake the window or nobody sees it"
        );
        drop(link);
        remove_socket_files(&path);
    }

    /// A daemon that appears late is the everyday case: the user opened Turn and the
    /// daemon is still binding its socket.
    #[test]
    fn a_daemon_that_appears_late_is_connected_to_and_the_handshake_is_reported() {
        let path = socket_path("late");
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("a runtime for the fake daemon");
        let listener_path = path.clone();
        let daemon = runtime.spawn(async move {
            tokio::time::sleep(Duration::from_millis(120)).await;
            let Ok(listener) = UnixListener::bind(&listener_path) else {
                return;
            };
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let (read, mut write) = stream.into_split();
            let mut lines = tokio::io::BufReader::new(read).lines();
            let _ = lines.next_line().await;
            let welcome = Welcome::new(1, "0.1.0-test", 4242, 1_700_000_000_000);
            if let Ok(bytes) = turn_proto::encode(&ServerFrame::welcome(welcome)) {
                let _ = write.write_all(&bytes).await;
            }
            tokio::time::sleep(Duration::from_secs(10)).await;
        });

        let (wake, _) = counting_waker();
        let link = DaemonLink::spawn(path.clone(), "0.1.0", wake);
        let seen = wait_for(&link, |message| {
            matches!(message, Inbound::Status(ConnectionState::Connected { .. }))
        });
        let connected = seen.iter().find_map(|message| match message {
            Inbound::Status(state @ ConnectionState::Connected { .. }) => Some(state.clone()),
            _ => None,
        });
        match connected {
            Some(ConnectionState::Connected {
                daemon_pid,
                first_connection,
                daemon_restarted,
                ..
            }) => {
                assert_eq!(daemon_pid, 4242);
                assert!(first_connection);
                assert!(!daemon_restarted);
            }
            other => panic!("expected to connect; got {other:?} out of {seen:?}"),
        }

        daemon.abort();
        drop(link);
        remove_socket_files(&path);
    }

    /// A crash-looping daemon must not be reconnected to as fast as the kernel
    /// allows. Real time rather than a paused clock, deliberately: the failure this
    /// guards against is a loop with nothing in it to await, which a paused clock
    /// would never advance past.
    #[test]
    fn a_daemon_that_hangs_up_after_the_handshake_is_not_reconnected_to_in_a_tight_loop() {
        let path = socket_path("crash-loop");
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("a runtime for the fake daemon");
        let handshakes = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&handshakes);
        let listener_path = path.clone();
        let daemon = runtime.spawn(async move {
            let Ok(listener) = UnixListener::bind(&listener_path) else {
                return;
            };
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                counter.fetch_add(1, Ordering::SeqCst);
                let (read, mut write) = stream.into_split();
                let mut lines = tokio::io::BufReader::new(read).lines();
                if lines.next_line().await.is_err() {
                    continue;
                }
                let welcome = Welcome::new(1, "0.1.0-test", 4242, 1_700_000_000_000);
                if let Ok(bytes) = turn_proto::encode(&ServerFrame::welcome(welcome)) {
                    let _ = write.write_all(&bytes).await;
                }
                // Dropping both halves closes the socket, which is what the window
                // sees when the daemon on the other end dies.
            }
        });

        let (wake, _) = counting_waker();
        let link = DaemonLink::spawn(path.clone(), "0.1.0", wake);
        std::thread::sleep(Duration::from_millis(700));
        let statuses: Vec<u32> = link
            .drain()
            .into_iter()
            .filter_map(|message| match message {
                Inbound::Status(ConnectionState::Connecting { attempt }) => Some(attempt),
                _ => None,
            })
            .collect();
        drop(link);
        daemon.abort();
        remove_socket_files(&path);

        let count = handshakes.load(Ordering::SeqCst);
        assert!(count >= 1, "it must have connected at all; saw {count}");
        // With the backoff intact the delays are 0ms, 250ms and 500ms, so seven
        // hundred milliseconds buys two or three attempts. Resetting the counter on a
        // handshake instead produces one per socket round trip — hundreds.
        assert!(
            count <= 10,
            "a handshake immediately followed by a hang-up must not buy back the backoff; \
             saw {count} reconnections in 700ms"
        );
        assert!(
            statuses.iter().any(|attempt| *attempt >= 2),
            "the attempt counter must keep climbing across a crash loop; saw {statuses:?}"
        );
    }

    /// The refusal is the one failure the supervisor stops on, because retrying it
    /// would hide the sentence the user has to read.
    #[test]
    fn a_refused_build_stops_retrying_and_says_why() {
        let path = socket_path("refused");
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("a runtime for the fake daemon");
        let listener_path = path.clone();
        let daemon = runtime.spawn(async move {
            let Ok(listener) = UnixListener::bind(&listener_path) else {
                return;
            };
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let (read, mut write) = stream.into_split();
                let mut lines = tokio::io::BufReader::new(read).lines();
                let _ = lines.next_line().await;
                let error = ProtoError::new(
                    turn_proto::ErrorCode::UnsupportedVersion,
                    "This Turn app is too old for the daemon it is talking to",
                );
                if let Ok(bytes) = turn_proto::encode(&ServerFrame::rejected(error)) {
                    let _ = write.write_all(&bytes).await;
                }
            }
        });

        let (wake, _) = counting_waker();
        let link = DaemonLink::spawn(path.clone(), "0.1.0", wake);
        let seen = wait_for(&link, |message| {
            matches!(
                message,
                Inbound::Status(ConnectionState::Incompatible { .. })
            )
        });
        assert!(
            seen.iter().any(|message| matches!(
                message,
                Inbound::Status(ConnectionState::Incompatible { .. })
            )),
            "a refusal must reach the window as a refusal; saw {seen:?}"
        );
        // And it stops: no further attempts arrive.
        std::thread::sleep(Duration::from_millis(300));
        let after: Vec<Inbound> = link.drain();
        assert!(
            !after.iter().any(|message| matches!(
                message,
                Inbound::Status(ConnectionState::Connecting { .. })
            )),
            "the supervisor must stop rather than loop on a refusal; saw {after:?}"
        );

        drop(link);
        daemon.abort();
        remove_socket_files(&path);
    }

    #[test]
    fn repeated_authentication_refusals_name_the_capability_problem() {
        let path = socket_path("auth-refused");
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("a runtime for the fake daemon");
        let listener_path = path.clone();
        let daemon = runtime.spawn(async move {
            let Ok(listener) = UnixListener::bind(&listener_path) else {
                return;
            };
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let (read, mut write) = stream.into_split();
                let mut lines = tokio::io::BufReader::new(read).lines();
                let _ = lines.next_line().await;
                let error = ProtoError::new(
                    turn_proto::ErrorCode::Unauthorized,
                    "This client did not present the current daemon capability",
                );
                if let Ok(bytes) = turn_proto::encode(&ServerFrame::rejected(error)) {
                    let _ = write.write_all(&bytes).await;
                }
            }
        });

        let (wake, _) = counting_waker();
        let link = DaemonLink::spawn(path.clone(), "0.1.0", wake);
        let seen = wait_for(&link, |message| {
            matches!(
                message,
                Inbound::Status(ConnectionState::Disconnected { message, retrying: true })
                    if message.contains("repeatedly reached the daemon")
                        && message.contains("capability file")
            )
        });
        assert!(
            seen.iter().any(|message| matches!(
                message,
                Inbound::Status(ConnectionState::Disconnected { message, retrying: true })
                    if message.contains("repeatedly reached the daemon")
                        && message.contains("capability file")
            )),
            "persistent authentication failures need a specific diagnosis; saw {seen:?}"
        );

        drop(link);
        daemon.abort();
        remove_socket_files(&path);
    }

    /// A request the window sends must come back tagged with why it was sent, or a
    /// failure is unattributable and the user is shown "error".
    #[test]
    fn an_answer_arrives_carrying_the_intent_that_produced_it() {
        let path = socket_path("intent");
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("a runtime for the fake daemon");
        let listener_path = path.clone();
        let daemon = runtime.spawn(async move {
            let Ok(listener) = UnixListener::bind(&listener_path) else {
                return;
            };
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let (read, mut write) = stream.into_split();
            let mut lines = tokio::io::BufReader::new(read).lines();
            let _ = lines.next_line().await;
            let welcome = Welcome::new(1, "0.1.0-test", 77, 1_700_000_000_000);
            if let Ok(bytes) = turn_proto::encode(&ServerFrame::welcome(welcome)) {
                let _ = write.write_all(&bytes).await;
            }
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(frame) = serde_json::from_str::<turn_proto::ClientFrame>(&line) else {
                    continue;
                };
                let Some(id) = frame.request_id().cloned() else {
                    continue;
                };
                let reply = ServerFrame::error(
                    Some(id),
                    ProtoError::new(
                        turn_proto::ErrorCode::Conflict,
                        "the last pane cannot close",
                    ),
                );
                if let Ok(bytes) = turn_proto::encode(&reply) {
                    let _ = write.write_all(&bytes).await;
                }
            }
        });

        let (wake, _) = counting_waker();
        let link = DaemonLink::spawn(path.clone(), "0.1.0", wake);
        wait_for(&link, |message| {
            matches!(message, Inbound::Status(ConnectionState::Connected { .. }))
        });
        assert!(matches!(
            link.send(Ask::Action("closing the pane"), Request::ListTemplates),
            SendOutcome::Queued
        ));

        let seen = wait_for(&link, |message| matches!(message, Inbound::Failed { .. }));
        let failure = seen.iter().find_map(|message| match message {
            Inbound::Failed { ask, error } => Some((ask.clone(), error.clone())),
            _ => None,
        });
        match failure {
            Some((ask, error)) => {
                assert_eq!(ask, Ask::Action("closing the pane"));
                assert_eq!(ask.describing(), "closing the pane");
                assert_eq!(error.code, turn_proto::ErrorCode::Conflict);
            }
            None => panic!("the failure must be attributable; saw {seen:?}"),
        }

        drop(link);
        daemon.abort();
        remove_socket_files(&path);
    }

    #[test]
    fn a_request_sent_with_no_connection_is_dropped_rather_than_queued_for_later() {
        // Replaying a queued mutation after a reconnect would act on a world the
        // window has not fetched. Start with no peer and wait until that fact is
        // observable, so this is a real offline send rather than a connection race.
        let path = socket_path("dropped");
        let (wake, _) = counting_waker();
        let link = DaemonLink::spawn(path.clone(), "0.1.0", wake);
        wait_for(&link, |message| {
            matches!(
                message,
                Inbound::Status(ConnectionState::Disconnected { .. })
            )
        });
        let rejected = link.send(
            Ask::Action("creating a workspace"),
            Request::CreateWorkspace {
                name: "must-not-appear".into(),
                root: "/tmp/must-not-appear".into(),
            },
        );
        assert!(
            matches!(
                rejected,
                SendOutcome::Rejected(Inbound::Failed {
                    ask: Ask::Action("creating a workspace"),
                    ref error
                }) if error.code == turn_proto::ErrorCode::Unavailable
            ),
            "an offline user action must be rejected immediately; saw {rejected:?}"
        );

        // Now make a real peer appear. It holds the connection open, first watching
        // long enough for the stale mutation to arrive, then watching for a new read
        // sent against this live generation. The old implementation fails here: its
        // unbounded channel delivers CreateWorkspace immediately after the Welcome.
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("a runtime for the late fake daemon");
        let listener_path = path.clone();
        let (observed_tx, observed_rx) = sync_mpsc::channel::<Option<Request>>();
        let daemon = runtime.spawn(async move {
            let Ok(listener) = UnixListener::bind(&listener_path) else {
                return;
            };
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let (read, mut write) = stream.into_split();
            let mut lines = tokio::io::BufReader::new(read).lines();
            let _ = lines.next_line().await;
            let welcome = Welcome::new(1, "0.1.0-test", 4242, 1_700_000_000_000);
            if let Ok(bytes) = turn_proto::encode(&ServerFrame::welcome(welcome)) {
                let _ = write.write_all(&bytes).await;
            }

            for timeout in [Duration::from_millis(500), Duration::from_secs(2)] {
                let observed = match tokio::time::timeout(timeout, lines.next_line()).await {
                    Ok(Ok(Some(line))) => serde_json::from_str::<turn_proto::ClientFrame>(&line)
                        .ok()
                        .and_then(|frame| match frame.message {
                            ClientMessage::Request { request, .. } => Some(request),
                            ClientMessage::Hello(_) => None,
                        }),
                    _ => None,
                };
                let _ = observed_tx.send(observed);
            }
        });

        let connected = wait_for(&link, |message| {
            matches!(message, Inbound::Status(ConnectionState::Connected { .. }))
        });
        assert!(
            connected.iter().any(|message| matches!(
                message,
                Inbound::Status(ConnectionState::Connected { .. })
            )),
            "the late peer must actually connect; saw {connected:?}"
        );
        let stale = observed_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("the peer reports what arrived before a fresh send");
        assert_eq!(
            stale, None,
            "an intent created offline crossed a later connection: {stale:?}"
        );

        assert!(matches!(
            link.send(Ask::Templates, Request::ListTemplates),
            SendOutcome::Queued
        ));
        let fresh = observed_rx
            .recv_timeout(Duration::from_secs(3))
            .expect("the peer reports the request from its own generation");
        assert_eq!(fresh, Some(Request::ListTemplates));

        drop(link);
        daemon.abort();
        remove_socket_files(&path);
    }

    #[test]
    fn an_intent_says_whether_a_failure_is_worth_telling_the_user_about() {
        assert!(Ask::Action("splitting the pane").is_worth_reporting());
        assert!(Ask::Sessions.is_worth_reporting());
        assert!(
            !Ask::Stream.is_worth_reporting(),
            "a banner per keystroke would be unusable"
        );
        assert!(
            !Ask::Activity.is_worth_reporting(),
            "the user did not ask for activity reporting and cannot act on its failure"
        );
    }
}
