//! The daemon's start-up and shutdown failures.
//!
//! Everything here is something the user can act on, so each variant carries the
//! path or pid that makes it actionable. Failures *during* a session are not here:
//! those are answered to the client as a [`turn_proto::ProtoError`] and the daemon
//! keeps running.

use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    /// Another process owns the persistent state, independently of which socket it
    /// chose. The socket alone is not a store-ownership boundary: two daemons can
    /// be configured with different socket paths while pointing at the same SQLite
    /// database.
    #[error(
        "a Turn daemon is already running for data directory {data_dir} \
         (owner pid {owner_pid:?}); stop it before starting another"
    )]
    DataDirInUse {
        data_dir: PathBuf,
        owner_pid: Option<u32>,
    },

    /// Failing to establish the store-ownership lock is fatal. Continuing would
    /// allow restore to fence another daemon's live leases and split PTY ownership.
    #[error("could not lock Turn data directory {data_dir}: {cause}")]
    DataDirLock {
        data_dir: PathBuf,
        #[source]
        cause: std::io::Error,
    },

    /// Another daemon answered on the socket. Starting a second one would mean two
    /// owners for the same store and the same ptys, so this is fatal rather than
    /// something to work around.
    #[error(
        "a Turn daemon is already running on {socket} (pid {pid}); \
         stop it before starting another"
    )]
    AlreadyRunning { socket: PathBuf, pid: u32 },

    /// A daemon-shaped peer rejected the probe before revealing its pid. It still
    /// owns the socket and cannot safely be displaced.
    #[error(
        "a Turn daemon is already running on {socket} (pid unavailable); \
         stop it before starting another"
    )]
    AlreadyRunningUnidentified { socket: PathBuf },

    /// Something is listening but it is not a Turn daemon, so the path cannot be
    /// taken over: removing it might break whatever owns it.
    #[error("{socket} is in use by something that is not a Turn daemon")]
    SocketNotOurs { socket: PathBuf },

    #[error("could not remove the stale socket {socket}: {cause}")]
    StaleSocket {
        socket: PathBuf,
        #[source]
        cause: std::io::Error,
    },

    #[error("could not listen on {socket}: {cause}")]
    Bind {
        socket: PathBuf,
        #[source]
        cause: std::io::Error,
    },

    /// The per-generation IPC capability must exist before the socket is exposed
    /// to clients. Continuing without it would turn an authentication failure into
    /// an unauthenticated daemon.
    #[error("could not install IPC authentication token at {path}: {cause}")]
    IpcAuthToken {
        path: PathBuf,
        #[source]
        cause: std::io::Error,
    },

    /// Unix socket paths are limited by the kernel (104 bytes on macOS, 108 on
    /// Linux) and the failure mode when they are too long is an opaque
    /// `EINVAL`. Caught here with the escape hatch named.
    #[error(
        "the socket path {socket} is {length} bytes, over the {limit} the kernel \
         allows; set TURN_SOCKET to something shorter"
    )]
    SocketPathTooLong {
        socket: PathBuf,
        length: usize,
        limit: usize,
    },

    #[error("could not create {path}: {cause}")]
    Directory {
        path: PathBuf,
        #[source]
        cause: std::io::Error,
    },

    #[error("no platform data directory could be resolved; set TURN_DATA_DIR")]
    NoDataDir,

    #[error(transparent)]
    Store(#[from] turn_store::StoreError),

    #[error(transparent)]
    HookServer(#[from] turn_agents::ServerError),

    #[error("{message}")]
    Usage { message: String },
}

impl DaemonError {
    pub(crate) fn directory(path: impl AsRef<Path>, cause: std::io::Error) -> Self {
        DaemonError::Directory {
            path: path.as_ref().to_path_buf(),
            cause,
        }
    }

    pub(crate) fn usage(message: impl Into<String>) -> Self {
        DaemonError::Usage {
            message: message.into(),
        }
    }

    /// Whether the operator should be told to look at a running daemon rather than
    /// at their own configuration.
    pub fn is_contention(&self) -> bool {
        matches!(
            self,
            DaemonError::DataDirInUse { .. }
                | DaemonError::AlreadyRunning { .. }
                | DaemonError::AlreadyRunningUnidentified { .. }
                | DaemonError::SocketNotOurs { .. }
        )
    }
}

pub type Result<T> = std::result::Result<T, DaemonError>;
