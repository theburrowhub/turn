//! Being the only daemon.
//!
//! There are two independent ownership boundaries:
//!
//! * a process lock on the canonical data directory, which is the authority for the
//!   store, leases and PTYs even when daemons choose different sockets; and
//! * the unix socket itself, which must not be displaced when another listener owns it.
//!
//! A unix socket file outlives the process that bound it, so its presence proves
//! nothing. The only reliable question at that boundary is whether something
//! *answers*, and the only answer that means "a Turn daemon owns this" is a `welcome`
//! or a `rejected` — both of which come out of [`turn_proto`]'s handshake.
//!
//! Getting this wrong is expensive in both directions. Refusing to start because a
//! file exists leaves the user unable to run Turn after a crash, with no way to know
//! why. Deleting the file and binding over a live daemon gives two owners for the
//! same database and the same ptys, and the UI silently reaches whichever it
//! connected to.

use crate::error::{DaemonError, Result};
use crate::paths;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
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

/// Stable inode used as the process-level ownership boundary for one data directory.
///
/// The file is deliberately retained after clean shutdown. Removing a lock file can
/// split the lock domain: a contender may already hold the old inode while a third
/// process creates and locks a new one at the same pathname. An unlocked leftover is
/// harmless because the kernel releases `flock` when the owning process exits.
const DATA_DIR_LOCK_FILE: &str = ".turnd.lock";

/// Exclusive ownership of a canonical Turn data directory.
///
/// The open file is the guard. Closing it, including after a crash, releases the
/// kernel lock. The canonical directory is retained so every store and scratch path
/// in the daemon is derived from the same filesystem identity rather than the user's
/// possibly aliased spelling.
pub struct DataDirLock {
    canonical_data_dir: PathBuf,
    _file: File,
}

impl std::fmt::Debug for DataDirLock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DataDirLock")
            .field("canonical_data_dir", &self.canonical_data_dir)
            .finish_non_exhaustive()
    }
}

impl DataDirLock {
    /// Acquires the single-writer guard before SQLite is opened or migrations and
    /// restore can mutate durable state.
    pub fn acquire(data_dir: &Path) -> Result<Self> {
        let canonical_data_dir =
            std::fs::canonicalize(data_dir).map_err(|cause| DaemonError::DataDirLock {
                data_dir: data_dir.to_path_buf(),
                cause,
            })?;
        let lock_path = canonical_data_dir.join(DATA_DIR_LOCK_FILE);
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // A pre-created symlink must not redirect the ownership boundary to a
            // different inode. `CLOEXEC` also prevents launched agents inheriting a
            // descriptor that would keep the daemon lock alive after a crash.
            options
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        }
        let mut file = options
            .open(&lock_path)
            .map_err(|cause| DaemonError::DataDirLock {
                data_dir: canonical_data_dir.clone(),
                cause,
            })?;
        if !file
            .metadata()
            .map_err(|cause| DaemonError::DataDirLock {
                data_dir: canonical_data_dir.clone(),
                cause,
            })?
            .is_file()
        {
            return Err(DaemonError::DataDirLock {
                data_dir: canonical_data_dir,
                cause: std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "the daemon lock is not a regular file",
                ),
            });
        }

        if let Err(cause) = try_lock_exclusive(&file) {
            if cause.kind() == std::io::ErrorKind::WouldBlock {
                return Err(DaemonError::DataDirInUse {
                    data_dir: canonical_data_dir,
                    owner_pid: read_owner_pid(&mut file),
                });
            }
            return Err(DaemonError::DataDirLock {
                data_dir: canonical_data_dir,
                cause,
            });
        }

        verify_lock_identity(&file, &lock_path).map_err(|cause| DaemonError::DataDirLock {
            data_dir: canonical_data_dir.clone(),
            cause,
        })?;
        write_owner_pid(&mut file).map_err(|cause| DaemonError::DataDirLock {
            data_dir: canonical_data_dir.clone(),
            cause,
        })?;

        Ok(Self {
            canonical_data_dir,
            _file: file,
        })
    }

    pub fn data_dir(&self) -> &Path {
        &self.canonical_data_dir
    }
}

#[cfg(unix)]
impl Drop for DataDirLock {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd;
        // `flock` follows the open file description across fork. Agent processes are
        // launched with CLOEXEC, but an immediate restart can otherwise race a child
        // between fork and exec even after the daemon dropped its own descriptor.
        // Explicitly unlocking at the ownership boundary releases that shared lock
        // synchronously; the file itself remains as the stable inode for contenders.
        let result = unsafe { libc::flock(self._file.as_raw_fd(), libc::LOCK_UN) };
        if result != 0 {
            tracing::warn!(
                error = %std::io::Error::last_os_error(),
                data_dir = %self.canonical_data_dir.display(),
                "could not explicitly release the data-directory lock"
            );
        }
    }
}

/// `flock` is tied to the open file description, does not wait, and is released by
/// the kernel on process death. Any unsupported-filesystem error is propagated: a
/// best-effort ownership guard is not an ownership guard.
#[cfg(unix)]
fn try_lock_exclusive(file: &File) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    loop {
        // SAFETY: `file` owns a valid descriptor for the duration of the call. The
        // operation neither reads nor writes Rust memory.
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

#[cfg(not(unix))]
fn try_lock_exclusive(_file: &File) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "data-directory process locking is unavailable on this platform",
    ))
}

/// Ensures a concurrent unlink/recreate during acquisition cannot make us lock an
/// inode other contenders will no longer open. This is an acquisition check; the
/// lock file must never be removed during daemon operation or clean shutdown.
#[cfg(unix)]
fn verify_lock_identity(file: &File, path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    let opened = file.metadata()?;
    let named = std::fs::symlink_metadata(path)?;
    if opened.dev() != named.dev() || opened.ino() != named.ino() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "the daemon lock pathname changed during acquisition",
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn verify_lock_identity(_file: &File, _path: &Path) -> std::io::Result<()> {
    Ok(())
}

fn write_owner_pid(file: &mut File) -> std::io::Result<()> {
    file.set_len(0)?;
    file.seek(SeekFrom::Start(0))?;
    writeln!(file, "{}", std::process::id())?;
    file.flush()
}

fn read_owner_pid(file: &mut File) -> Option<u32> {
    let mut owner = String::new();
    file.seek(SeekFrom::Start(0)).ok()?;
    // Owner metadata is one decimal PID. Never trust an already-present lock file
    // enough to allocate for its entire contents merely to improve an error message.
    Read::by_ref(file)
        .take(64)
        .read_to_string(&mut owner)
        .ok()?;
    owner.trim().parse().ok()
}

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
    match tokio::time::timeout(PROBE_TIMEOUT, handshake(stream, socket)).await {
        Ok(Some(occupant)) => occupant,
        // A connection that accepts and then says nothing is a socket held by
        // something that is not going to talk to us.
        Ok(None) | Err(_) => Occupant::Foreign,
    }
}

/// Completes a handshake far enough to identify the peer.
async fn handshake(stream: UnixStream, socket: &Path) -> Option<Occupant> {
    let (read_half, mut write_half) = stream.into_split();
    let hello = match probe_auth_token(socket) {
        Some(token) => Hello::new("turnd-probe", crate::DAEMON_VERSION, token),
        None => Hello::unauthenticated("turnd-probe", crate::DAEMON_VERSION),
    };
    let hello = ClientFrame::hello(hello);
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

/// Reads just enough of the sidecar to let a probe obtain a current daemon's PID.
/// Absence or an unsafe/malformed file falls back to an unauthenticated probe, which
/// can still identify a `rejected` response without gaining any daemon authority.
fn probe_auth_token(socket: &Path) -> Option<turn_proto::AuthToken> {
    let path = turn_proto::ipc_auth_token_path(socket);
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let mut file = options.open(path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut secret = String::with_capacity(64);
    Read::by_ref(&mut file)
        .take(65)
        .read_to_string(&mut secret)
        .ok()?;
    (secret.len() == 64 && secret.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| turn_proto::AuthToken::new(secret))
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

    #[cfg(unix)]
    #[test]
    fn the_data_directory_lock_follows_filesystem_identity_and_recovers_after_exit() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("state");
        std::fs::create_dir(&data_dir).unwrap();
        let alias = temp.path().join("state-alias");
        symlink(&data_dir, &alias).unwrap();

        let first = DataDirLock::acquire(&data_dir).expect("the first owner");
        let error = DataDirLock::acquire(&alias).expect_err("an alias must not split ownership");
        assert!(
            matches!(
                error,
                DaemonError::DataDirInUse {
                    owner_pid: Some(pid),
                    ..
                } if pid == std::process::id()
            ),
            "{error}"
        );

        // The file remains, but the kernel lock follows the process/open file rather
        // than stale text. Dropping the simulated process owner makes restart safe.
        drop(first);
        let restarted = DataDirLock::acquire(&alias).expect("a crashed owner releases the lock");
        assert_eq!(
            restarted.data_dir(),
            std::fs::canonicalize(&data_dir).unwrap()
        );
        assert!(data_dir.join(DATA_DIR_LOCK_FILE).is_file());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_cannot_redirect_the_data_directory_lock() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("not-the-lock");
        std::fs::write(&target, b"").unwrap();
        symlink(&target, temp.path().join(DATA_DIR_LOCK_FILE)).unwrap();

        let error = DataDirLock::acquire(temp.path()).expect_err("O_NOFOLLOW must fail closed");
        assert!(matches!(error, DaemonError::DataDirLock { .. }), "{error}");
    }

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
