//! Starting the daemon, and accepting connections.

mod client;
mod security;

use crate::config::Config;
use crate::core::{ClientId, Command, Core, COMMAND_CAPACITY};
use crate::error::Result;
use crate::{instance, paths};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::net::UnixListener;
use tokio::sync::{mpsc, oneshot, Semaphore};
use tokio::task::JoinHandle;
use turn_agents::HookServer;
use turn_store::Store;

use security::{IpcAuthenticator, IpcCounters};
pub use security::{IpcStats, MAX_IPC_CONNECTIONS, REQUESTS_PER_SECOND, REQUEST_BURST};

/// Facts about this daemon that every handshake repeats.
///
/// `pid` and `started_ms` are what let a reconnecting UI tell "my socket hiccupped"
/// from "the daemon restarted and is holding nothing of mine" — which decides whether
/// it has to re-attach every pane.
#[derive(Debug, Clone)]
pub struct DaemonInfo {
    pub version: String,
    pub pid: u32,
    pub started_ms: i64,
}

/// A running daemon.
///
/// Dropping this does not stop anything: the tasks own their work and the processes
/// outlive the handle by design. Call [`DaemonHandle::shutdown`] to stop, or
/// [`DaemonHandle::run_until_signal`] to hand control to the operating system.
pub struct DaemonHandle {
    socket_path: PathBuf,
    hook_base_url: String,
    data_dir: PathBuf,
    info: DaemonInfo,
    commands: mpsc::Sender<Command>,
    authenticator: Arc<IpcAuthenticator>,
    ipc_stats: Arc<IpcCounters>,
    accept: JoinHandle<()>,
    core: JoinHandle<()>,
    /// Shared with the core task. Keeping it here makes ownership visible in the
    /// daemon's RAII boundary; keeping it in the core means dropping this handle
    /// cannot unlock a still-running detached daemon.
    _data_dir_lock: Arc<instance::DataDirLock>,
    /// Kept alive for as long as the daemon runs: dropping it stops the hook server,
    /// and an agent whose callbacks start failing is a worse outcome than an idle port.
    hooks: Arc<HookServer>,
}

impl DaemonHandle {
    /// The socket clients connect to.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// The loopback base URL agents post their hooks to.
    pub fn hook_base_url(&self) -> &str {
        &self.hook_base_url
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn info(&self) -> &DaemonInfo {
        &self.info
    }

    /// Counters from the hook server, including how many posts were refused for an
    /// unknown token.
    pub fn hook_stats(&self) -> turn_agents::HookStats {
        self.hooks.stats()
    }

    /// Owner-only capability file a local client must read for its handshake.
    pub fn ipc_auth_token_path(&self) -> &Path {
        self.authenticator.path()
    }

    /// Aggregate IPC rejection/load counters. Secrets and request payloads are
    /// deliberately absent.
    pub fn ipc_stats(&self) -> IpcStats {
        self.ipc_stats.snapshot()
    }

    /// Flushes state, stops the tasks and removes the socket.
    pub async fn shutdown(self) {
        // Close the admission boundary before waiting for persistence. No new
        // connection may authenticate into a daemon that is already shutting down.
        self.authenticator.revoke();
        self.accept.abort();
        let _ = self.accept.await;
        let (done, wait) = oneshot::channel();
        if self.commands.send(Command::Shutdown { done }).await.is_ok() {
            // The core flushes before answering. This timeout only bounds the graceful
            // phase; a timed-out task is aborted and joined below so the method never
            // returns while it still owns the data-directory lock.
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), wait).await;
        }
        let mut core = self.core;
        if tokio::time::timeout(std::time::Duration::from_secs(5), &mut core)
            .await
            .is_err()
        {
            tracing::error!("core did not stop after five seconds; aborting it");
            core.abort();
            let _ = (&mut core).await;
        }
        // A completed Tokio task retains its future allocation until the JoinHandle is
        // dropped. That allocation owned Core's Arc<DataDirLock>, so merely awaiting
        // it still left an immediate restart racing the old lock under parallel load.
        drop(core);
        instance::remove_socket(&self.socket_path);
        self.hooks.shutdown();
        // `shutdown(self)` is itself a Future. Under load its storage can remain alive
        // for one more poll after Ready, so relying on the implicit end-of-scope drop
        // lets an immediate restart race this final Arc. Release ownership explicitly
        // before returning to the caller.
        drop(self.hooks);
        drop(self._data_dir_lock);
    }

    /// Runs until the operating system asks the daemon to stop.
    ///
    /// Both signals are handled the same way: flush and go. A daemon that ignored
    /// `SIGTERM` would be killed a moment later with its last few minutes of state
    /// unwritten.
    pub async fn run_until_signal(self) {
        wait_for_signal().await;
        tracing::info!("stopping on a signal");
        self.shutdown().await;
    }
}

/// Starts a daemon: store, hook server, socket, core task, accept loop.
pub async fn start(config: Config) -> Result<DaemonHandle> {
    paths::ensure_dir(&config.data_dir)?;

    // This is the first operation after making the directory exist. The socket is
    // only a transport address and may be overridden, so it cannot protect the
    // SQLite store or prevent a second Core::restore from fencing live leases.
    let data_dir_lock = Arc::new(instance::DataDirLock::acquire(&config.data_dir)?);
    let data_dir = data_dir_lock.data_dir().to_path_buf();

    let store = if config.persist {
        Store::open_in(&data_dir)?
    } else {
        // Everything else behaves identically; nothing survives the process.
        Store::open_in_memory()?
    };

    let (hooks, hook_events) = HookServer::start_with_helper(config.hook_helper.clone()).await?;
    let hooks = Arc::new(hooks);
    let hook_base_url = hooks.base_url().to_string();

    // The data-directory guard above protects persistent ownership. This independent
    // boundary protects the transport path from a daemon using another store.
    let listener = instance::bind_exclusive(&config.socket_path).await?;

    let (commands, inbox) = mpsc::channel(COMMAND_CAPACITY);
    let core = match Core::new(
        Arc::clone(&data_dir_lock),
        store,
        Arc::clone(&hooks),
        config.registry,
        data_dir.clone(),
        commands.clone(),
    ) {
        Ok(core) => core,
        Err(error) => {
            instance::remove_socket(&config.socket_path);
            hooks.shutdown();
            return Err(error);
        }
    };

    let authenticator = match IpcAuthenticator::install(&config.socket_path) {
        Ok(authenticator) => Arc::new(authenticator),
        Err(error) => {
            instance::remove_socket(&config.socket_path);
            hooks.shutdown();
            return Err(error);
        }
    };
    let ipc_stats = Arc::new(IpcCounters::default());

    let info = DaemonInfo {
        version: crate::DAEMON_VERSION.to_string(),
        pid: std::process::id(),
        started_ms: turn_core::now_ms(),
    };

    let core_task = tokio::spawn(core.run(inbox, hook_events));
    let accept_task = tokio::spawn(accept_loop(
        listener,
        commands.clone(),
        info.clone(),
        Arc::clone(&authenticator),
        Arc::clone(&ipc_stats),
    ));

    tracing::info!(
        socket = %config.socket_path.display(),
        data_dir = %data_dir.display(),
        hooks = %hook_base_url,
        pid = info.pid,
        version = %info.version,
        persist = config.persist,
        "turnd is listening"
    );

    Ok(DaemonHandle {
        socket_path: config.socket_path,
        hook_base_url,
        data_dir,
        info,
        commands,
        authenticator,
        ipc_stats,
        accept: accept_task,
        core: core_task,
        _data_dir_lock: data_dir_lock,
        hooks,
    })
}

/// Accepts connections until the task is aborted.
async fn accept_loop(
    listener: UnixListener,
    commands: mpsc::Sender<Command>,
    info: DaemonInfo,
    authenticator: Arc<IpcAuthenticator>,
    stats: Arc<IpcCounters>,
) {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let expected_uid = effective_uid();
    let capacity = Arc::new(Semaphore::new(MAX_IPC_CONNECTIONS));
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let peer_uid = match stream.peer_cred() {
                    Ok(credentials) => credentials.uid(),
                    Err(error) => {
                        stats.reject_uid();
                        tracing::warn!(%error, "refused IPC peer whose credentials could not be read");
                        continue;
                    }
                };
                if !peer_is_authorized(peer_uid, expected_uid) {
                    stats.reject_uid();
                    tracing::warn!(
                        peer_uid,
                        expected_uid,
                        "refused IPC peer with a different UID"
                    );
                    continue;
                }
                let permit = match Arc::clone(&capacity).try_acquire_owned() {
                    Ok(permit) => permit,
                    Err(_) => {
                        stats.reject_capacity();
                        tracing::warn!(
                            limit = MAX_IPC_CONNECTIONS,
                            "refused IPC connection at capacity"
                        );
                        continue;
                    }
                };
                let id = ClientId(NEXT.fetch_add(1, Ordering::Relaxed));
                let active = stats.connection_opened();
                let admission = client::ClientAdmission::new(
                    Arc::clone(&authenticator),
                    Arc::clone(&stats),
                    permit,
                    active,
                );
                tokio::spawn(client::serve(
                    stream,
                    id,
                    commands.clone(),
                    info.clone(),
                    admission,
                ));
            }
            Err(error) => {
                // Per-connection failures are normal — a client that vanished between
                // the kernel queueing it and us accepting it. Refusing to serve anyone
                // else because of one of those would be the wrong response.
                tracing::warn!(%error, "could not accept a connection");
                if error.kind() == std::io::ErrorKind::InvalidInput {
                    // The listener itself is unusable; nothing will improve by looping.
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        }
    }
}

#[cfg(unix)]
fn effective_uid() -> u32 {
    // SAFETY: `geteuid` takes no pointers and has no failure mode.
    unsafe { libc::geteuid() }
}

fn peer_is_authorized(peer_uid: u32, expected_uid: u32) -> bool {
    peer_uid == expected_uid
}

/// Waits for `SIGINT` or `SIGTERM`.
#[cfg(unix)]
async fn wait_for_signal() {
    use tokio::signal::unix::{signal, SignalKind};
    let mut terminate = match signal(SignalKind::terminate()) {
        Ok(stream) => stream,
        Err(error) => {
            tracing::error!(%error, "could not listen for SIGTERM; only SIGINT will stop the daemon");
            let _ = tokio::signal::ctrl_c().await;
            return;
        }
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = terminate.recv() => {}
    }
}

#[cfg(not(unix))]
async fn wait_for_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn peer_credentials_are_read_from_the_socket_and_a_wrong_uid_is_refused() {
        let (_client, server) = tokio::net::UnixStream::pair().unwrap();
        let actual = server.peer_cred().unwrap().uid();
        assert!(peer_is_authorized(actual, effective_uid()));
        assert!(
            !peer_is_authorized(actual, actual.wrapping_add(1)),
            "the credential gate must fail closed for a different expected owner"
        );
    }
}
