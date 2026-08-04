//! Being the only daemon.
//!
//! A unix socket file outlives the process that bound it, so its presence proves
//! nothing. The only reliable question is whether something *answers*, and the only
//! answer that means "a Turn daemon owns this" is a `welcome` or a `rejected` —
//! both of which come out of [`turn_proto`]'s handshake.
//!
//! Getting this wrong is expensive in both directions. Refusing to start because a
//! file exists leaves the user unable to run Turn after a crash, with no way to know
//! why. Deleting the file and binding over a live daemon gives two owners for the
//! same database and the same ptys, and the UI silently reaches whichever it
//! connected to.

use crate::error::{DaemonError, Result};
use crate::paths;
use std::path::Path;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use turn_proto::envelope::{ClientFrame, Hello, ServerFrame, ServerMessage};

/// How long the probe waits for an answer.
///
/// Generous for a loopback round trip against a healthy daemon, short enough that a
/// hung process does not delay start-up noticeably. A daemon that cannot complete a
/// handshake in a second is not one we should hand the user's sessions back to.
const PROBE_TIMEOUT: Duration = Duration::from_millis(1_000);

/// What was found at a socket path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Occupant {
    /// Nothing is there.
    Absent,
    /// A file is there but nothing answers. Safe to remove.
    Stale,
    /// A Turn daemon answered.
    Live { pid: u32, version: String },
    /// Something answered, but not with Turn's handshake. Not ours to remove.
    Foreign,
}

/// Asks what is at a socket path.
pub async fn probe(socket: &Path) -> Occupant {
    if !socket.exists() {
        return Occupant::Absent;
    }
    let Ok(stream) = UnixStream::connect(socket).await else {
        // Refused or unreachable: the file is a leftover from a process that is
        // gone. This is the ordinary case after a crash or a hard reboot.
        return Occupant::Stale;
    };
    match tokio::time::timeout(PROBE_TIMEOUT, handshake(stream)).await {
        Ok(Some(occupant)) => occupant,
        // A connection that accepts and then says nothing is a socket held by
        // something that is not going to talk to us.
        Ok(None) | Err(_) => Occupant::Foreign,
    }
}

/// Completes a handshake far enough to identify the peer.
async fn handshake(stream: UnixStream) -> Option<Occupant> {
    let (read_half, mut write_half) = stream.into_split();
    let hello = ClientFrame::hello(Hello::new("turnd-probe", crate::DAEMON_VERSION));
    let frame = turn_proto::framing::encode(&hello).ok()?;
    write_half.write_all(&frame).await.ok()?;
    write_half.flush().await.ok()?;

    let mut lines = BufReader::new(read_half).lines();
    let line = lines.next_line().await.ok()??;
    let frame: ServerFrame = serde_json::from_str(&line).ok()?;
    match frame.message {
        ServerMessage::Welcome(welcome) => Some(Occupant::Live {
            pid: welcome.daemon_pid,
            version: welcome.daemon_version,
        }),
        // A refusal is still a Turn daemon: it just speaks a different protocol
        // version. Taking its socket would be worse than telling the user.
        ServerMessage::Rejected { .. } => Some(Occupant::Live {
            pid: 0,
            version: "unknown".to_string(),
        }),
        _ => Some(Occupant::Foreign),
    }
}

/// Binds the socket, refusing to displace a live daemon.
///
/// The socket is created with owner-only permissions. Anything that can connect can
/// write to every pty the daemon owns, so a mode that let the rest of the machine in
/// would hand out the user's terminals.
pub async fn bind_exclusive(socket: &Path) -> Result<UnixListener> {
    paths::check_socket_path(socket)?;

    match probe(socket).await {
        Occupant::Live { pid, version } => {
            tracing::info!(pid, %version, socket = %socket.display(), "a daemon is already running");
            return Err(DaemonError::AlreadyRunning {
                socket: socket.to_path_buf(),
                pid,
            });
        }
        Occupant::Foreign => {
            return Err(DaemonError::SocketNotOurs {
                socket: socket.to_path_buf(),
            })
        }
        Occupant::Stale => {
            tracing::info!(socket = %socket.display(), "removing a stale socket");
            std::fs::remove_file(socket).map_err(|cause| DaemonError::StaleSocket {
                socket: socket.to_path_buf(),
                cause,
            })?;
        }
        Occupant::Absent => {}
    }

    if let Some(parent) = socket.parent() {
        if !parent.as_os_str().is_empty() {
            paths::ensure_dir(parent)?;
        }
    }

    let listener = UnixListener::bind(socket).map_err(|cause| DaemonError::Bind {
        socket: socket.to_path_buf(),
        cause,
    })?;
    restrict_permissions(socket);
    Ok(listener)
}

/// Makes the socket reachable only by its owner.
#[cfg(unix)]
fn restrict_permissions(socket: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Err(error) = std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600)) {
        // Worth a loud line: the daemon is usable but the socket is more open than
        // intended, and that is a fact the operator should have.
        tracing::warn!(
            socket = %socket.display(),
            %error,
            "could not restrict the socket to the owner"
        );
    }
}

#[cfg(not(unix))]
fn restrict_permissions(_socket: &Path) {}

/// Removes the socket file on the way out, so the next start does not have to
/// diagnose a leftover.
pub fn remove_socket(socket: &Path) {
    if let Err(error) = std::fs::remove_file(socket) {
        if error.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(socket = %socket.display(), %error, "could not remove the socket");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_path_with_nothing_at_it_is_absent() {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("turnd.sock");
        assert_eq!(probe(&socket).await, Occupant::Absent);
    }

    #[tokio::test]
    async fn a_leftover_socket_file_is_stale_and_gets_cleared_away() {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("turnd.sock");
        // A file that is not a socket at all: connect() fails, so nothing owns it.
        std::fs::write(&socket, b"leftover").unwrap();
        assert_eq!(probe(&socket).await, Occupant::Stale);

        let listener = bind_exclusive(&socket)
            .await
            .expect("the stale file must be cleared");
        assert!(socket.exists());
        drop(listener);
    }

    #[tokio::test]
    async fn a_socket_held_by_something_that_never_answers_is_not_taken_over() {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("turnd.sock");
        // Accepts connections and says nothing — a foreign owner. Removing its
        // socket would break it, so we refuse instead.
        let _held = UnixListener::bind(&socket).unwrap();

        assert_eq!(probe(&socket).await, Occupant::Foreign);
        let error = bind_exclusive(&socket).await.expect_err("must refuse");
        assert!(
            matches!(error, DaemonError::SocketNotOurs { .. }),
            "{error}"
        );
        assert!(error.is_contention());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_socket_is_reachable_only_by_its_owner() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("turnd.sock");
        let _listener = bind_exclusive(&socket).await.unwrap();
        let mode = std::fs::metadata(&socket).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "anything that can connect can write to a pty");
    }
}
