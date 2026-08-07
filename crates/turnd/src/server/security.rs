//! Security boundaries shared by the accept loop and each client connection.

use crate::error::{DaemonError, Result};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use turn_proto::AuthToken;

/// Hard descriptor/task bound for the control socket. Two UIs fit comfortably;
/// stalled or hostile peers cannot grow the daemon without limit.
pub const MAX_IPC_CONNECTIONS: usize = 32;

/// An idle peer cannot reserve one of the connection slots forever without
/// authenticating.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

/// Per-client token bucket. Normal UI bursts (typing, resizes and pane actions)
/// fit, while sustained request floods are rejected before they reach Core.
pub const REQUEST_BURST: u32 = 256;
pub const REQUESTS_PER_SECOND: u32 = 128;

/// Repeated over-budget frames close the abusive connection, which also bounds
/// the number of error frames the daemon will generate for one flood.
pub const MAX_CONSECUTIVE_RATE_LIMITS: u32 = 16;

/// Bad unauthenticated frames are cheap but not free. Allow enough for a useful
/// diagnostic, then release the connection slot.
pub const MAX_PREAUTH_FRAME_ERRORS: u32 = 4;
pub const MAX_CONSECUTIVE_MALFORMED_FRAMES: u32 = 16;

/// Snapshot suitable for tests and operational diagnostics. It contains counts,
/// never identities or capabilities.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IpcStats {
    pub active_connections: usize,
    pub peak_connections: usize,
    pub rejected_uid: u64,
    pub rejected_capacity: u64,
    pub rejected_auth: u64,
    pub rejected_handshake_timeout: u64,
    pub rate_limited_frames: u64,
}

#[derive(Default)]
pub(super) struct IpcCounters {
    active_connections: AtomicUsize,
    peak_connections: AtomicUsize,
    rejected_uid: AtomicU64,
    rejected_capacity: AtomicU64,
    rejected_auth: AtomicU64,
    rejected_handshake_timeout: AtomicU64,
    rate_limited_frames: AtomicU64,
}

impl IpcCounters {
    pub(super) fn snapshot(&self) -> IpcStats {
        IpcStats {
            active_connections: self.active_connections.load(Ordering::Relaxed),
            peak_connections: self.peak_connections.load(Ordering::Relaxed),
            rejected_uid: self.rejected_uid.load(Ordering::Relaxed),
            rejected_capacity: self.rejected_capacity.load(Ordering::Relaxed),
            rejected_auth: self.rejected_auth.load(Ordering::Relaxed),
            rejected_handshake_timeout: self.rejected_handshake_timeout.load(Ordering::Relaxed),
            rate_limited_frames: self.rate_limited_frames.load(Ordering::Relaxed),
        }
    }

    pub(super) fn connection_opened(self: &std::sync::Arc<Self>) -> ConnectionGuard {
        let active = self.active_connections.fetch_add(1, Ordering::Relaxed) + 1;
        self.peak_connections.fetch_max(active, Ordering::Relaxed);
        ConnectionGuard(std::sync::Arc::clone(self))
    }

    pub(super) fn reject_uid(&self) {
        self.rejected_uid.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn reject_capacity(&self) {
        self.rejected_capacity.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn reject_auth(&self) {
        self.rejected_auth.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn reject_handshake_timeout(&self) {
        self.rejected_handshake_timeout
            .fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn rate_limited(&self) {
        self.rate_limited_frames.fetch_add(1, Ordering::Relaxed);
    }
}

pub(super) struct ConnectionGuard(std::sync::Arc<IpcCounters>);

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.0.active_connections.fetch_sub(1, Ordering::Relaxed);
    }
}

/// The current daemon generation's single IPC capability.
pub(super) struct IpcAuthenticator {
    token: AuthToken,
    path: PathBuf,
    active: AtomicBool,
}

impl std::fmt::Debug for IpcAuthenticator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IpcAuthenticator")
            .field("path", &self.path)
            .field("active", &self.active.load(Ordering::Relaxed))
            .finish()
    }
}

impl IpcAuthenticator {
    /// Generates and atomically publishes a fresh 244-bit capability beside the
    /// bound socket. A restart always rotates it.
    pub(super) fn install(socket: &Path) -> Result<Self> {
        let token = AuthToken::new(format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        ));
        let path = turn_proto::ipc_auth_token_path(socket);
        write_atomically(&path, token.expose_secret().as_bytes()).map_err(|cause| {
            DaemonError::IpcAuthToken {
                path: path.clone(),
                cause,
            }
        })?;
        Ok(Self {
            token,
            path,
            active: AtomicBool::new(true),
        })
    }

    pub(super) fn verify(&self, candidate: Option<&AuthToken>) -> bool {
        self.active.load(Ordering::Acquire)
            && candidate.is_some_and(|candidate| {
                constant_time_eq(
                    self.token.expose_secret().as_bytes(),
                    candidate.expose_secret().as_bytes(),
                )
            })
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    /// Revokes this generation before deleting its file. Content comparison keeps
    /// a late shutdown from deleting a newer generation's replacement.
    pub(super) fn revoke(&self) {
        self.active.store(false, Ordering::Release);
        let Ok(current) = read_bounded(&self.path) else {
            return;
        };
        if constant_time_eq(&current, self.token.expose_secret().as_bytes()) {
            if let Err(error) = std::fs::remove_file(&self.path) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!(path = %self.path.display(), %error, "could not remove IPC token");
                }
            }
        }
    }
}

/// A small deterministic token bucket so unit tests do not sleep.
pub(super) struct RequestLimiter {
    tokens: f64,
    last: Instant,
}

impl RequestLimiter {
    pub(super) fn new(now: Instant) -> Self {
        Self {
            tokens: f64::from(REQUEST_BURST),
            last: now,
        }
    }

    pub(super) fn allow(&mut self, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens =
            (self.tokens + elapsed * f64::from(REQUESTS_PER_SECOND)).min(f64::from(REQUEST_BURST));
        if self.tokens < 1.0 {
            return false;
        }
        self.tokens -= 1.0;
        true
    }
}

fn constant_time_eq(expected: &[u8], candidate: &[u8]) -> bool {
    let mut difference = expected.len() ^ candidate.len();
    for (index, expected_byte) in expected.iter().copied().enumerate() {
        difference |= usize::from(expected_byte ^ candidate.get(index).copied().unwrap_or(0));
    }
    difference == 0
}

fn write_atomically(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut temporary = path.as_os_str().to_os_string();
    temporary.push(format!(
        ".tmp-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let temporary = PathBuf::from(temporary);

    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(contents)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)?;
        sync_directory(parent)
    })();

    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

fn read_bounded(path: &Path) -> std::io::Result<Vec<u8>> {
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
            "IPC token path is not a regular file",
        ));
    }
    let mut contents = Vec::with_capacity(64);
    Read::by_ref(&mut file)
        .take(129)
        .read_to_end(&mut contents)?;
    if contents.len() != 64 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "IPC token must be exactly 64 bytes",
        ));
    }
    Ok(contents)
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> std::io::Result<()> {
    File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn limiter_allows_a_burst_then_refills_at_the_documented_rate() {
        let start = Instant::now();
        let mut limiter = RequestLimiter::new(start);
        for _ in 0..REQUEST_BURST {
            assert!(limiter.allow(start));
        }
        assert!(!limiter.allow(start));
        assert!(limiter.allow(start + Duration::from_secs(1)));
    }

    #[test]
    fn restart_rotation_and_revocation_invalidate_the_old_token() {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("turnd.sock");
        let first = IpcAuthenticator::install(&socket).unwrap();
        let old = AuthToken::new(std::fs::read_to_string(first.path()).unwrap());
        assert!(first.verify(Some(&old)));
        first.revoke();
        assert!(!first.verify(Some(&old)));

        let second = IpcAuthenticator::install(&socket).unwrap();
        let current = AuthToken::new(std::fs::read_to_string(second.path()).unwrap());
        assert_ne!(old, current);
        assert!(
            !second.verify(Some(&old)),
            "a replayed generation is refused"
        );
        assert!(second.verify(Some(&current)));
        assert_eq!(
            std::fs::metadata(second.path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        second.revoke();
    }

    #[test]
    fn token_formatting_never_exposes_the_capability() {
        let token = AuthToken::new("a".repeat(64));
        assert!(!format!("{token:?}").contains(&"a".repeat(64)));
    }

    #[test]
    fn a_precreated_symlink_cannot_redirect_token_publication() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("turnd.sock");
        let path = turn_proto::ipc_auth_token_path(&socket);
        let target = temp.path().join("unrelated-secret");
        std::fs::write(&target, "leave-me-alone").unwrap();
        symlink(&target, &path).unwrap();

        let authenticator = IpcAuthenticator::install(&socket).unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "leave-me-alone");
        assert!(std::fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_file());
        authenticator.revoke();
    }
}
