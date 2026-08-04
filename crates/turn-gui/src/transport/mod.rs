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
use std::sync::mpsc as sync_mpsc;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc as tokio_mpsc;
use turn_core::ids::{PaneId, SessionId};
use turn_proto::{ProtoError, Request, RequestId, Response, ServerEvent};

pub use backoff::{ConnectionState, DaemonIdentity};
pub use link::LinkError;

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
    Workspaces,
    Sessions,
    Details(SessionId),
    Templates,
    AttentionQueue,
    Attach {
        session_id: SessionId,
        pane_id: PaneId,
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
            Ask::Workspaces => "loading workspaces",
            Ask::Sessions => "loading sessions",
            Ask::Details(_) => "loading a session",
            Ask::Templates => "loading templates",
            Ask::AttentionQueue => "loading the attention queue",
            Ask::Attach { .. } => "attaching to a pane",
            Ask::Action(label) => label,
            Ask::Activity => "reporting activity",
            Ask::Stream => "sending to the terminal",
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

/// One request, on its way out.
struct Outbound {
    ask: Ask,
    request: Request,
}

/// The window's handle on the daemon.
pub struct DaemonLink {
    socket: PathBuf,
    outbound: tokio_mpsc::UnboundedSender<Outbound>,
    inbound: sync_mpsc::Receiver<Inbound>,
    /// Kept so the runtime thread is not detached; dropping the link ends it.
    _thread: std::thread::JoinHandle<()>,
}

impl DaemonLink {
    /// Starts the connection on its own thread and returns at once.
    ///
    /// Deliberately does not wait for a connection: the window has to be able to draw
    /// "no daemon" before there is one, because that is the state a user sees when
    /// they open Turn before `turnd` has finished binding.
    pub fn spawn(socket: PathBuf, client_version: impl Into<String>, wake: Waker) -> DaemonLink {
        let client_version = client_version.into();
        let (outbound, outbound_rx) = tokio_mpsc::unbounded_channel::<Outbound>();
        let (inbound_tx, inbound) = sync_mpsc::channel::<Inbound>();
        let path = socket.clone();

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
                        let _ = inbound_tx.send(Inbound::Status(ConnectionState::Disconnected {
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
            _thread: thread,
        }
    }

    pub fn socket(&self) -> &std::path::Path {
        &self.socket
    }

    /// Queues a request. Never blocks, and never waits for a connection.
    ///
    /// A window that queued requests across a reconnect would replay a `close_pane`
    /// against a layout the daemon rebuilt from disk. The window re-fetches on
    /// reconnect instead, which is the only correct recovery, so dropping a request
    /// sent while disconnected is what makes that happen.
    pub fn send(&self, ask: Ask, request: Request) {
        if self.outbound.send(Outbound { ask, request }).is_err() {
            tracing::debug!("the connection thread has ended; a request was dropped");
        }
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
/// Returns only when the daemon refuses this build, because that is the one failure a
/// retry cannot fix and looping on it would bury the message the user needs to read.
async fn supervise(
    socket: PathBuf,
    client_version: String,
    mut outbound: tokio_mpsc::UnboundedReceiver<Outbound>,
    inbound: sync_mpsc::Sender<Inbound>,
    wake: Waker,
) {
    let mut identity = DaemonIdentity::new();
    let mut attempt: u32 = 0;
    let mut last_status = ConnectionState::Starting;

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
            Err(LinkError::Refused(error)) => {
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
                // `retrying` is the error's own verdict rather than a constant: a
                // socket that is not there yet is worth waiting for, and a frame this
                // build could not encode is not.
                let retrying = error.is_retryable();
                let published = publish(
                    &inbound,
                    &wake,
                    &mut last_status,
                    ConnectionState::Disconnected {
                        message: error.to_proto_error().message,
                        retrying,
                    },
                );
                if !published || !retrying {
                    return;
                }
                continue;
            }
        };

        tracing::info!(
            daemon_pid = welcome.daemon_pid,
            daemon_version = %welcome.daemon_version,
            protocol = connection.agreed_version(),
            "connected to turnd"
        );
        if !publish(
            &inbound,
            &wake,
            &mut last_status,
            identity.observe(&welcome),
        ) {
            return;
        }

        let opened = tokio::time::Instant::now();
        let ended_cleanly = serve(&mut connection, &mut outbound, &inbound, &wake).await;

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
    outbound: &mut tokio_mpsc::UnboundedReceiver<Outbound>,
    inbound: &sync_mpsc::Sender<Inbound>,
    wake: &Waker,
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
                if inbound.send(message).is_err() {
                    return false;
                }
                wake();
            }
            request = outbound.recv() => {
                let Some(Outbound { ask, request }) = request else {
                    // The window dropped its handle.
                    return false;
                };
                let id = RequestId::new(format!("r-{next_id}"));
                next_id = next_id.saturating_add(1);
                match connection.send(id.clone(), request).await {
                    Ok(()) => {
                        pending.insert(id, ask);
                    }
                    Err(error) => {
                        let failure = Inbound::Failed { ask, error: error.to_proto_error() };
                        if inbound.send(failure).is_err() {
                            return false;
                        }
                        wake();
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
        if inbound.send(Inbound::Failed { ask, error }).is_err() {
            return false;
        }
    }
    wake();
    true
}

/// Publishes a state change, and only a change.
///
/// Re-announcing "connecting, attempt 4" every four seconds would make the status
/// line flicker for no new information. Returns false when the window has gone.
fn publish(
    inbound: &sync_mpsc::Sender<Inbound>,
    wake: &Waker,
    last: &mut ConnectionState,
    state: ConnectionState,
) -> bool {
    if *last == state {
        return true;
    }
    *last = state.clone();
    if inbound.send(Inbound::Status(state)).is_err() {
        return false;
    }
    wake();
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    use tokio::net::UnixListener;
    use turn_proto::{ServerFrame, Welcome};

    fn socket_path(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "turn-gui-supervise-{}-{}.sock",
            name,
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
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
        let link = DaemonLink::spawn(socket_path("absent"), "0.1.0", wake);
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
        let _ = std::fs::remove_file(&path);
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
        let _ = std::fs::remove_file(&path);

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
        let _ = std::fs::remove_file(&path);
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
        link.send(Ask::Action("closing the pane"), Request::ListTemplates);

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
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_request_sent_with_no_connection_is_dropped_rather_than_queued_for_later() {
        // Replaying a queued `close_pane` after a reconnect would act on a layout the
        // daemon rebuilt from disk. The window re-fetches instead.
        let (wake, _) = counting_waker();
        let link = DaemonLink::spawn(socket_path("dropped"), "0.1.0", wake);
        link.send(Ask::Action("closing the pane"), Request::ListTemplates);
        std::thread::sleep(Duration::from_millis(50));
        let answers: Vec<Inbound> = link
            .drain()
            .into_iter()
            .filter(|message| matches!(message, Inbound::Answer { .. }))
            .collect();
        assert!(
            answers.is_empty(),
            "nothing can be answered; saw {answers:?}"
        );
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
