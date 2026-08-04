//! Turn's local hook receiver.
//!
//! Agents report to Turn by POSTing a JSON payload here. The design constraints
//! are all about not being in the way of the user's agent:
//!
//! * **It answers immediately.** A hook that hangs stalls the agent that fired
//!   it, so the handler does a hash lookup, a parse, a `try_send` and returns.
//!   Nothing awaits anything downstream, and a full event channel drops the
//!   event rather than applying backpressure to Claude Code.
//! * **It never answers with a decision.** Claude Code's hook protocol allows a
//!   response body that allows or denies a tool call. Turn always replies with an
//!   empty 200: approving on the user's behalf is exactly the thing this product
//!   promises not to do.
//! * **It is only reachable by processes that hold a token.** The listener binds
//!   127.0.0.1 and every registered node gets its own random token, so another
//!   account on the machine cannot forge "Claude is waiting for you" events for
//!   someone else's session. An unknown token is refused and counted.
//! * **It cannot be made to allocate.** The body limit is enforced by the server
//!   before the bytes are buffered, so a hostile `Content-Length` costs nothing.
//! * **It cannot be made to hold resources.** Connections are capped and reaped:
//!   the daemon holds every one of the user's sessions, so a local process that
//!   opens sockets and says nothing must cost a bounded number of file
//!   descriptors and tasks and then be dropped. See [`Limits`], and
//!   `docs/SECURITY.md` for what this does and does not defend against.

use crate::adapter::{AgentAdapter, EventContext, HookEndpoint};
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Router;
use std::collections::HashMap;
use std::future::Future;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot, OwnedSemaphorePermit, Semaphore};
use tokio::time::{Instant, Sleep};
use turn_core::event::TurnEvent;
use turn_core::ids::{NodeId, SessionId};

/// Largest hook payload accepted, before the body is buffered at all.
///
/// Claude Code's biggest payload is a `Stop` carrying the last assistant
/// message; 256 KiB is orders of magnitude more than that and still small enough
/// that a hostile sender cannot cost us memory.
pub const MAX_BODY_BYTES: usize = 256 * 1024;

/// Events buffered for the daemon before new ones are dropped.
///
/// Bounded deliberately. If the daemon stops draining, the correct behaviour is
/// to lose events and say so, not to slow every agent on the machine down to the
/// speed of the slowest consumer.
pub const EVENT_CHANNEL_CAPACITY: usize = 1024;

/// Connections served at once before further ones are closed on arrival.
///
/// A hook post lives for a millisecond, and the machine has tens of agents, not
/// thousands. The cap exists so that a process opening sockets and saying nothing
/// costs a bounded number of file descriptors and tasks: the daemon holds every
/// live session, and running out of descriptors would break the parts of it that
/// have nothing to do with hooks.
pub const MAX_CONNECTIONS: usize = 128;

/// How long a connection may make no progress before it is dropped.
///
/// Measured from the last byte read or written, not from accept, so a client that
/// is genuinely mid-request is never cut off. It catches the two shapes that would
/// otherwise sit in a slot forever: a socket that connects and never speaks, and a
/// `Content-Length` that promises bytes the sender does not intend to send.
///
/// Generous on purpose. Closing an idle keep-alive connection races a client that
/// decides to reuse it at that moment, and losing that race loses a real event —
/// which is worse than a stalled socket lingering, because [`Limits::max_connections`]
/// is what actually bounds the damage. Claude Code gives a hook three seconds, so
/// thirty is far outside any legitimate request.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// Resource bounds for one server. Configurable so the tests can exercise the
/// limits without waiting ten seconds for a reaper.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub max_connections: usize,
    pub idle_timeout: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_connections: MAX_CONNECTIONS,
            idle_timeout: IDLE_TIMEOUT,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error("could not bind the hook server to 127.0.0.1: {0}")]
    Bind(#[source] std::io::Error),
    #[error("could not read the hook server's local address: {0}")]
    LocalAddr(#[source] std::io::Error),
}

/// One node that has been told where to send its hooks.
struct Registration {
    session_id: SessionId,
    node_id: NodeId,
    adapter: Arc<dyn AgentAdapter>,
}

/// Counters the UI can surface, and tests can assert on.
///
/// `refused` is the interesting one: a non-zero value means something on this
/// machine posted to Turn without a valid token.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct HookStats {
    /// Payloads accepted for a registered token.
    pub accepted: u64,
    /// Requests rejected for an unknown token.
    pub refused: u64,
    /// Accepted payloads that were not valid JSON.
    pub unparsable: u64,
    /// Events dropped because the daemon was not draining fast enough.
    pub dropped: u64,
    /// Events handed to the daemon.
    pub emitted: u64,
    /// Connections closed on arrival because [`Limits::max_connections`] were
    /// already open. Non-zero means something is opening sockets faster than
    /// hooks can be served.
    pub overloaded: u64,
    /// Connections dropped for making no progress within [`Limits::idle_timeout`].
    pub timed_out: u64,
}

#[derive(Default)]
struct Counters {
    accepted: AtomicU64,
    refused: AtomicU64,
    unparsable: AtomicU64,
    dropped: AtomicU64,
    emitted: AtomicU64,
    overloaded: AtomicU64,
    timed_out: AtomicU64,
}

struct ServerState {
    registrations: RwLock<HashMap<String, Registration>>,
    events: mpsc::Sender<TurnEvent>,
    /// Shared with the listener, which counts the connections it refuses before
    /// any request exists to attribute them to.
    counters: Arc<Counters>,
}

impl ServerState {
    fn stats(&self) -> HookStats {
        HookStats {
            accepted: self.counters.accepted.load(Ordering::Relaxed),
            refused: self.counters.refused.load(Ordering::Relaxed),
            unparsable: self.counters.unparsable.load(Ordering::Relaxed),
            dropped: self.counters.dropped.load(Ordering::Relaxed),
            emitted: self.counters.emitted.load(Ordering::Relaxed),
            overloaded: self.counters.overloaded.load(Ordering::Relaxed),
            timed_out: self.counters.timed_out.load(Ordering::Relaxed),
        }
    }
}

/// The running hook server.
///
/// Dropping it shuts the listener down, which is what makes it safe to create
/// one per test without leaking ports.
pub struct HookServer {
    state: Arc<ServerState>,
    local_addr: SocketAddr,
    base_url: String,
    helper_path: Option<PathBuf>,
    shutdown: Mutex<Option<oneshot::Sender<()>>>,
}

impl HookServer {
    /// Binds an ephemeral port on the loopback interface and starts serving.
    ///
    /// The receiver is the daemon's end of the event stream; when it is dropped
    /// the server keeps answering agents (they must not start failing) and simply
    /// discards what it normalises.
    pub async fn start() -> Result<(Self, mpsc::Receiver<TurnEvent>), ServerError> {
        Self::start_with_helper(None).await
    }

    /// As [`HookServer::start`], recording where the `turn-hook` helper lives so
    /// every endpoint it hands out can point adapters at it.
    pub async fn start_with_helper(
        helper_path: Option<PathBuf>,
    ) -> Result<(Self, mpsc::Receiver<TurnEvent>), ServerError> {
        Self::start_with(helper_path, Limits::default()).await
    }

    /// As [`HookServer::start_with_helper`], with explicit resource bounds.
    pub async fn start_with(
        helper_path: Option<PathBuf>,
        limits: Limits,
    ) -> Result<(Self, mpsc::Receiver<TurnEvent>), ServerError> {
        let (events_tx, events_rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
        let counters = Arc::new(Counters::default());
        let state = Arc::new(ServerState {
            registrations: RwLock::new(HashMap::new()),
            events: events_tx,
            counters: Arc::clone(&counters),
        });

        // 127.0.0.1 explicitly, never 0.0.0.0: nothing off this machine has any
        // business reporting agent state.
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .map_err(ServerError::Bind)?;
        let local_addr = listener.local_addr().map_err(ServerError::LocalAddr)?;
        let listener = GuardedListener {
            inner: listener,
            permits: Arc::new(Semaphore::new(limits.max_connections.max(1))),
            counters,
            limits,
        };

        let router = Router::new()
            .route("/hook/{token}", post(receive))
            .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
            .with_state(Arc::clone(&state));

        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        tokio::spawn(async move {
            let served = axum::serve(listener, router)
                .with_graceful_shutdown(async move {
                    // A dropped sender also shuts us down, so a leaked handle
                    // cannot keep a port bound forever.
                    let _ = shutdown_rx.await;
                })
                .await;
            if let Err(error) = served {
                tracing::warn!(%error, "the hook server stopped serving");
            }
        });

        Ok((
            Self {
                state,
                local_addr,
                base_url: format!("http://{local_addr}"),
                helper_path,
                shutdown: Mutex::new(Some(shutdown_tx)),
            },
            events_rx,
        ))
    }

    /// Base URL adapters embed in their configuration.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    pub fn stats(&self) -> HookStats {
        self.state.stats()
    }

    /// Issues a fresh token for one node and returns where its hooks should post.
    ///
    /// Callable from any task: registration happens while sessions are being
    /// launched concurrently.
    pub fn register(
        &self,
        session_id: SessionId,
        node_id: NodeId,
        adapter: Arc<dyn AgentAdapter>,
    ) -> HookEndpoint {
        let token = mint_token();
        self.registrations().insert(
            token.clone(),
            Registration {
                session_id,
                node_id,
                adapter,
            },
        );
        HookEndpoint {
            base_url: self.base_url.clone(),
            token,
            helper_path: self.helper_path.clone(),
        }
    }

    /// Revokes a token. Any further post with it is refused like any forgery.
    pub fn unregister(&self, token: &str) -> bool {
        self.registrations().remove(token).is_some()
    }

    /// How many nodes are currently allowed to report.
    pub fn registered(&self) -> usize {
        self.registrations().len()
    }

    /// Stops serving. Idempotent, and also run on drop.
    pub fn shutdown(&self) {
        let taken = self
            .shutdown
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(tx) = taken {
            let _ = tx.send(());
        }
    }

    fn registrations(&self) -> std::sync::RwLockWriteGuard<'_, HashMap<String, Registration>> {
        // A poisoned registry is recoverable: the map is a plain HashMap and a
        // panic mid-insert cannot leave it in a state that breaks an invariant.
        self.state
            .registrations
            .write()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

impl Drop for HookServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Mints a per-node secret.
///
/// Two v4 UUIDs, which is 244 bits of randomness from the operating system's
/// CSPRNG — `uuid`'s v4 generator reads `getrandom`, not a seeded userspace PRNG —
/// rendered as 64 hex characters so it survives being pasted into a settings file,
/// a TOML fragment and an environment variable untouched.
///
/// Far more than a guessing attack needs to be hopeless, and deliberately not a
/// counter or a hash of anything: a token that could be derived from the node id
/// would be worth nothing, since every agent already knows its own node id.
fn mint_token() -> String {
    let high = uuid::Uuid::new_v4().simple().to_string();
    let low = uuid::Uuid::new_v4().simple().to_string();
    format!("{high}{low}")
}

/// A listener that will only hand out a bounded number of live connections, each
/// carrying its own idle deadline.
///
/// This sits under `axum::serve` rather than beside it because the shapes it
/// defends against never produce a request: a socket that connects and stays
/// silent, or a body that stops halfway, is invisible to any middleware — hyper is
/// still reading headers, so nothing downstream has been called yet.
struct GuardedListener {
    inner: TcpListener,
    permits: Arc<Semaphore>,
    counters: Arc<Counters>,
    limits: Limits,
}

impl axum::serve::Listener for GuardedListener {
    type Io = Deadlined<TcpStream>;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            let (stream, addr) = match self.inner.accept().await {
                Ok(accepted) => accepted,
                Err(error) => {
                    // Accept can fail for reasons that resolve themselves — the
                    // peer vanishing, or the process being briefly out of
                    // descriptors. Pausing beats spinning, and giving up would
                    // silently end hook delivery for every session.
                    tracing::warn!(%error, "the hook listener could not accept a connection");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
            };

            match Arc::clone(&self.permits).try_acquire_owned() {
                Ok(permit) => {
                    let _ = stream.set_nodelay(true);
                    return (
                        Deadlined::new(
                            stream,
                            permit,
                            self.limits.idle_timeout,
                            Arc::clone(&self.counters),
                        ),
                        addr,
                    );
                }
                Err(_) => {
                    // Closed immediately, which is the honest answer under load:
                    // queueing it would keep the descriptor and the memory that
                    // the cap exists to bound.
                    self.counters.overloaded.fetch_add(1, Ordering::Relaxed);
                    tracing::warn!(
                        %addr,
                        max_connections = self.limits.max_connections,
                        "refused a hook connection: too many are already open"
                    );
                    drop(stream);
                }
            }
        }
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        self.inner.local_addr()
    }
}

/// A connection that fails once it has been idle too long, and releases its slot
/// when dropped.
///
/// The deadline is polled on the same task as the connection, so the timer's wake
/// is what brings a stalled connection back to be failed. Every byte in either
/// direction pushes it out again: this is an idle timeout, not a lifetime, because
/// cutting off a client mid-request would lose a real event.
struct Deadlined<S> {
    inner: S,
    /// Held for the connection's life. Dropping it frees a slot.
    _permit: OwnedSemaphorePermit,
    deadline: Pin<Box<Sleep>>,
    idle_timeout: Duration,
    counters: Arc<Counters>,
    expired: bool,
}

impl<S> Deadlined<S> {
    fn new(
        inner: S,
        permit: OwnedSemaphorePermit,
        idle_timeout: Duration,
        counters: Arc<Counters>,
    ) -> Self {
        Self {
            inner,
            _permit: permit,
            deadline: Box::pin(tokio::time::sleep(idle_timeout)),
            idle_timeout,
            counters,
            expired: false,
        }
    }

    /// Whether the connection has gone quiet for too long. Registers the task's
    /// waker with the timer, which is what makes an idle connection wake up to be
    /// closed rather than sitting there forever.
    fn stalled(&mut self, cx: &mut Context<'_>) -> bool {
        if self.expired {
            return true;
        }
        if self.deadline.as_mut().poll(cx).is_ready() {
            self.expired = true;
            self.counters.timed_out.fetch_add(1, Ordering::Relaxed);
            tracing::debug!("dropped a hook connection that made no progress");
        }
        self.expired
    }

    fn made_progress(&mut self) {
        let next = Instant::now() + self.idle_timeout;
        self.deadline.as_mut().reset(next);
    }
}

fn stalled_error() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        "the hook connection made no progress",
    )
}

impl<S: AsyncRead + Unpin> AsyncRead for Deadlined<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let me = self.get_mut();
        if me.stalled(cx) {
            return Poll::Ready(Err(stalled_error()));
        }
        let before = buf.filled().len();
        let polled = Pin::new(&mut me.inner).poll_read(cx, buf);
        if let Poll::Ready(Ok(())) = &polled {
            if buf.filled().len() > before {
                me.made_progress();
            }
        }
        polled
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Deadlined<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let me = self.get_mut();
        if me.stalled(cx) {
            return Poll::Ready(Err(stalled_error()));
        }
        let polled = Pin::new(&mut me.inner).poll_write(cx, data);
        if let Poll::Ready(Ok(written)) = &polled {
            if *written > 0 {
                me.made_progress();
            }
        }
        polled
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let me = self.get_mut();
        Pin::new(&mut me.inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let me = self.get_mut();
        Pin::new(&mut me.inner).poll_shutdown(cx)
    }
}

/// The one route. Everything about it is shaped by "do not make the agent wait".
async fn receive(
    State(state): State<Arc<ServerState>>,
    Path(token): Path<String>,
    body: Bytes,
) -> Response {
    let Some((session_id, node_id, adapter)) = lookup(&state, &token) else {
        state.counters.refused.fetch_add(1, Ordering::Relaxed);
        // Logged, because a post with a bad token means either a stale agent
        // from a previous daemon run or something on this machine trying it on.
        tracing::warn!(
            token_len = token.len(),
            "refused a hook post with an unknown token"
        );
        return StatusCode::NOT_FOUND.into_response();
    };

    state.counters.accepted.fetch_add(1, Ordering::Relaxed);

    // From here on the answer is always 200. A registered agent has done nothing
    // wrong even if its payload is unreadable, and telling it otherwise risks it
    // treating Turn as a failing hook.
    let payload: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(payload) => payload,
        Err(error) => {
            state.counters.unparsable.fetch_add(1, Ordering::Relaxed);
            tracing::debug!(%error, adapter = adapter.id(), "unreadable hook payload");
            return StatusCode::OK.into_response();
        }
    };

    let ctx = EventContext {
        session_id,
        node_id,
        timestamp_ms: turn_core::now_ms(),
    };
    for event in adapter.normalise(&payload, &ctx) {
        // try_send, never send: the agent is holding a connection open while we
        // do this, so a slow daemon must cost us the event and not the turn.
        match state.events.try_send(event) {
            Ok(()) => {
                state.counters.emitted.fetch_add(1, Ordering::Relaxed);
            }
            Err(mpsc::error::TrySendError::Full(dropped)) => {
                state.counters.dropped.fetch_add(1, Ordering::Relaxed);
                tracing::warn!(
                    kind = turn_core::event::event_name(&dropped.kind),
                    "dropped an event: the daemon is not draining the hook channel"
                );
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                state.counters.dropped.fetch_add(1, Ordering::Relaxed);
                tracing::debug!("dropped an event: nothing is listening any more");
            }
        }
    }

    StatusCode::OK.into_response()
}

/// Resolves a token without holding the lock across normalisation.
fn lookup(state: &ServerState, token: &str) -> Option<(SessionId, NodeId, Arc<dyn AgentAdapter>)> {
    // A read lock held only for the clone. Normalising a payload is pure but not
    // instant, and it must not block a concurrent registration.
    let registrations = state
        .registrations
        .read()
        .unwrap_or_else(PoisonError::into_inner);
    let registration = registrations.get(token)?;
    Some((
        registration.session_id.clone(),
        registration.node_id.clone(),
        Arc::clone(&registration.adapter),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude::ClaudeCodeAdapter;
    use serde_json::json;
    use turn_core::event::{Confidence, EventKind};

    /// Starts a server with one Claude Code node registered.
    async fn server_with_node() -> (HookServer, mpsc::Receiver<TurnEvent>, HookEndpoint) {
        server_with_limits(Limits::default()).await
    }

    async fn server_with_limits(
        limits: Limits,
    ) -> (HookServer, mpsc::Receiver<TurnEvent>, HookEndpoint) {
        let (server, rx) = HookServer::start_with(None, limits)
            .await
            .expect("the server must bind");
        let endpoint = server.register(
            SessionId::from_stored("sess_server01"),
            NodeId::from_stored("proc_server01"),
            Arc::new(ClaudeCodeAdapter::new()),
        );
        (server, rx, endpoint)
    }

    /// Sends a request written by hand, so a test can lie about what follows.
    async fn raw_request(addr: SocketAddr, request: &str) -> TcpStream {
        use tokio::io::AsyncWriteExt;
        let mut stream = TcpStream::connect(addr).await.expect("must connect");
        stream
            .write_all(request.as_bytes())
            .await
            .expect("must write");
        stream
    }

    /// Waits for a condition, so a test never depends on a fixed sleep.
    async fn eventually(label: &str, mut condition: impl FnMut() -> bool) {
        for _ in 0..200 {
            if condition() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("timed out waiting for: {label}");
    }

    async fn post(url: &str, body: serde_json::Value) -> StatusCode {
        let response = reqwest::Client::new()
            .post(url)
            .json(&body)
            .send()
            .await
            .expect("the local server must answer");
        StatusCode::from_u16(response.status().as_u16()).expect("a real status code")
    }

    #[tokio::test]
    async fn the_server_listens_only_on_loopback() {
        let (server, _rx) = HookServer::start().await.unwrap();
        assert!(server.local_addr().ip().is_loopback());
        assert_ne!(server.local_addr().port(), 0, "an ephemeral port was bound");
        assert!(server.base_url().starts_with("http://127.0.0.1:"));
    }

    /// The end-to-end path: a real Claude Code payload over a real socket
    /// arrives as a normalised event.
    #[tokio::test]
    async fn a_real_stop_payload_arrives_as_a_completed_turn() {
        let (server, mut rx, endpoint) = server_with_node().await;

        let status = post(
            &endpoint.url(),
            json!({
                "hook_event_name": "Stop",
                "session_id": "84cde77e-f54f-41e7-bb05-2716cb61b6bf",
                "cwd": "/private/tmp",
                "last_assistant_message": "OK",
                "background_tasks": [],
                "stop_hook_active": false
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let event = rx.recv().await.expect("an event must arrive");
        match &event.kind {
            EventKind::AgentTurnCompleted {
                last_message,
                background_tasks,
            } => {
                assert_eq!(last_message.as_deref(), Some("OK"));
                assert_eq!(*background_tasks, 0);
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(event.session_id.as_str(), "sess_server01");
        assert_eq!(event.node_id.as_ref().unwrap().as_str(), "proc_server01");
        assert_eq!(event.confidence, Confidence::Explicit);
        assert_eq!(server.stats().emitted, 1);
        assert_eq!(server.stats().refused, 0);
    }

    /// The security property: knowing the port is not enough.
    #[tokio::test]
    async fn a_forged_post_with_a_wrong_token_is_refused_and_emits_nothing() {
        let (server, mut rx, endpoint) = server_with_node().await;

        let forged = format!("{}/hook/{}", server.base_url(), "f".repeat(64));
        let status = post(&forged, json!({ "hook_event_name": "Stop" })).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(server.stats().refused, 1);
        assert_eq!(
            rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty),
            "a forged post must not be able to move a session's state"
        );

        // And the legitimate token still works, so the refusal was targeted.
        assert_eq!(
            post(&endpoint.url(), json!({ "hook_event_name": "SessionEnd" })).await,
            StatusCode::OK
        );
        assert!(rx.recv().await.is_some());
    }

    #[tokio::test]
    async fn a_revoked_token_stops_working_immediately() {
        let (server, mut rx, endpoint) = server_with_node().await;
        assert_eq!(server.registered(), 1);

        assert!(server.unregister(&endpoint.token));
        assert!(
            !server.unregister(&endpoint.token),
            "revoking twice is not an error but is not a second success either"
        );
        assert_eq!(server.registered(), 0);

        let status = post(&endpoint.url(), json!({ "hook_event_name": "Stop" })).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(rx.try_recv(), Err(mpsc::error::TryRecvError::Empty));
    }

    /// A registered agent always gets a 200, whatever it sends. Anything else
    /// risks Claude Code deciding Turn's hook is broken.
    #[tokio::test]
    async fn a_registered_agent_gets_two_hundred_even_when_nothing_can_be_derived() {
        let (server, mut rx, endpoint) = server_with_node().await;

        // Valid JSON the adapter has no mapping for.
        assert_eq!(
            post(&endpoint.url(), json!({ "hook_event_name": "PostToolUse" })).await,
            StatusCode::OK
        );
        // Valid JSON that is not even an object.
        assert_eq!(
            post(&endpoint.url(), json!([1, 2, 3])).await,
            StatusCode::OK
        );

        // Outright malformed body.
        let response = reqwest::Client::new()
            .post(endpoint.url())
            .header("content-type", "application/json")
            .body("{not json at all")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), 200);

        assert_eq!(rx.try_recv(), Err(mpsc::error::TryRecvError::Empty));
        let stats = server.stats();
        assert_eq!(stats.accepted, 3);
        assert_eq!(stats.unparsable, 1);
        assert_eq!(stats.emitted, 0);
    }

    /// Turn never answers a hook with an allow/deny decision, because Turn never
    /// makes one. The body must stay empty.
    #[tokio::test]
    async fn the_response_never_carries_a_permission_decision() {
        let (_server, _rx, endpoint) = server_with_node().await;

        let response = reqwest::Client::new()
            .post(endpoint.url())
            .json(&json!({
                "hook_event_name": "PermissionRequest",
                "tool_name": "Bash",
                "tool_input": { "command": "rm -rf /" }
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), 200);
        let body = response.text().await.unwrap();
        assert!(
            body.is_empty(),
            "Turn must not return a hook decision, got {body:?}"
        );
    }

    /// A hostile sender must not be able to make the server allocate.
    #[tokio::test]
    async fn an_oversized_payload_is_rejected_rather_than_buffered() {
        let (server, mut rx, endpoint) = server_with_node().await;

        let huge = json!({
            "hook_event_name": "Stop",
            "last_assistant_message": "z".repeat(MAX_BODY_BYTES + 4_096)
        });
        let response = reqwest::Client::new()
            .post(endpoint.url())
            .json(&huge)
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status().as_u16(),
            413,
            "the body limit must be enforced before the payload is buffered"
        );
        assert_eq!(rx.try_recv(), Err(mpsc::error::TryRecvError::Empty));

        // A payload just under the limit still works, so the cap is not so tight
        // that a long assistant message is lost.
        let acceptable = json!({
            "hook_event_name": "Stop",
            "last_assistant_message": "z".repeat(1_024)
        });
        assert_eq!(post(&endpoint.url(), acceptable).await, StatusCode::OK);
        assert!(rx.recv().await.is_some());
        assert_eq!(server.stats().dropped, 0);
    }

    #[tokio::test]
    async fn two_nodes_get_different_tokens_and_their_events_stay_apart() {
        let (server, mut rx) = HookServer::start().await.unwrap();
        let adapter: Arc<dyn AgentAdapter> = Arc::new(ClaudeCodeAdapter::new());
        let first = server.register(
            SessionId::from_stored("sess_one"),
            NodeId::from_stored("proc_one"),
            Arc::clone(&adapter),
        );
        let second = server.register(
            SessionId::from_stored("sess_two"),
            NodeId::from_stored("proc_two"),
            adapter,
        );
        assert_ne!(first.token, second.token);
        assert_eq!(first.token.len(), 64, "256 bits of hex");

        post(&second.url(), json!({ "hook_event_name": "SessionEnd" })).await;
        let event = rx.recv().await.unwrap();
        assert_eq!(event.session_id.as_str(), "sess_two");
        assert_eq!(event.node_id.as_ref().unwrap().as_str(), "proc_two");
    }

    /// Registration happens while sessions launch concurrently, so it must not
    /// need external synchronisation.
    #[tokio::test]
    async fn registration_is_safe_from_many_tasks_at_once() {
        let (server, _rx) = HookServer::start().await.unwrap();
        let server = Arc::new(server);

        let mut tasks = Vec::new();
        for index in 0..32 {
            let server = Arc::clone(&server);
            tasks.push(tokio::spawn(async move {
                server
                    .register(
                        SessionId::from_stored(format!("sess_{index}")),
                        NodeId::from_stored(format!("proc_{index}")),
                        Arc::new(ClaudeCodeAdapter::new()),
                    )
                    .token
            }));
        }

        let mut tokens = Vec::new();
        for task in tasks {
            tokens.push(task.await.expect("no task may panic"));
        }
        assert_eq!(server.registered(), 32);
        tokens.sort();
        tokens.dedup();
        assert_eq!(tokens.len(), 32, "every token must be unique");
    }

    /// If the daemon stops draining, agents must keep working.
    #[tokio::test]
    async fn a_dropped_receiver_does_not_start_failing_the_agents_hooks() {
        let (server, rx, endpoint) = server_with_node().await;
        drop(rx);

        for _ in 0..5 {
            assert_eq!(
                post(&endpoint.url(), json!({ "hook_event_name": "SessionEnd" })).await,
                StatusCode::OK,
                "the agent must never see a hook failure because Turn went away"
            );
        }
        assert_eq!(server.stats().dropped, 5);
        assert_eq!(server.stats().emitted, 0);
    }

    #[tokio::test]
    async fn a_full_channel_drops_events_instead_of_blocking_the_agent() {
        let (server, _rx, endpoint) = server_with_node().await;

        // Fill the channel and then some. Every request must still answer.
        for _ in 0..(EVENT_CHANNEL_CAPACITY + 16) {
            assert_eq!(
                post(&endpoint.url(), json!({ "hook_event_name": "SessionEnd" })).await,
                StatusCode::OK
            );
        }
        let stats = server.stats();
        assert_eq!(stats.emitted, EVENT_CHANNEL_CAPACITY as u64);
        assert_eq!(stats.dropped, 16);
    }

    #[tokio::test]
    async fn an_endpoint_carries_the_helper_path_when_one_is_configured() {
        let (server, _rx) =
            HookServer::start_with_helper(Some(PathBuf::from("/opt/turn/bin/turn-hook")))
                .await
                .unwrap();
        let endpoint = server.register(
            SessionId::from_stored("sess_helper"),
            NodeId::from_stored("proc_helper"),
            Arc::new(ClaudeCodeAdapter::new()),
        );
        assert_eq!(
            endpoint.helper_path.as_deref(),
            Some(std::path::Path::new("/opt/turn/bin/turn-hook"))
        );
    }

    /// Every wrong token gets the same answer, whatever is wrong with it. A
    /// different status, a different body or a different counter for "close but
    /// not quite" would turn the endpoint into an oracle.
    #[tokio::test]
    async fn no_variation_on_a_wrong_token_gets_a_different_answer() {
        let (server, mut rx, endpoint) = server_with_node().await;
        let real = endpoint.token.clone();

        let mut wrong = vec![
            // Right length, right alphabet, wrong value.
            "f".repeat(64),
            // The real token with one character changed.
            format!(
                "{}{}",
                &real[..63],
                if real.ends_with('a') { 'b' } else { 'a' }
            ),
            // The real token, truncated and extended.
            real[..32].to_string(),
            format!("{real}00"),
            // Casing, in case a lookup ever stops being exact.
            real.to_uppercase(),
            // Shapes that are not tokens at all.
            "0".to_string(),
            "%2e%2e%2f".to_string(),
        ];
        wrong.retain(|candidate| *candidate != real);

        let client = reqwest::Client::new();
        let mut answers = Vec::new();
        for candidate in &wrong {
            let response = client
                .post(format!("{}/hook/{candidate}", server.base_url()))
                .json(&json!({ "hook_event_name": "Stop" }))
                .send()
                .await
                .expect("the server must answer");
            answers.push((response.status().as_u16(), response.text().await.unwrap()));
        }

        assert!(
            answers.iter().all(|answer| *answer == (404, String::new())),
            "a wrong token must be indistinguishable from any other: {answers:?}"
        );
        assert_eq!(server.stats().refused, wrong.len() as u64);
        assert_eq!(server.stats().accepted, 0);
        assert_eq!(rx.try_recv(), Err(mpsc::error::TryRecvError::Empty));
    }

    /// The token is only accepted where it belongs. A valid token in a header, a
    /// query string or a different route is not a way in.
    #[tokio::test]
    async fn a_valid_token_only_works_in_the_path_it_was_issued_for() {
        let (server, mut rx, endpoint) = server_with_node().await;
        let client = reqwest::Client::new();
        let body = json!({ "hook_event_name": "SessionEnd" });

        for url in [
            format!("{}/hook/", server.base_url()),
            format!("{}/hook", server.base_url()),
            format!("{}/hook/{}/extra", server.base_url(), endpoint.token),
            format!("{}/?token={}", server.base_url(), endpoint.token),
        ] {
            let status = client
                .post(&url)
                .json(&body)
                .send()
                .await
                .expect("must answer")
                .status();
            assert!(status.is_client_error(), "{url} answered {status}");
        }

        // And a GET is not a way to reach the handler either.
        let status = client
            .get(endpoint.url())
            .send()
            .await
            .expect("must answer")
            .status();
        assert_eq!(status.as_u16(), 405);

        assert_eq!(rx.try_recv(), Err(mpsc::error::TryRecvError::Empty));
        assert_eq!(server.stats().accepted, 0);
    }

    /// The whole path, with a payload written to attack the user rather than the
    /// server: a valid token, a real event name, and every string field carrying
    /// something that should not survive into the UI.
    #[tokio::test]
    async fn a_hostile_payload_from_a_registered_agent_arrives_sanitised() {
        let (_server, mut rx, endpoint) = server_with_node().await;

        let status = post(
            &endpoint.url(),
            json!({
                "hook_event_name": "PermissionRequest",
                // Would become an argv entry when the session is resumed.
                "session_id": "--dangerously-skip-permissions",
                // Would rewrite the user's clipboard and clear their screen.
                "model": "claude\u{1b}]52;c;cGF5bG9hZA==\u{7}\u{1b}[2J",
                "tool_name": "Bash",
                "tool_input": {
                    // Reads as `touch safe.txt` with the override applied.
                    "command": "rm -rf ~/work \u{202e}txt.efas hcuot\u{202c}",
                    // A second log record, forged out of one field.
                    "description": "harmless\nWARN turn: approved automatically"
                }
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let event = rx.recv().await.expect("an event must arrive");
        let rendered = serde_json::to_string(&event).expect("events serialise");

        // Nothing anywhere in the event — fields or retained payload — can
        // misrepresent itself. The payload keeps its control characters, but JSON
        // encoding has turned them into six readable characters rather than a live
        // escape sequence, which is why this holds for the whole document.
        let offenders: Vec<char> = rendered
            .chars()
            .filter(|c| !turn_pty::is_display_safe(*c))
            .collect();
        assert!(
            offenders.is_empty(),
            "characters that lie about themselves reached a stored event: \
             {offenders:?} in {rendered}"
        );
        assert!(
            event
                .raw
                .as_deref()
                .expect("the payload is kept")
                .contains("\\u001b"),
            "and the payload still records that an escape byte was sent"
        );
        assert_eq!(
            event.agent.model.as_deref(),
            Some("claude"),
            "the readable part of a field survives, the rest does not"
        );

        match &event.kind {
            EventKind::AgentPermissionRequired { command, risk, .. } => {
                assert_eq!(command.as_deref(), Some("rm -rf ~/work txt.efas hcuot"));
                assert_eq!(
                    *risk,
                    turn_core::event::Risk::High,
                    "and the rating still saw the real command"
                );
            }
            other => panic!("unexpected {other:?}"),
        }

        // The forged flag was refused rather than repaired, so there is nothing to
        // pass to `--resume`.
        let started = post(
            &endpoint.url(),
            json!({
                "hook_event_name": "SessionStart",
                "session_id": "--dangerously-skip-permissions"
            }),
        )
        .await;
        assert_eq!(started, StatusCode::OK);
        match &rx.recv().await.unwrap().kind {
            EventKind::AgentStarted { external_id, .. } => assert_eq!(*external_id, None),
            other => panic!("unexpected {other:?}"),
        }
    }

    /// A socket that connects and says nothing must cost a bounded number of
    /// slots and then be let go. Both halves matter: without the cap a local
    /// process could exhaust the daemon's file descriptors, and without the
    /// reaper it could park every slot indefinitely.
    #[tokio::test]
    async fn silent_connections_are_capped_and_then_reaped() {
        let (server, mut rx, endpoint) = server_with_limits(Limits {
            max_connections: 4,
            idle_timeout: Duration::from_millis(150),
        })
        .await;

        // Forty sockets that connect and never speak.
        let mut held = Vec::new();
        for _ in 0..40 {
            held.push(
                TcpStream::connect(server.local_addr())
                    .await
                    .expect("connecting must still succeed"),
            );
        }

        eventually("the cap to engage", || server.stats().overloaded > 0).await;
        assert!(
            server.stats().overloaded >= 30,
            "most of the flood must be refused on arrival, got {:?}",
            server.stats()
        );

        // The few that got a slot are given up on, so the server recovers without
        // the attacker doing anything.
        eventually("the idle connections to be reaped", || {
            server.stats().timed_out > 0
        })
        .await;

        // And a real hook is served again.
        assert_eq!(
            post(&endpoint.url(), json!({ "hook_event_name": "SessionEnd" })).await,
            StatusCode::OK
        );
        assert!(rx.recv().await.is_some());
        drop(held);
    }

    /// A `Content-Length` that promises bytes the sender never sends must not
    /// leave a request half-applied, and must not stall anything else.
    #[tokio::test]
    async fn a_content_length_that_lies_forges_nothing_and_blocks_nobody() {
        let (server, mut rx, endpoint) = server_with_limits(Limits {
            max_connections: 8,
            idle_timeout: Duration::from_millis(150),
        })
        .await;

        // A valid token, a valid start of a payload, and then silence.
        let path = format!("/hook/{}", endpoint.token);
        let liar = raw_request(
            server.local_addr(),
            &format!(
                "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\n\
                 Content-Length: 4096\r\n\r\n{{\"hook_event_name\":\"St"
            ),
        )
        .await;

        // Another agent's hook is served while that one hangs.
        assert_eq!(
            post(&endpoint.url(), json!({ "hook_event_name": "SessionEnd" })).await,
            StatusCode::OK
        );
        assert!(rx.recv().await.is_some());

        eventually("the stalled request to be dropped", || {
            server.stats().timed_out > 0
        })
        .await;
        assert_eq!(
            rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty),
            "half a payload must never become an event"
        );
        drop(liar);
    }

    /// A body with no declared length must be cut off at the same limit as one
    /// that declares an honest length, or the cap is only a suggestion.
    #[tokio::test]
    async fn a_chunked_body_is_cut_off_at_the_same_limit_as_a_declared_one() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let (server, mut rx, endpoint) = server_with_node().await;
        let path = format!("/hook/{}", endpoint.token);
        let mut stream = raw_request(
            server.local_addr(),
            &format!(
                "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\n\
                 Transfer-Encoding: chunked\r\n\r\n"
            ),
        )
        .await;

        // Chunks that never end, well past the cap.
        let chunk = format!("{:x}\r\n{}\r\n", 8 * 1024, "z".repeat(8 * 1024));
        let mut sent = 0usize;
        let mut refused = false;
        while sent < MAX_BODY_BYTES * 4 {
            if stream.write_all(chunk.as_bytes()).await.is_err() {
                refused = true;
                break;
            }
            sent += 8 * 1024;
        }
        let mut answer = String::new();
        let _ = stream.read_to_string(&mut answer).await;

        assert!(
            refused || answer.contains("413") || answer.is_empty(),
            "an endless chunked body must not be accepted whole: {answer:?}"
        );
        assert_eq!(rx.try_recv(), Err(mpsc::error::TryRecvError::Empty));
        assert_eq!(server.stats().emitted, 0);
    }

    /// JSON nested thousands of levels deep is a stack overflow if the parser has
    /// no depth limit — and a stack overflow in the daemon is every session lost.
    #[tokio::test]
    async fn deeply_nested_json_is_refused_instead_of_overflowing_the_stack() {
        let (server, mut rx, endpoint) = server_with_node().await;

        for depth in [128, 10_000, 60_000] {
            let mut body = "[".repeat(depth);
            body.push_str(&"]".repeat(depth));
            let response = reqwest::Client::new()
                .post(endpoint.url())
                .header("content-type", "application/json")
                .body(body)
                .send()
                .await
                .expect("the server must survive to answer");
            assert_eq!(
                response.status().as_u16(),
                200,
                "a registered agent still gets a 200 at depth {depth}"
            );
        }

        // Nested objects too, which is the shape that would recurse through the
        // adapter rather than only the parser.
        let mut body = String::new();
        for index in 0..10_000 {
            body.push_str(&format!("{{\"k{index}\":"));
        }
        body.push_str("null");
        body.push_str(&"}".repeat(10_000));
        assert_eq!(
            reqwest::Client::new()
                .post(endpoint.url())
                .header("content-type", "application/json")
                .body(body)
                .send()
                .await
                .unwrap()
                .status()
                .as_u16(),
            200
        );

        assert_eq!(rx.try_recv(), Err(mpsc::error::TryRecvError::Empty));
        assert_eq!(server.stats().emitted, 0);
        assert_eq!(
            server.stats().unparsable,
            4,
            "each one was rejected by the parser rather than walked"
        );

        // Still healthy afterwards.
        assert_eq!(
            post(&endpoint.url(), json!({ "hook_event_name": "SessionEnd" })).await,
            StatusCode::OK
        );
        assert!(rx.recv().await.is_some());
    }

    /// A body that is not UTF-8 at all, including the encodings JSON forbids.
    #[tokio::test]
    async fn a_body_that_is_not_text_is_counted_and_answered_rather_than_fatal() {
        let (server, mut rx, endpoint) = server_with_node().await;
        let client = reqwest::Client::new();

        for body in [
            // Invalid UTF-8.
            vec![0xff, 0xfe, 0x00, 0x80],
            // A lone surrogate, which JSON may spell but not mean.
            br#"{"hook_event_name":"\ud800"}"#.to_vec(),
            // A NUL inside a string.
            b"{\"hook_event_name\":\"St\0op\"}".to_vec(),
        ] {
            let status = client
                .post(endpoint.url())
                .header("content-type", "application/json")
                .body(body.clone())
                .send()
                .await
                .expect("the server must answer")
                .status();
            assert_eq!(status.as_u16(), 200, "body {body:?}");
        }
        assert_eq!(server.stats().unparsable, 3);
        assert_eq!(rx.try_recv(), Err(mpsc::error::TryRecvError::Empty));
    }

    /// A hundred agents posting at once must all be answered, and the numbers must
    /// add up afterwards.
    #[tokio::test]
    async fn a_hundred_concurrent_posts_are_all_served_and_accounted_for() {
        let (server, mut rx) = HookServer::start().await.unwrap();
        let server = Arc::new(server);
        let adapter: Arc<dyn AgentAdapter> = Arc::new(ClaudeCodeAdapter::new());

        let mut tasks = Vec::new();
        for index in 0..100 {
            let endpoint = server.register(
                SessionId::from_stored(format!("sess_{index}")),
                NodeId::from_stored(format!("proc_{index}")),
                Arc::clone(&adapter),
            );
            tasks.push(tokio::spawn(async move {
                post(&endpoint.url(), json!({ "hook_event_name": "SessionEnd" })).await
            }));
        }
        for task in tasks {
            assert_eq!(task.await.unwrap(), StatusCode::OK);
        }

        let stats = server.stats();
        assert_eq!(stats.accepted, 100);
        assert_eq!(stats.emitted, 100);
        assert_eq!(stats.refused, 0);
        assert_eq!(stats.dropped, 0);

        let mut seen = std::collections::HashSet::new();
        for _ in 0..100 {
            let event = rx.recv().await.expect("every event arrives");
            seen.insert(event.session_id.as_str().to_string());
        }
        assert_eq!(seen.len(), 100, "every session's event stayed its own");
    }

    #[tokio::test]
    async fn shutting_down_releases_the_port() {
        let (server, _rx) = HookServer::start().await.unwrap();
        let addr = server.local_addr();
        server.shutdown();
        server.shutdown(); // idempotent
        drop(server);

        // The listener closes asynchronously; retry briefly rather than racing.
        let mut rebound = false;
        for _ in 0..50 {
            if tokio::net::TcpListener::bind(addr).await.is_ok() {
                rebound = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(rebound, "a shut-down server must not keep {addr} bound");
    }
}
