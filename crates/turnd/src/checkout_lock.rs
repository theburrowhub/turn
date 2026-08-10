//! Host-wide authority for writing one checkout.
//!
//! SQLite remains the durable product model, but a database chosen with
//! `TURN_DATA_DIR` cannot arbitrate with another database. This module supplies the
//! missing kernel boundary. Every daemon for the same uid opens the same retained
//! lock inode for a checkout filesystem identity and takes a non-blocking `flock`.
//!
//! The lock descriptor is selectively inherited by processes launched in the owning
//! main-checkout Session. If the daemon disappears, a surviving shell or Agent keeps
//! the lock alive. A replacement daemon can acquire only after those runtimes end,
//! preserving the existing explicit recovery flow without allowing a second writer.

use crate::instance::{try_lock_exclusive, verify_lock_identity};
use serde::{Deserialize, Serialize};
#[cfg(unix)]
use std::fs::DirBuilder;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use turn_core::model::WorkspaceWriteLease;
use turn_proto::WriteLeaseOwnerView;

const LOCK_FORMAT_VERSION: u32 = 1;
const MAX_OWNER_BYTES: u64 = 64 * 1024;

/// Canonical filesystem identity of a checkout directory.
///
/// Device and inode, rather than path text, make symlinks, bind aliases and directory
/// renames converge while keeping independent Git worktrees separate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CheckoutIdentity {
    pub(crate) canonical_path: PathBuf,
    device: u64,
    inode: u64,
}

impl CheckoutIdentity {
    pub(crate) fn resolve(path: &Path) -> Result<Self, CheckoutLockError> {
        let canonical_path =
            std::fs::canonicalize(path).map_err(|cause| CheckoutLockError::CheckoutIdentity {
                path: path.to_path_buf(),
                cause,
            })?;
        let metadata = std::fs::metadata(&canonical_path).map_err(|cause| {
            CheckoutLockError::CheckoutIdentity {
                path: canonical_path.clone(),
                cause,
            }
        })?;
        if !metadata.is_dir() {
            return Err(CheckoutLockError::CheckoutIdentity {
                path: canonical_path,
                cause: std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "the checkout is not a directory",
                ),
            });
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Ok(Self {
                canonical_path,
                device: metadata.dev(),
                inode: metadata.ino(),
            })
        }
        #[cfg(not(unix))]
        {
            let _ = metadata;
            Err(CheckoutLockError::Unsupported)
        }
    }

    fn file_name(&self) -> String {
        format!(
            "v{LOCK_FORMAT_VERSION}-{:016x}-{:016x}.lock",
            self.device, self.inode
        )
    }
}

/// Safe owner metadata exposed when another daemon already holds the lock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CheckoutLockOwner {
    version: u32,
    pub(crate) daemon_pid: u32,
    pub(crate) data_dir: String,
    pub(crate) canonical_checkout: String,
    pub(crate) lease: WorkspaceWriteLease,
    pub(crate) owner: WriteLeaseOwnerView,
}

impl CheckoutLockOwner {
    pub(crate) fn new(
        data_dir: &Path,
        identity: &CheckoutIdentity,
        lease: WorkspaceWriteLease,
        owner: WriteLeaseOwnerView,
    ) -> Self {
        Self {
            version: LOCK_FORMAT_VERSION,
            daemon_pid: std::process::id(),
            data_dir: data_dir.to_string_lossy().into_owned(),
            canonical_checkout: identity.canonical_path.to_string_lossy().into_owned(),
            lease,
            owner,
        }
    }
}

/// The held host-wide lock. Dropping closes only this daemon's descriptor.
///
/// There is deliberately no explicit `LOCK_UN`: inherited descriptors share the
/// same open file description, and unlocking it from the daemon would also unlock a
/// surviving writer. The kernel releases authority when the final inherited copy is
/// closed.
pub(crate) struct CheckoutWriteLock {
    identity: CheckoutIdentity,
    path: PathBuf,
    file: File,
    owner: CheckoutLockOwner,
}

impl std::fmt::Debug for CheckoutWriteLock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CheckoutWriteLock")
            .field("identity", &self.identity)
            .field("path", &self.path)
            .field("lease", &self.owner.lease.id)
            .finish_non_exhaustive()
    }
}

impl CheckoutWriteLock {
    pub(crate) fn acquire(
        checkout: &Path,
        lock_root: &Path,
        build_owner: impl FnOnce(&CheckoutIdentity) -> CheckoutLockOwner,
    ) -> Result<Self, CheckoutLockError> {
        let identity = CheckoutIdentity::resolve(checkout)?;
        let root = ensure_lock_root(lock_root)?;
        let path = root.join(identity.file_name());
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // `CLOEXEC` is part of the authority boundary, not an optimisation: the
            // lock reaches a child only through the one descriptor portable-pty
            // explicitly preserves for a checkout writer.
            options
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        }
        let file = options
            .open(&path)
            .map_err(|cause| CheckoutLockError::LockFile {
                path: path.clone(),
                cause,
            })?;
        let metadata = file
            .metadata()
            .map_err(|cause| CheckoutLockError::LockFile {
                path: path.clone(),
                cause,
            })?;
        if !metadata.is_file() {
            return Err(CheckoutLockError::LockFile {
                path,
                cause: std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "the checkout lock is not a regular file",
                ),
            });
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            if metadata.uid() != unsafe { libc::geteuid() } {
                return Err(CheckoutLockError::LockFile {
                    path,
                    cause: std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "the checkout lock belongs to another uid",
                    ),
                });
            }
            if metadata.permissions().mode() & 0o777 != 0o600 {
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).map_err(
                    |cause| CheckoutLockError::LockFile {
                        path: path.clone(),
                        cause,
                    },
                )?;
            }
        }

        if let Err(cause) = try_lock_exclusive(&file) {
            if cause.kind() == std::io::ErrorKind::WouldBlock {
                let owner = read_owner(&path).ok().map(Box::new);
                return Err(CheckoutLockError::Contended { path, owner });
            }
            return Err(CheckoutLockError::LockFile { path, cause });
        }
        verify_lock_identity(&file, &path).map_err(|cause| CheckoutLockError::LockFile {
            path: path.clone(),
            cause,
        })?;

        // The checkout name may have been replaced while the lock pathname was being
        // opened. Never attach authority for one inode to a different directory.
        let after = CheckoutIdentity::resolve(checkout)?;
        if after != identity {
            return Err(CheckoutLockError::CheckoutChanged {
                before: identity.canonical_path,
                after: after.canonical_path,
            });
        }

        let owner = build_owner(&identity);
        write_owner(&path, &owner)?;
        Ok(Self {
            identity,
            path,
            file,
            owner,
        })
    }

    pub(crate) fn update_owner(
        &mut self,
        owner: CheckoutLockOwner,
    ) -> Result<(), CheckoutLockError> {
        verify_lock_identity(&self.file, &self.path).map_err(|cause| {
            CheckoutLockError::LockFile {
                path: self.path.clone(),
                cause,
            }
        })?;
        write_owner(&self.path, &owner)?;
        self.owner = owner;
        Ok(())
    }

    pub(crate) fn identity(&self) -> &CheckoutIdentity {
        &self.identity
    }

    pub(crate) fn protects(
        &self,
        checkout: &Path,
        lease: &WorkspaceWriteLease,
    ) -> Result<bool, CheckoutLockError> {
        let current = CheckoutIdentity::resolve(checkout)?;
        verify_lock_identity(&self.file, &self.path).map_err(|cause| {
            CheckoutLockError::LockFile {
                path: self.path.clone(),
                cause,
            }
        })?;
        Ok(current == self.identity && self.owner.lease.id == lease.id)
    }

    /// Duplicates this lock descriptor for exactly one process spawn and clears
    /// `CLOEXEC` on that duplicate. The caller keeps the returned guard alive until
    /// `Command::spawn` completes, then drops its parent-side copy.
    pub(crate) fn inherit_for_spawn(&self) -> Result<CheckoutLockInheritance, CheckoutLockError> {
        let file = inheritable_clone(&self.file, &self.path)?;
        Ok(CheckoutLockInheritance { _file: file })
    }
}

/// Parent-side lifetime for a descriptor a just-spawned writer inherits.
pub(crate) struct CheckoutLockInheritance {
    _file: File,
}

impl CheckoutLockInheritance {
    #[cfg(unix)]
    pub(crate) fn raw_fd(&self) -> std::os::fd::RawFd {
        use std::os::fd::AsRawFd;
        self._file.as_raw_fd()
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum CheckoutLockError {
    #[error("could not resolve checkout identity at {path}: {cause}")]
    CheckoutIdentity {
        path: PathBuf,
        #[source]
        cause: std::io::Error,
    },
    #[error("could not prepare host checkout-lock directory {path}: {cause}")]
    LockRoot {
        path: PathBuf,
        #[source]
        cause: std::io::Error,
    },
    #[error("could not use host checkout lock {path}: {cause}")]
    LockFile {
        path: PathBuf,
        #[source]
        cause: std::io::Error,
    },
    #[error("checkout identity changed while acquiring authority ({before} -> {after})")]
    CheckoutChanged { before: PathBuf, after: PathBuf },
    #[error("host checkout lock {path} is already held")]
    Contended {
        path: PathBuf,
        owner: Option<Box<CheckoutLockOwner>>,
    },
    #[cfg(not(unix))]
    #[error("host checkout locks are unavailable on this platform")]
    Unsupported,
}

#[cfg(not(unix))]
fn ensure_lock_root(_root: &Path) -> Result<PathBuf, CheckoutLockError> {
    Err(CheckoutLockError::Unsupported)
}

#[cfg(unix)]
fn ensure_lock_root(root: &Path) -> Result<PathBuf, CheckoutLockError> {
    let root = root.to_path_buf();
    let mut builder = DirBuilder::new();
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700).recursive(true);
    }
    match builder.create(&root) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(cause) => return Err(CheckoutLockError::LockRoot { path: root, cause }),
    }

    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let metadata =
            std::fs::symlink_metadata(&root).map_err(|cause| CheckoutLockError::LockRoot {
                path: root.clone(),
                cause,
            })?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(CheckoutLockError::LockRoot {
                path: root,
                cause: std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "the host checkout-lock path is not a real directory",
                ),
            });
        }
        if metadata.uid() != unsafe { libc::geteuid() } {
            return Err(CheckoutLockError::LockRoot {
                path: root,
                cause: std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "the host checkout-lock directory belongs to another uid",
                ),
            });
        }
        if metadata.permissions().mode() & 0o777 != 0o700 {
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).map_err(
                |cause| CheckoutLockError::LockRoot {
                    path: root.clone(),
                    cause,
                },
            )?;
        }
    }
    std::fs::canonicalize(&root).map_err(|cause| CheckoutLockError::LockRoot { path: root, cause })
}

/// Replaces owner metadata beside the stable lock inode.
///
/// The sidecar is intentionally separate: contenders cannot take a shared lock while
/// the owner holds `LOCK_EX`, and rewriting JSON inside the lock inode would let one
/// catch a truncated heartbeat. Rename gives readers either complete old metadata or
/// complete new metadata without ever replacing the inode that defines authority.
fn write_owner(path: &Path, owner: &CheckoutLockOwner) -> Result<(), CheckoutLockError> {
    let mut encoded = serde_json::to_vec(owner).map_err(|cause| CheckoutLockError::LockFile {
        path: path.to_path_buf(),
        cause: std::io::Error::new(std::io::ErrorKind::InvalidData, cause),
    })?;
    if encoded.len() as u64 > MAX_OWNER_BYTES {
        return Err(CheckoutLockError::LockFile {
            path: path.to_path_buf(),
            cause: std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "checkout lock owner metadata is too large",
            ),
        });
    }
    encoded.push(b'\n');
    let owner_path = owner_path(path);
    let temporary = path.with_extension(format!("owner.{}.tmp", std::process::id()));
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let result = (|| {
        let mut file = options
            .open(&temporary)
            .map_err(|cause| CheckoutLockError::LockFile {
                path: temporary.clone(),
                cause,
            })?;
        file.write_all(&encoded)
            .and_then(|_| file.flush())
            .map_err(|cause| CheckoutLockError::LockFile {
                path: temporary.clone(),
                cause,
            })?;
        std::fs::rename(&temporary, &owner_path).map_err(|cause| CheckoutLockError::LockFile {
            path: owner_path,
            cause,
        })
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

fn read_owner(path: &Path) -> Result<CheckoutLockOwner, CheckoutLockError> {
    let path = owner_path(path);
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options
        .open(&path)
        .map_err(|cause| CheckoutLockError::LockFile {
            path: path.clone(),
            cause,
        })?;
    let mut encoded = Vec::new();
    Read::by_ref(&mut file)
        .take(MAX_OWNER_BYTES + 1)
        .read_to_end(&mut encoded)
        .map_err(|cause| CheckoutLockError::LockFile {
            path: path.clone(),
            cause,
        })?;
    if encoded.len() as u64 > MAX_OWNER_BYTES {
        return Err(CheckoutLockError::LockFile {
            path,
            cause: std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "checkout lock owner metadata is too large",
            ),
        });
    }
    let owner: CheckoutLockOwner =
        serde_json::from_slice(&encoded).map_err(|cause| CheckoutLockError::LockFile {
            path: path.clone(),
            cause: std::io::Error::new(std::io::ErrorKind::InvalidData, cause),
        })?;
    if owner.version != LOCK_FORMAT_VERSION {
        return Err(CheckoutLockError::LockFile {
            path,
            cause: std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("unsupported checkout lock version {}", owner.version),
            ),
        });
    }
    Ok(owner)
}

fn owner_path(lock_path: &Path) -> PathBuf {
    lock_path.with_extension("owner.json")
}

#[cfg(unix)]
fn inheritable_clone(file: &File, path: &Path) -> Result<File, CheckoutLockError> {
    use std::os::fd::{AsRawFd, FromRawFd};

    // Keep inherited authority away from stdin/stdout/stderr even if the daemon was
    // started with one of them closed.
    let fd = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 3) };
    if fd == -1 {
        return Err(CheckoutLockError::LockFile {
            path: path.to_path_buf(),
            cause: std::io::Error::last_os_error(),
        });
    }
    // Keep CLOEXEC set in the multithreaded daemon. portable-pty clears it only in
    // the forked child after closing unrelated descriptors, so a concurrent
    // std::process::Command can never accidentally inherit checkout authority.
    Ok(unsafe { File::from_raw_fd(fd) })
}

#[cfg(not(unix))]
fn inheritable_clone(_file: &File, _path: &Path) -> Result<File, CheckoutLockError> {
    Err(CheckoutLockError::Unsupported)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use turn_core::ids::{CheckoutId, WorkspaceId};
    use turn_core::model::SessionMode;

    fn owner(identity: &CheckoutIdentity, data_dir: &Path) -> CheckoutLockOwner {
        let workspace = WorkspaceId::new();
        let lease = WorkspaceWriteLease::active(
            workspace.clone(),
            turn_core::ids::SessionId::new(),
            CheckoutId::primary_for(&workspace),
            10,
        );
        CheckoutLockOwner::new(
            data_dir,
            identity,
            lease.clone(),
            WriteLeaseOwnerView {
                session_id: lease.session_id,
                session_name: "writer".into(),
                mode: SessionMode::MainCheckout,
                cwd: identity.canonical_path.to_string_lossy().into_owned(),
                branch: None,
                last_activity_ms: 10,
            },
        )
    }

    #[cfg(unix)]
    #[test]
    fn aliases_collide_and_distinct_checkout_inodes_do_not() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let checkout = temp.path().join("checkout");
        let other = temp.path().join("other-worktree");
        let alias = temp.path().join("alias");
        let locks = temp.path().join("locks");
        std::fs::create_dir(&checkout).unwrap();
        std::fs::create_dir(&other).unwrap();
        symlink(&checkout, &alias).unwrap();

        let first =
            CheckoutWriteLock::acquire(&checkout, &locks, |identity| owner(identity, temp.path()))
                .expect("the first writer");
        let conflict =
            CheckoutWriteLock::acquire(&alias, &locks, |identity| owner(identity, temp.path()))
                .expect_err("a symlink alias must reach the same lock");
        let CheckoutLockError::Contended {
            owner: Some(held), ..
        } = conflict
        else {
            panic!("typed owner metadata was lost: {conflict:?}");
        };
        assert_eq!(held.lease.id, first.owner.lease.id);

        CheckoutWriteLock::acquire(&other, &locks, |identity| owner(identity, temp.path()))
            .expect("an independent worktree has independent authority");
    }

    #[cfg(unix)]
    #[test]
    fn an_unrelated_spawn_cannot_inherit_checkout_authority() {
        use std::os::fd::AsRawFd;

        let temp = tempfile::tempdir().unwrap();
        let checkout = temp.path().join("checkout");
        let locks = temp.path().join("locks");
        std::fs::create_dir(&checkout).unwrap();
        let lock =
            CheckoutWriteLock::acquire(&checkout, &locks, |identity| owner(identity, temp.path()))
                .expect("the daemon lock");
        let inherited = lock.inherit_for_spawn().expect("an inheritable duplicate");
        let lock_flags = unsafe { libc::fcntl(lock.file.as_raw_fd(), libc::F_GETFD) };
        assert_ne!(lock_flags, -1, "the daemon descriptor must be valid");
        assert_ne!(
            lock_flags & libc::FD_CLOEXEC,
            0,
            "the daemon descriptor must never reach an unrelated exec"
        );
        let flags = unsafe { libc::fcntl(inherited._file.as_raw_fd(), libc::F_GETFD) };
        assert_ne!(flags, -1, "the inherited descriptor must be valid");
        assert_ne!(
            flags & libc::FD_CLOEXEC,
            0,
            "the parent copy must stay CLOEXEC"
        );
        let mut child = Command::new("/bin/sh")
            .args(["-c", "trap '' HUP; exec sleep 30"])
            .spawn()
            .expect("an unrelated concurrent spawn");
        drop(inherited);
        drop(lock);

        CheckoutWriteLock::acquire(&checkout, &locks, |identity| owner(identity, temp.path()))
            .expect("an unrelated exec must not retain checkout authority");

        child.kill().unwrap();
        child.wait().unwrap();
    }
}
