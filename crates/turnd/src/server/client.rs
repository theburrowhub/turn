//! One connection: a reader, a writer, and the handshake between them.
//!
//! The two halves are separate tasks over a split socket. The reader turns lines into
//! requests and puts the answers on the same queue the daemon's pushes go through, so
//! there is exactly one writer and no interleaved frames. The queue is bounded, which
//! is what keeps a client that has stopped reading from costing the daemon memory: its
//! own requests stall, and nobody else's do.

use super::security::{
    ConnectionGuard, IpcAuthenticator, IpcCounters, RequestLimiter, HANDSHAKE_TIMEOUT,
    MAX_CONSECUTIVE_MALFORMED_FRAMES, MAX_CONSECUTIVE_RATE_LIMITS, MAX_PREAUTH_FRAME_ERRORS,
};
use super::DaemonInfo;
use crate::core::{ClientId, Command, CLIENT_FRAME_CAPACITY};
use std::sync::Arc;
use std::time::Instant;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot, OwnedSemaphorePermit};
use turn_proto::envelope::{ClientMessage, Welcome};
use turn_proto::{ClientFrame, ErrorCode, LineDecoder, ProtoError, ServerFrame};

/// How many bytes are read from the socket at a time.
const READ_CHUNK: usize = 64 * 1024;

/// How many queued frames one write may carry.
///
/// Batching matters for output: a busy pane produces a frame every few milliseconds,
/// and one syscall per frame is a measurable share of the daemon's work for no benefit.
const MAX_FRAMES_PER_WRITE: usize = 64;

/// Resources granted by the accept loop for exactly one client lifetime.
pub(super) struct ClientAdmission {
    authenticator: Arc<IpcAuthenticator>,
    stats: Arc<IpcCounters>,
    _permit: OwnedSemaphorePermit,
    _active: ConnectionGuard,
}

impl ClientAdmission {
    pub(super) fn new(
        authenticator: Arc<IpcAuthenticator>,
        stats: Arc<IpcCounters>,
        permit: OwnedSemaphorePermit,
        active: ConnectionGuard,
    ) -> Self {
        Self {
            authenticator,
            stats,
            _permit: permit,
            _active: active,
        }
    }
}

/// Serves one connection from the handshake to the socket closing.
pub(super) async fn serve(
    stream: UnixStream,
    id: ClientId,
    commands: mpsc::Sender<Command>,
    info: DaemonInfo,
    admission: ClientAdmission,
) {
    let (mut read_half, write_half) = stream.into_split();
    let (frames, outbox) = mpsc::channel::<ServerFrame>(CLIENT_FRAME_CAPACITY);
    let mut writer = tokio::spawn(write_frames(write_half, outbox));

    let mut decoder = LineDecoder::new();
    let mut buffer = vec![0u8; READ_CHUNK];
    let mut agreed: Option<u32> = None;
    let handshake_deadline = tokio::time::Instant::now() + HANDSHAKE_TIMEOUT;
    let mut limiter = RequestLimiter::new(Instant::now());
    let mut consecutive_rate_limits = 0u32;
    let mut consecutive_bad_frames = 0u32;
    let mut preauth_frame_errors = 0u32;

    'connection: loop {
        let read_result = if agreed.is_none() {
            match tokio::time::timeout_at(handshake_deadline, read_half.read(&mut buffer)).await {
                Ok(result) => result,
                Err(_) => {
                    admission.stats.reject_handshake_timeout();
                    tracing::info!(%id, "refused IPC peer that did not complete its handshake");
                    break;
                }
            }
        } else {
            read_half.read(&mut buffer).await
        };
        let read = match read_result {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) => {
                tracing::debug!(%id, %error, "read failed");
                break;
            }
        };
        decoder.feed(&buffer[..read]);

        while let Some(line) = decoder.next_line() {
            let line = match line {
                Ok(line) => line,
                Err(error) => {
                    // One bad line costs one line. The connection stays up: dropping it
                    // would take every pane this UI is rendering with it.
                    tracing::debug!(%id, %error, "bad frame");
                    if send(&frames, ServerFrame::error(None, error.to_proto_error())).await {
                        consecutive_bad_frames += 1;
                        if agreed.is_none() {
                            preauth_frame_errors += 1;
                        }
                        if consecutive_bad_frames >= MAX_CONSECUTIVE_MALFORMED_FRAMES
                            || preauth_frame_errors >= MAX_PREAUTH_FRAME_ERRORS
                        {
                            break 'connection;
                        }
                        continue;
                    }
                    break 'connection;
                }
            };

            let frame = match parse(&line) {
                Ok(frame) => frame,
                Err(error) => {
                    let fatal = error.code.is_fatal_to_connection();
                    let _ = send(&frames, ServerFrame::error(None, error)).await;
                    consecutive_bad_frames += 1;
                    if agreed.is_none() {
                        preauth_frame_errors += 1;
                    }
                    if fatal {
                        break 'connection;
                    }
                    if consecutive_bad_frames >= MAX_CONSECUTIVE_MALFORMED_FRAMES
                        || preauth_frame_errors >= MAX_PREAUTH_FRAME_ERRORS
                    {
                        break 'connection;
                    }
                    continue;
                }
            };
            consecutive_bad_frames = 0;

            if agreed.is_some() && !limiter.allow(Instant::now()) {
                admission.stats.rate_limited();
                consecutive_rate_limits += 1;
                let error = ProtoError::new(
                    ErrorCode::RateLimited,
                    "This client is sending requests too quickly; back off before retrying",
                );
                let request_id = frame.request_id().cloned();
                let _ = send(&frames, ServerFrame::error(request_id, error)).await;
                if consecutive_rate_limits >= MAX_CONSECUTIVE_RATE_LIMITS {
                    tracing::warn!(%id, "closed an IPC client after a sustained request flood");
                    break 'connection;
                }
                continue;
            }
            consecutive_rate_limits = 0;

            match (&agreed, frame.message) {
                // ------------------------------------------------------- handshake
                (None, ClientMessage::Hello(hello)) => {
                    if !admission.authenticator.verify(hello.auth_token.as_ref()) {
                        admission.stats.reject_auth();
                        let error = ProtoError::new(
                            ErrorCode::Unauthorized,
                            "This client did not present the current daemon capability",
                        );
                        tracing::warn!(%id, client = %hello.client, "refused an unauthenticated IPC handshake");
                        let _ = send(&frames, ServerFrame::rejected(error)).await;
                        break 'connection;
                    }
                    match turn_proto::negotiate(frame.v) {
                        Ok(version) => {
                            let welcome =
                                Welcome::new(version, &info.version, info.pid, info.started_ms);
                            if !send(&frames, ServerFrame::welcome(welcome)).await {
                                break 'connection;
                            }
                            // Registered before the first request is served, so a push
                            // caused by that request cannot be missed.
                            let (ready, wait) = oneshot::channel();
                            let opened = Command::ClientOpened {
                                client: id,
                                agreed_version: version,
                                frames: frames.clone(),
                                ready,
                            };
                            if commands.send(opened).await.is_err() {
                                break 'connection;
                            }
                            let _ = wait.await;
                            agreed = Some(version);
                            tracing::info!(
                                %id, client = %hello.client, version = %hello.client_version,
                                protocol = version, "handshake complete"
                            );
                        }
                        Err(error) => {
                            // Refused, and told which side is old. A UI that half works
                            // is worse than one that will not start.
                            tracing::info!(%id, client = %hello.client, %error, "refused a handshake");
                            let _ = send(&frames, ServerFrame::rejected(error)).await;
                            break 'connection;
                        }
                    }
                }
                (Some(_), ClientMessage::Hello(_)) => {
                    let error = ProtoError::new(
                        ErrorCode::AlreadyHandshaked,
                        "This connection has already completed its handshake",
                    );
                    if !send(&frames, ServerFrame::error(None, error)).await {
                        break 'connection;
                    }
                }
                (None, ClientMessage::Request { id: request_id, .. }) => {
                    let error = ProtoError::new(
                        ErrorCode::HandshakeRequired,
                        "Send hello before any request",
                    );
                    let _ = send(&frames, ServerFrame::error(Some(request_id), error)).await;
                    break 'connection;
                }

                // --------------------------------------------------------- requests
                (
                    Some(version),
                    ClientMessage::Request {
                        id: request_id,
                        request,
                    },
                ) => {
                    // Catches a peer that changed code without re-handshaking, which is
                    // the everyday case while developing a UI against a live daemon.
                    if let Err(error) = expect_version(frame.v, *version) {
                        let _ = send(&frames, ServerFrame::error(Some(request_id), error)).await;
                        break 'connection;
                    }
                    let (reply, answer) = oneshot::channel();
                    let command = Command::Request {
                        client: id,
                        id: request_id.clone(),
                        request: Box::new(request),
                        reply,
                    };
                    if commands.send(command).await.is_err() {
                        break 'connection;
                    }
                    let frame = match answer.await {
                        Ok(Ok(response)) => ServerFrame::response(request_id, response),
                        Ok(Err(error)) => ServerFrame::error(Some(request_id), error),
                        Err(_) => {
                            // The core dropped the reply channel, which only happens if
                            // it is shutting down.
                            break 'connection;
                        }
                    };
                    if !send(&frames, frame).await {
                        break 'connection;
                    }
                }
            }
        }
    }

    if agreed.is_some() {
        let _ = commands.send(Command::ClientClosed { client: id }).await;
    }
    drop(frames);
    if tokio::time::timeout(std::time::Duration::from_secs(1), &mut writer)
        .await
        .is_err()
    {
        // A peer that never reads must not retain its descriptor, connection permit
        // and writer task after the reader has decided the connection is over.
        writer.abort();
        let _ = writer.await;
    }
}

/// Parses a line into a frame, reporting a version problem as one.
///
/// A frame this build cannot deserialise might still be a frame from a peer speaking a
/// different protocol version, and `malformed_message` would send whoever is debugging
/// it looking for a typo instead of a mismatch. So the version is read out of the raw
/// JSON before the shape is blamed.
fn parse(line: &[u8]) -> std::result::Result<ClientFrame, ProtoError> {
    match serde_json::from_slice::<ClientFrame>(line) {
        Ok(frame) => Ok(frame),
        Err(shape) => {
            if let Ok(value) = serde_json::from_slice::<serde_json::Value>(line) {
                if let Some(version) = value.get("v").and_then(serde_json::Value::as_u64) {
                    turn_proto::negotiate(version as u32)?;
                }
            }
            Err(ProtoError::new(
                ErrorCode::MalformedMessage,
                "A message could not be understood",
            )
            .with_detail(shape.to_string()))
        }
    }
}

/// Checks a post-handshake frame against the agreed version.
fn expect_version(frame_version: u32, agreed: u32) -> std::result::Result<(), ProtoError> {
    if frame_version == agreed {
        return Ok(());
    }
    Err(ProtoError::new(
        ErrorCode::UnsupportedVersion,
        format!(
            "This connection agreed on protocol {agreed} but a frame arrived marked \
             {frame_version}. Reconnect to negotiate again"
        ),
    ))
}

/// Queues a frame. Returns false when the connection is finished with.
async fn send(frames: &mpsc::Sender<ServerFrame>, frame: ServerFrame) -> bool {
    let terminal = frame.is_terminal();
    if frames.send(frame).await.is_err() {
        return false;
    }
    !terminal
}

/// Writes queued frames, batching whatever is already waiting into one syscall.
async fn write_frames(
    mut write_half: tokio::net::unix::OwnedWriteHalf,
    mut outbox: mpsc::Receiver<ServerFrame>,
) {
    let mut batch = Vec::new();
    while let Some(frame) = outbox.recv().await {
        batch.clear();
        encode(&frame, &mut batch);
        for _ in 1..MAX_FRAMES_PER_WRITE {
            match outbox.try_recv() {
                Ok(frame) => encode(&frame, &mut batch),
                Err(_) => break,
            }
        }
        if batch.is_empty() {
            continue;
        }
        if write_half.write_all(&batch).await.is_err() {
            return;
        }
        if write_half.flush().await.is_err() {
            return;
        }
    }
    let _ = write_half.shutdown().await;
}

/// Appends one frame to the write buffer.
fn encode(frame: &ServerFrame, batch: &mut Vec<u8>) {
    if let Err(error) = turn_proto::encode_into(frame, batch) {
        // Only reachable for a value serde cannot represent, which would be a bug in a
        // payload we built. Losing the frame is better than losing the connection.
        tracing::error!(%error, "could not encode a frame");
    }
}

/// Reports what a frame is, for a log line. Never its contents.
impl std::fmt::Debug for super::DaemonHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DaemonHandle")
            .field("socket", &self.socket_path)
            .field("hooks", &self.hook_base_url)
            .field("pid", &self.info.pid)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_from_another_protocol_version_is_reported_as_a_version_problem() {
        // A request whose payload this build cannot make sense of — a newer daemon
        // added fields, say — carrying a version this build does not serve. Answering
        // `malformed_message` would send whoever is reading the log looking for a typo
        // instead of a mismatch, which is why the version is checked first.
        let line = br#"{"v":99,"type":"request","id":"r-1","request":{"op":"resize_pane"}}"#;
        let error = parse(line).expect_err("must be refused");
        assert_eq!(error.code, ErrorCode::UnsupportedVersion);
        assert!(error.code.is_fatal_to_connection());

        // The same broken shape at a version this build does serve is simply broken.
        let current = format!(
            r#"{{"v":{},"type":"request","id":"r-1","request":{{"op":"resize_pane"}}}}"#,
            turn_proto::PROTOCOL_VERSION
        );
        let error = parse(current.as_bytes()).expect_err("must be refused");
        assert_eq!(error.code, ErrorCode::MalformedMessage);
    }

    /// A frame that parses but is marked with an unsupported version is caught by the
    /// handshake or by `expect_version`, not here: it is a well-formed message in a
    /// dialect this connection did not agree to.
    #[test]
    fn a_well_formed_frame_at_an_unsupported_version_is_left_for_the_version_checks() {
        let line = br#"{"v":99,"type":"request","id":"r-1","request":{"op":"list_templates"}}"#;
        let frame = parse(line).expect("it parses; it is the version that is wrong");
        assert_eq!(frame.v, 99);
        assert!(turn_proto::negotiate(frame.v).is_err());
        assert!(expect_version(frame.v, turn_proto::PROTOCOL_VERSION).is_err());
    }

    #[test]
    fn a_frame_that_is_merely_wrong_is_reported_as_malformed() {
        let line = format!(
            r#"{{"v":{},"type":"request","id":"r-1","request":{{"op":"fly_to_the_moon"}}}}"#,
            turn_proto::PROTOCOL_VERSION
        );
        let error = parse(line.as_bytes()).expect_err("must be refused");
        assert_eq!(error.code, ErrorCode::MalformedMessage);
        assert!(
            !error.code.is_fatal_to_connection(),
            "one bad line costs one line"
        );
    }

    #[test]
    fn a_valid_frame_parses_at_the_current_version() {
        let line = format!(
            r#"{{"v":{},"type":"hello","client":"turn-ui","client_version":"0.1.0"}}"#,
            turn_proto::PROTOCOL_VERSION
        );
        let frame = parse(line.as_bytes()).expect("must parse");
        assert_eq!(frame.v, turn_proto::PROTOCOL_VERSION);
        assert!(matches!(frame.message, ClientMessage::Hello(_)));
    }

    #[test]
    fn a_frame_marked_with_the_wrong_version_after_the_handshake_is_fatal() {
        let error = expect_version(3, 2).expect_err("must be refused");
        assert_eq!(error.code, ErrorCode::UnsupportedVersion);
        assert!(expect_version(2, 2).is_ok());
    }
}
