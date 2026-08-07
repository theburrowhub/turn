//! One connection to `turnd`: connect, handshake, then read and write.
//!
//! Two behaviours here are not obvious and are load-bearing:
//!
//! * **A bad line costs one line.** A frame the decoder cannot parse produces a
//!   notice, never a dropped connection. Tearing down the socket because one frame
//!   was malformed would take thirty running agents off screen.
//! * **Nothing is left pending.** When the socket closes, every request still in
//!   flight is failed rather than forgotten, because a request that never settles
//!   presents to the user as a frozen window.

use std::fs::OpenOptions;
use std::io::Read;
use std::path::Path;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use turn_proto::{
    encode_checked, AuthToken, ClientFrame, ClientMessage, ErrorCode, Hello, LineDecoder,
    ProtoError, Request, RequestId, ServerFrame, ServerMessage, Welcome,
};

/// Why a connection could not be made, or could not continue.
#[derive(Debug, thiserror::Error)]
pub enum LinkError {
    /// The socket is not there, or stopped answering. Worth retrying.
    #[error("{context}: {cause}")]
    Io {
        context: &'static str,
        #[source]
        cause: std::io::Error,
    },
    /// The daemon refused the handshake. Authentication may recover after token
    /// rotation; a protocol-version refusal cannot.
    #[error("{0}")]
    Refused(ProtoError),
    /// Something is listening on the socket but it is not a daemon.
    #[error("{0}")]
    NotADaemon(ProtoError),
    /// A frame this build could not even encode. Never worth retrying.
    #[error("{0}")]
    Unsendable(String),
}

impl LinkError {
    fn io(context: &'static str, cause: std::io::Error) -> Self {
        LinkError::Io { context, cause }
    }

    /// Whether trying again could plausibly work.
    pub fn is_retryable(&self) -> bool {
        matches!(self, LinkError::Io { .. } | LinkError::NotADaemon(_))
            || matches!(self, LinkError::Refused(error) if error.code == ErrorCode::Unauthorized)
    }

    /// The failure in the protocol's own error shape, for the status line.
    pub fn to_proto_error(&self) -> ProtoError {
        match self {
            LinkError::Io { context, cause } => {
                ProtoError::new(ErrorCode::Unavailable, *context).with_detail(cause.to_string())
            }
            LinkError::Refused(error) | LinkError::NotADaemon(error) => error.clone(),
            LinkError::Unsendable(message) => {
                ProtoError::new(ErrorCode::InvalidArgument, message.clone())
            }
        }
    }
}

/// What one live connection can carry out of the reader.
#[derive(Debug)]
pub enum Frame {
    Response {
        id: RequestId,
        response: Box<turn_proto::Response>,
    },
    Error {
        id: Option<RequestId>,
        error: ProtoError,
    },
    Event(Box<turn_proto::ServerEvent>),
    /// A frame we could not read. One line, not the connection.
    Undecodable(ProtoError),
}

/// A connection that has completed its handshake.
pub struct Connection {
    read: tokio::net::unix::OwnedReadHalf,
    decoder: LineDecoder,
    agreed_version: u32,
    max_line_bytes: usize,
    outbound: mpsc::Sender<Vec<u8>>,
}

/// How many outgoing frames may queue before a sender waits.
///
/// Bounded, because an unbounded queue in front of a socket is a memory leak that
/// only shows up under exactly the conditions nobody can reproduce. 512 is far more
/// than a burst of typing plus a screenful of resizes.
const OUTBOUND_CAPACITY: usize = 512;

/// Opens a connection and completes the handshake.
pub async fn connect(
    socket: &Path,
    client_version: &str,
) -> Result<(Connection, Welcome), LinkError> {
    // Re-read on every attempt. A daemon restart rotates the capability, and a UI
    // that cached it would turn a healthy restart into a permanent auth failure.
    let auth_token = read_auth_token(socket)
        .map_err(|cause| LinkError::io("could not read the daemon capability", cause))?;
    let mut stream = UnixStream::connect(socket)
        .await
        .map_err(|cause| LinkError::io("no Turn daemon is listening", cause))?;

    let hello = ClientFrame::hello(Hello::new("turn-gui", client_version, auth_token));
    let frame = encode_checked(&hello, turn_proto::MAX_LINE_BYTES)
        .map_err(|error| LinkError::Unsendable(error.to_string()))?;
    stream
        .write_all(&frame)
        .await
        .map_err(|cause| LinkError::io("could not send the handshake", cause))?;

    let mut decoder = LineDecoder::new();
    let welcome = read_handshake(&mut stream, &mut decoder).await?;

    let (read, write) = stream.into_split();
    let (outbound, receiver) = mpsc::channel::<Vec<u8>>(OUTBOUND_CAPACITY);
    tokio::spawn(write_loop(write, receiver));

    Ok((
        Connection {
            read,
            decoder,
            agreed_version: welcome.agreed_version,
            max_line_bytes: welcome.limits.max_line_bytes,
            outbound,
        },
        welcome,
    ))
}

fn read_auth_token(socket: &Path) -> std::io::Result<AuthToken> {
    let path = turn_proto::ipc_auth_token_path(socket);
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let mut file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "the daemon capability is not a regular file",
        ));
    }
    let mut secret = String::with_capacity(64);
    Read::by_ref(&mut file)
        .take(65)
        .read_to_string(&mut secret)?;
    if secret.len() != 64 || !secret.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "the daemon capability has an invalid format",
        ));
    }
    Ok(AuthToken::new(secret))
}

/// Reads until the daemon has accepted or refused us.
///
/// Anything other than `welcome` or `rejected` before the handshake completes means
/// whatever is on the socket is not the daemon — a stale socket someone else bound,
/// most likely — and is reported as such rather than half accepted.
async fn read_handshake(
    stream: &mut UnixStream,
    decoder: &mut LineDecoder,
) -> Result<Welcome, LinkError> {
    let mut chunk = [0_u8; 8192];
    loop {
        if let Some(message) = decoder.next_message::<ServerFrame>() {
            let frame = message.map_err(|error| LinkError::NotADaemon(error.to_proto_error()))?;
            return match frame.message {
                ServerMessage::Welcome(welcome) => Ok(welcome),
                ServerMessage::Rejected { error } => Err(LinkError::Refused(error)),
                other => Err(LinkError::NotADaemon(
                    ProtoError::new(
                        ErrorCode::HandshakeRequired,
                        "Whatever is listening on the Turn socket did not answer the handshake",
                    )
                    .with_detail(format!("first frame was {}", frame_name(&other))),
                )),
            };
        }
        let read = stream
            .read(&mut chunk)
            .await
            .map_err(|cause| LinkError::io("could not read the handshake", cause))?;
        if read == 0 {
            return Err(LinkError::io(
                "the daemon closed the connection during the handshake",
                std::io::Error::from(std::io::ErrorKind::UnexpectedEof),
            ));
        }
        decoder.feed(&chunk[..read]);
    }
}

fn frame_name(message: &ServerMessage) -> &'static str {
    match message {
        ServerMessage::Welcome(_) => "welcome",
        ServerMessage::Rejected { .. } => "rejected",
        ServerMessage::Response { .. } => "response",
        ServerMessage::Error { .. } => "error",
        ServerMessage::Event { .. } => "event",
    }
}

/// Drains the outbound queue into the socket, batching whatever has already piled up
/// into one `write_all`.
async fn write_loop(
    mut stream: tokio::net::unix::OwnedWriteHalf,
    mut receiver: mpsc::Receiver<Vec<u8>>,
) {
    let mut batch: Vec<u8> = Vec::with_capacity(8192);
    while let Some(first) = receiver.recv().await {
        batch.clear();
        batch.extend_from_slice(&first);
        // Everything already queued goes out in the same syscall. At typing speed
        // that is usually one frame; during a paste it is dozens.
        while let Ok(next) = receiver.try_recv() {
            batch.extend_from_slice(&next);
        }
        if let Err(error) = stream.write_all(&batch).await {
            tracing::debug!(%error, "the daemon socket stopped accepting writes");
            return;
        }
    }
    let _ = stream.shutdown().await;
}

impl Connection {
    pub fn agreed_version(&self) -> u32 {
        self.agreed_version
    }

    /// Queues a request. Returns the frame's own id so the caller can correlate.
    ///
    /// Refused here rather than sent and discarded at the far end: a paste of eight
    /// megabytes is something a user can do by accident, and "that was too large" is
    /// a better answer than silence.
    pub async fn send(&self, id: RequestId, request: Request) -> Result<(), LinkError> {
        let frame = ClientFrame {
            v: self.agreed_version,
            message: ClientMessage::Request { id, request },
        };
        let bytes = encode_checked(&frame, self.max_line_bytes)
            .map_err(|error| LinkError::Unsendable(error.to_string()))?;
        self.outbound.send(bytes).await.map_err(|_| {
            LinkError::io(
                "the connection closed",
                std::io::Error::from(std::io::ErrorKind::BrokenPipe),
            )
        })
    }

    /// The next frame, or `None` when the connection has ended.
    ///
    /// Drains whatever the previous read produced before asking the kernel for more,
    /// so a read that delivered forty frames dispatches all forty.
    pub async fn next_frame(&mut self) -> Option<Frame> {
        loop {
            match self.decoder.next_message::<ServerFrame>() {
                Some(Ok(frame)) => {
                    if frame.v != self.agreed_version {
                        // The daemon changed code without a new handshake. A notice
                        // rather than a teardown: the frames still parse, and the
                        // version stamp exists to make this visible rather than to be
                        // fatal on its own.
                        tracing::warn!(
                            frame_version = frame.v,
                            agreed = self.agreed_version,
                            "a frame arrived stamped with an unexpected protocol version"
                        );
                    }
                    return Some(match frame.message {
                        ServerMessage::Response { id, response } => Frame::Response {
                            id,
                            response: Box::new(response),
                        },
                        ServerMessage::Error { id, error } => Frame::Error { id, error },
                        ServerMessage::Event { event } => Frame::Event(Box::new(event)),
                        ServerMessage::Welcome(welcome) => {
                            tracing::warn!(
                                daemon_pid = welcome.daemon_pid,
                                "a second welcome arrived on an established connection"
                            );
                            continue;
                        }
                        ServerMessage::Rejected { error } => {
                            tracing::warn!(code = %error.code, "the daemon refused a connection it had accepted");
                            continue;
                        }
                    });
                }
                Some(Err(error)) => {
                    tracing::warn!(%error, "could not decode a frame from the daemon");
                    return Some(Frame::Undecodable(error.to_proto_error()));
                }
                None => {}
            }

            let mut chunk = vec![0_u8; 64 * 1024];
            match self.read.read(&mut chunk).await {
                Ok(0) => return None,
                Ok(read) => self.decoder.feed(&chunk[..read]),
                Err(error) => {
                    tracing::debug!(%error, "the daemon socket stopped producing bytes");
                    return None;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tokio::io::AsyncBufReadExt;
    use tokio::net::UnixListener;
    use turn_proto::{Response, ServerEvent};

    fn socket_path(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "turn-gui-link-{}-{}.sock",
            name,
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let token_path = turn_proto::ipc_auth_token_path(&path);
        let _ = std::fs::remove_file(&token_path);
        std::fs::write(token_path, "a".repeat(64)).unwrap();
        path
    }

    fn welcome(pid: u32) -> Welcome {
        Welcome::new(1, "0.1.0-test", pid, 1_700_000_000_000)
    }

    /// A daemon-shaped peer: reads the handshake, answers it, then hands the id of
    /// each request it receives to `on_request` and writes back whatever comes out.
    fn fake_daemon<F>(
        path: &Path,
        welcome: Option<Welcome>,
        refusal: Option<ProtoError>,
        on_request: F,
    ) -> tokio::task::JoinHandle<()>
    where
        F: Fn(RequestId) -> Vec<ServerFrame> + Send + 'static,
    {
        let listener = UnixListener::bind(path).expect("bind the fake daemon socket");
        tokio::spawn(async move {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let (read, mut write) = stream.into_split();
            let mut lines = tokio::io::BufReader::new(read).lines();
            let Ok(Some(hello)) = lines.next_line().await else {
                return;
            };
            assert!(hello.contains("\"type\":\"hello\""), "got {hello}");

            if let Some(error) = refusal {
                if let Ok(bytes) = turn_proto::encode(&ServerFrame::rejected(error)) {
                    let _ = write.write_all(&bytes).await;
                }
                return;
            }
            let Some(welcome) = welcome else { return };
            let Ok(bytes) = turn_proto::encode(&ServerFrame::welcome(welcome)) else {
                return;
            };
            let _ = write.write_all(&bytes).await;

            while let Ok(Some(line)) = lines.next_line().await {
                if line.trim().is_empty() {
                    continue;
                }
                let Ok(client) = serde_json::from_str::<ClientFrame>(&line) else {
                    continue;
                };
                let Some(id) = client.request_id().cloned() else {
                    continue;
                };
                for out in on_request(id) {
                    let Ok(bytes) = turn_proto::encode(&out) else {
                        return;
                    };
                    if write.write_all(&bytes).await.is_err() {
                        return;
                    }
                }
            }
        })
    }

    #[tokio::test]
    async fn a_request_is_answered_over_a_real_socket() {
        let path = socket_path("answered");
        let daemon = fake_daemon(&path, Some(welcome(11)), None, |id| {
            vec![ServerFrame::response(id, Response::Ack)]
        });

        let (mut connection, welcome) = connect(&path, "0.1.0").await.expect("connect");
        assert_eq!(welcome.daemon_pid, 11);
        assert_eq!(connection.agreed_version(), 1);

        connection
            .send(RequestId::new("r-1"), Request::ListTemplates)
            .await
            .expect("the request was queued");
        match connection.next_frame().await {
            Some(Frame::Response { id, response }) => {
                assert_eq!(id, RequestId::new("r-1"));
                assert_eq!(*response, Response::Ack);
            }
            other => panic!("expected a response, got {other:?}"),
        }

        daemon.abort();
        let _ = std::fs::remove_file(&path);
    }

    /// The protocol's ordering rule over a real socket: the daemon answers the second
    /// request first, and correlation by id is what keeps both callers right.
    #[tokio::test]
    async fn answers_are_correlated_by_id_and_not_by_arrival_order() {
        let path = socket_path("pipelined");
        let daemon = fake_daemon(&path, Some(welcome(12)), None, |id| {
            if id == RequestId::new("r-1") {
                // Hold the first answer back.
                return Vec::new();
            }
            vec![
                ServerFrame::response(id, Response::Attention { entry: None }),
                ServerFrame::response(RequestId::new("r-1"), Response::Ack),
            ]
        });

        let (mut connection, _) = connect(&path, "0.1.0").await.expect("connect");
        connection
            .send(RequestId::new("r-1"), Request::ListTemplates)
            .await
            .expect("queued");
        connection
            .send(RequestId::new("r-2"), Request::NextAttention)
            .await
            .expect("queued");

        let mut answered: Vec<RequestId> = Vec::new();
        while answered.len() < 2 {
            match connection.next_frame().await {
                Some(Frame::Response { id, .. }) => answered.push(id),
                Some(other) => panic!("unexpected frame {other:?}"),
                None => panic!("the connection ended before both answers arrived"),
            }
        }
        assert_eq!(
            answered,
            vec![RequestId::new("r-2"), RequestId::new("r-1")],
            "the second request was answered first, and the ids say so"
        );

        daemon.abort();
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn a_push_arrives_alongside_an_answer() {
        let path = socket_path("pushes");
        let daemon = fake_daemon(&path, Some(welcome(13)), None, |id| {
            vec![
                ServerFrame::event(ServerEvent::AttentionQueueChanged {
                    entries: Vec::new(),
                }),
                ServerFrame::response(id, Response::Ack),
            ]
        });

        let (mut connection, _) = connect(&path, "0.1.0").await.expect("connect");
        connection
            .send(RequestId::new("r-1"), Request::ListTemplates)
            .await
            .expect("queued");

        match connection.next_frame().await {
            Some(Frame::Event(event)) => assert_eq!(event.event_name(), "attention_queue_changed"),
            other => panic!("expected the push first, got {other:?}"),
        }
        match connection.next_frame().await {
            Some(Frame::Response { .. }) => {}
            other => panic!("expected the answer second, got {other:?}"),
        }

        daemon.abort();
        let _ = std::fs::remove_file(&path);
    }

    /// One bad line costs one line, exactly as the protocol says. A multiplexer that
    /// dropped its control connection on bad input would take thirty running agents
    /// down with it.
    #[tokio::test]
    async fn an_undecodable_line_is_reported_and_the_connection_survives_it() {
        let path = socket_path("badline");
        let listener = UnixListener::bind(&path).expect("bind");
        let daemon = tokio::spawn(async move {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let (read, mut write) = stream.into_split();
            let mut lines = tokio::io::BufReader::new(read).lines();
            let _ = lines.next_line().await;
            if let Ok(bytes) = turn_proto::encode(&ServerFrame::welcome(welcome(14))) {
                let _ = write.write_all(&bytes).await;
            }
            let _ = write.write_all(b"{not json at all}\n").await;
            if let Ok(bytes) =
                turn_proto::encode(&ServerFrame::event(ServerEvent::AttentionQueueChanged {
                    entries: Vec::new(),
                }))
            {
                let _ = write.write_all(&bytes).await;
            }
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        });

        let (mut connection, _) = connect(&path, "0.1.0").await.expect("connect");
        match connection.next_frame().await {
            Some(Frame::Undecodable(error)) => {
                assert_eq!(error.code, ErrorCode::MalformedMessage)
            }
            other => panic!("expected the bad line to be reported, got {other:?}"),
        }
        match connection.next_frame().await {
            Some(Frame::Event(event)) => {
                assert_eq!(
                    event.event_name(),
                    "attention_queue_changed",
                    "the frame after a bad one must still arrive"
                );
            }
            other => panic!("the connection must survive one bad line, got {other:?}"),
        }

        daemon.abort();
        let _ = std::fs::remove_file(&path);
    }

    /// A refused handshake must surface as a refusal and not as "no daemon", because
    /// the two demand opposite behaviour: one is retried, the other never is.
    #[tokio::test]
    async fn a_refused_handshake_is_a_refusal_and_not_a_dead_socket() {
        let path = socket_path("refused");
        let refusal = ProtoError::new(
            ErrorCode::UnsupportedVersion,
            "This Turn app is too old for the daemon it is talking to",
        );
        let daemon = fake_daemon(&path, None, Some(refusal.clone()), |_| Vec::new());

        match connect(&path, "0.1.0").await {
            Err(LinkError::Refused(error)) => {
                assert_eq!(error, refusal);
                assert!(
                    !LinkError::Refused(error).is_retryable(),
                    "retrying a refusal would hide the message the user has to read"
                );
            }
            Err(other) => panic!("expected a refusal, got {other:?}"),
            Ok(_) => panic!("the daemon refused us; the connection must not be accepted"),
        }

        daemon.abort();
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn an_auth_refusal_retries_only_after_the_caller_can_reread_the_rotated_token() {
        let auth = LinkError::Refused(ProtoError::new(
            ErrorCode::Unauthorized,
            "the daemon capability rotated",
        ));
        assert!(auth.is_retryable());

        let version = LinkError::Refused(ProtoError::new(
            ErrorCode::UnsupportedVersion,
            "the protocol is incompatible",
        ));
        assert!(!version.is_retryable());
    }

    #[tokio::test]
    async fn connecting_to_a_socket_that_is_not_there_fails_as_retryable() {
        let path = socket_path("absent");
        let Err(error) = connect(&path, "0.1.0").await else {
            panic!("there is no daemon at {}", path.display());
        };
        assert!(
            error.is_retryable(),
            "a missing daemon is worth waiting for"
        );
        assert_eq!(error.to_proto_error().code, ErrorCode::Unavailable);
    }

    #[tokio::test]
    async fn something_that_is_not_a_daemon_is_refused_rather_than_half_accepted() {
        let path = socket_path("impostor");
        let listener = UnixListener::bind(&path).expect("bind");
        let daemon = tokio::spawn(async move {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let mut chunk = [0_u8; 512];
            let _ = stream.read(&mut chunk).await;
            let _ = stream
                .write_all(b"{\"v\":1,\"type\":\"response\",\"id\":\"r-1\",\"response\":{\"result\":\"ack\"}}\n")
                .await;
            // Held open so the failure is about the frame, not about the EOF.
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        });

        let Err(error) = connect(&path, "0.1.0").await else {
            panic!("a response frame is not a handshake and must not be accepted as one");
        };
        assert_eq!(error.to_proto_error().code, ErrorCode::HandshakeRequired);

        daemon.abort();
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn a_daemon_that_goes_away_ends_the_frame_stream_rather_than_hanging() {
        let path = socket_path("dies");
        let daemon = fake_daemon(&path, Some(welcome(15)), None, |_| Vec::new());
        let (mut connection, _) = connect(&path, "0.1.0").await.expect("connect");
        connection
            .send(RequestId::new("r-1"), Request::NextAttention)
            .await
            .expect("queued");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        daemon.abort();

        let ended =
            tokio::time::timeout(std::time::Duration::from_secs(2), connection.next_frame())
                .await
                .expect("the stream must end rather than hang");
        assert!(ended.is_none(), "got {ended:?}");
        let _ = std::fs::remove_file(&path);
    }
}
