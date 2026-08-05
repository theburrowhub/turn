//! Starting the daemon companion when the desktop app is the product entry point.
//!
//! The daemon remains a separate process: closing the window must not close its PTYs.
//! A packaged app ships `turnd` beside `turn`; a source checkout may fall back to the
//! exact Cargo workspace this build came from. Nothing is looked up by a user-controlled
//! command string and no shell is involved.

use std::ffi::{OsStr, OsString};
use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{mpsc, Arc};

/// Explicit companion override for development and unusual package layouts.
pub const DAEMON_BIN_ENV: &str = "TURN_TURND_BIN";
const DAEMON_LOG_FILE: &str = "turnd.log";

/// Where the executable which was started came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompanionSource {
    Configured,
    PackagedSibling,
    CargoWorkspace,
}

impl std::fmt::Display for CompanionSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Configured => "TURN_TURND_BIN",
            Self::PackagedSibling => "packaged sibling",
            Self::CargoWorkspace => "development Cargo workspace",
        })
    }
}

/// A companion which this invocation launched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompanionStarted {
    /// PID of the exact program launched. In the source fallback this is Cargo,
    /// which replaces itself with `turnd`; the daemon handshake remains authoritative.
    pub launcher_pid: u32,
    pub source: CompanionSource,
    pub program: PathBuf,
    pub log_path: PathBuf,
}

/// A child handle retained by the UI only so its eventual failure can be reported and
/// reaped. Dropping this value never terminates the child.
pub struct CompanionMonitor {
    child: Child,
    program: PathBuf,
    log_path: PathBuf,
}

/// A launcher result delivered after the window has opened. Contention is provisional:
/// a protocol connection proves another launcher won safely and clears it. Any other
/// exit is an actual companion failure even if some unrelated socket accepts connects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompanionEvent {
    Contended(String),
    Failed(String),
}

impl std::fmt::Debug for CompanionMonitor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompanionMonitor")
            .field("pid", &self.child.id())
            .field("program", &self.program)
            .field("log_path", &self.log_path)
            .finish()
    }
}

impl CompanionMonitor {
    /// Waits off the UI thread and wakes the window if the process actually fails.
    /// Contention is not a failure: another UI may have won the same startup race.
    pub fn watch(mut self, wake: Arc<dyn Fn() + Send + Sync>) -> mpsc::Receiver<CompanionEvent> {
        let (send, receive) = mpsc::channel();
        let thread_send = send.clone();
        let thread_wake = Arc::clone(&wake);
        let fallback_log = self.log_path.clone();
        let result = std::thread::Builder::new()
            .name("turn-daemon-companion".to_string())
            .spawn(move || {
                let event = match self.child.wait() {
                    Ok(status) if status.code() == Some(3) => {
                        tracing::debug!(
                            %status,
                            program = %self.program.display(),
                            "the companion lost a safe startup race"
                        );
                        CompanionEvent::Contended(format!(
                            "Another Turn daemon owns this state; waiting for it to become available. See {}",
                            self.log_path.display()
                        ))
                    }
                    Ok(status) => CompanionEvent::Failed(format!(
                        "The Turn daemon exited with {status}; see {}",
                        self.log_path.display()
                    )),
                    Err(error) => CompanionEvent::Failed(format!(
                        "Could not monitor the Turn daemon {}: {error}; see {}",
                        self.program.display(),
                        self.log_path.display()
                    )),
                };
                let _ = thread_send.send(event);
                thread_wake();
            });
        if let Err(error) = result {
            let _ = send.send(CompanionEvent::Failed(format!(
                "Could not monitor the Turn daemon: {error}; see {}",
                fallback_log.display()
            )));
            wake();
        }
        receive
    }
}

/// Started process plus the handle used to surface an asynchronous exit.
#[derive(Debug)]
pub struct CompanionLaunch {
    pub started: CompanionStarted,
    pub monitor: CompanionMonitor,
}

/// The result of ensuring that there is something to connect to.
#[derive(Debug)]
pub enum EnsureOutcome {
    /// A listener already owns the socket. The existing process is left entirely alone;
    /// the protocol handshake will decide whether it is a compatible Turn daemon.
    EndpointOccupied,
    Started(CompanionLaunch),
}

/// An actionable failure shown in the window as well as written to the application log.
#[derive(Debug, thiserror::Error)]
pub enum CompanionError {
    #[error("could not locate the running Turn executable: {0}")]
    CurrentExecutable(#[source] io::Error),
    #[error("TURN_TURND_BIN must name a file, but {path} does not")]
    ConfiguredBinaryMissing { path: PathBuf },
    #[error(
        "could not find the turnd companion beside Turn at {sibling}; package turnd with the app or set TURN_TURND_BIN"
    )]
    CompanionMissing { sibling: PathBuf },
    #[error("refusing to replace non-socket entry at {socket}")]
    UnsafeSocketEntry { socket: PathBuf },
    #[error("could not inspect daemon socket {socket}: {cause}")]
    SocketProbe {
        socket: PathBuf,
        #[source]
        cause: io::Error,
    },
    #[error("could not create daemon log directory {path}: {cause}")]
    LogDirectory {
        path: PathBuf,
        #[source]
        cause: io::Error,
    },
    #[error("could not open daemon log {path}: {cause}")]
    LogFile {
        path: PathBuf,
        #[source]
        cause: io::Error,
    },
    #[error("could not start {program}; daemon log: {log_path}: {cause}")]
    Spawn {
        program: PathBuf,
        log_path: PathBuf,
        #[source]
        cause: io::Error,
    },
    #[error("could not build the development daemon companions with {program}; status {status}; see {log_path}")]
    DevelopmentBuild {
        program: PathBuf,
        status: ExitStatus,
        log_path: PathBuf,
    },
    #[error("the resolved {kind} path must be absolute before starting turnd: {path}")]
    RelativePath { kind: &'static str, path: PathBuf },
}

#[derive(Debug, Clone)]
struct LaunchContext {
    current_exe: PathBuf,
    configured_binary: Option<PathBuf>,
    workspace_manifest: Option<PathBuf>,
    cargo: OsString,
    log_path: PathBuf,
}

impl LaunchContext {
    fn from_process(data_dir: &Path) -> Result<Self, CompanionError> {
        let current_exe = std::env::current_exe().map_err(CompanionError::CurrentExecutable)?;
        let configured_binary = non_blank(std::env::var_os(DAEMON_BIN_ENV)).map(PathBuf::from);
        let workspace_manifest = development_manifest();
        let cargo = non_blank(std::env::var_os("CARGO")).unwrap_or_else(|| OsString::from("cargo"));
        let log_path = data_dir.join(DAEMON_LOG_FILE);
        Ok(Self {
            current_exe,
            configured_binary,
            workspace_manifest,
            cargo,
            log_path,
        })
    }
}

#[derive(Debug, Clone)]
struct LaunchSpec {
    program: PathBuf,
    prefix_args: Vec<OsString>,
    build_args: Option<Vec<OsString>>,
    current_dir: Option<PathBuf>,
    source: CompanionSource,
}

/// Ensures that the daemon endpoint exists without ever adopting or stopping an
/// existing daemon. A startup race is harmless: `turnd` owns the singleton locks and
/// exactly one contender wins.
pub fn ensure(socket: &Path, data_dir: &Path) -> Result<EnsureOutcome, CompanionError> {
    if !socket_needs_companion(socket)? {
        return Ok(EnsureOutcome::EndpointOccupied);
    }
    let context = LaunchContext::from_process(data_dir)?;
    launch_with(socket, data_dir, &context)
}

#[cfg(test)]
fn ensure_with(
    socket: &Path,
    data_dir: &Path,
    context: &LaunchContext,
) -> Result<EnsureOutcome, CompanionError> {
    if !socket_needs_companion(socket)? {
        return Ok(EnsureOutcome::EndpointOccupied);
    }
    launch_with(socket, data_dir, context)
}

fn launch_with(
    socket: &Path,
    data_dir: &Path,
    context: &LaunchContext,
) -> Result<EnsureOutcome, CompanionError> {
    if !socket.is_absolute() {
        return Err(CompanionError::RelativePath {
            kind: "socket",
            path: socket.to_path_buf(),
        });
    }
    if !data_dir.is_absolute() {
        return Err(CompanionError::RelativePath {
            kind: "data directory",
            path: data_dir.to_path_buf(),
        });
    }
    let spec = resolve_launch(context)?;
    prepare_development_companions(&spec, &context.log_path)?;
    spawn_companion(socket, data_dir, &context.log_path, spec)
}

fn socket_needs_companion(socket: &Path) -> Result<bool, CompanionError> {
    match std::fs::symlink_metadata(socket) {
        Ok(metadata) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::FileTypeExt;
                if !metadata.file_type().is_socket() {
                    return Err(CompanionError::UnsafeSocketEntry {
                        socket: socket.to_path_buf(),
                    });
                }
            }
            #[cfg(not(unix))]
            {
                let _ = metadata;
            }
        }
        Err(cause) if cause.kind() == io::ErrorKind::NotFound => return Ok(true),
        Err(cause) => {
            return Err(CompanionError::SocketProbe {
                socket: socket.to_path_buf(),
                cause,
            })
        }
    }

    match std::os::unix::net::UnixStream::connect(socket) {
        Ok(_) => Ok(false),
        Err(cause)
            if matches!(
                cause.kind(),
                io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
            ) =>
        {
            // A stale socket is deliberately handed to turnd. Its handshake-aware
            // singleton boundary is the authority which may remove it.
            Ok(true)
        }
        Err(cause) => Err(CompanionError::SocketProbe {
            socket: socket.to_path_buf(),
            cause,
        }),
    }
}

fn resolve_launch(context: &LaunchContext) -> Result<LaunchSpec, CompanionError> {
    if let Some(configured) = &context.configured_binary {
        let configured = absolute_from_current_dir(configured);
        if !configured.is_file() {
            return Err(CompanionError::ConfiguredBinaryMissing { path: configured });
        }
        return Ok(LaunchSpec {
            program: configured,
            prefix_args: Vec::new(),
            build_args: None,
            current_dir: None,
            source: CompanionSource::Configured,
        });
    }

    let sibling = context.current_exe.with_file_name(daemon_file_name());
    if sibling.is_file() {
        return Ok(LaunchSpec {
            program: sibling,
            prefix_args: Vec::new(),
            build_args: None,
            current_dir: None,
            source: CompanionSource::PackagedSibling,
        });
    }

    if let Some(manifest) = &context.workspace_manifest {
        let workspace = manifest.parent().map(Path::to_path_buf);
        return Ok(LaunchSpec {
            program: PathBuf::from(&context.cargo),
            prefix_args: vec![
                OsString::from("run"),
                OsString::from("--quiet"),
                OsString::from("--manifest-path"),
                manifest.as_os_str().to_owned(),
                OsString::from("--bin"),
                OsString::from("turnd"),
                OsString::from("--"),
            ],
            build_args: Some(vec![
                OsString::from("build"),
                OsString::from("--quiet"),
                OsString::from("--manifest-path"),
                manifest.as_os_str().to_owned(),
                OsString::from("--bin"),
                OsString::from("turnd"),
                OsString::from("--bin"),
                OsString::from("turn-hook"),
            ]),
            current_dir: workspace,
            source: CompanionSource::CargoWorkspace,
        });
    }

    Err(CompanionError::CompanionMissing { sibling })
}

/// A source checkout does not necessarily have either companion yet. Build both before
/// `cargo run`: Claude/Codex hooks degrade if `turn-hook` is missing beside `turnd`, so
/// building only the daemon would make a successful-looking bootstrap semantically
/// incomplete. This path is compiled into debug builds only by `development_manifest`.
fn prepare_development_companions(
    spec: &LaunchSpec,
    log_path: &Path,
) -> Result<(), CompanionError> {
    let Some(build_args) = &spec.build_args else {
        return Ok(());
    };
    let log = open_log(log_path)?;
    let stderr = log.try_clone().map_err(|cause| CompanionError::LogFile {
        path: log_path.to_path_buf(),
        cause,
    })?;
    let mut command = Command::new(&spec.program);
    command
        .args(build_args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(stderr));
    if let Some(current_dir) = &spec.current_dir {
        command.current_dir(current_dir);
    }
    let status = command.status().map_err(|cause| CompanionError::Spawn {
        program: spec.program.clone(),
        log_path: log_path.to_path_buf(),
        cause,
    })?;
    if status.success() {
        Ok(())
    } else {
        Err(CompanionError::DevelopmentBuild {
            program: spec.program.clone(),
            status,
            log_path: log_path.to_path_buf(),
        })
    }
}

fn spawn_companion(
    socket: &Path,
    data_dir: &Path,
    log_path: &Path,
    spec: LaunchSpec,
) -> Result<EnsureOutcome, CompanionError> {
    let log = open_log(log_path)?;
    let stderr = log.try_clone().map_err(|cause| CompanionError::LogFile {
        path: log_path.to_path_buf(),
        cause,
    })?;
    let mut command = Command::new(&spec.program);
    command
        .args(&spec.prefix_args)
        .arg("--socket")
        .arg(socket)
        .arg("--data-dir")
        .arg(data_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(stderr));
    if let Some(current_dir) = &spec.current_dir {
        command.current_dir(current_dir);
    }
    detach(&mut command);

    let child = command.spawn().map_err(|cause| CompanionError::Spawn {
        program: spec.program.clone(),
        log_path: log_path.to_path_buf(),
        cause,
    })?;
    let launcher_pid = child.id();

    let program = spec.program;
    let log_path = log_path.to_path_buf();
    Ok(EnsureOutcome::Started(CompanionLaunch {
        started: CompanionStarted {
            launcher_pid,
            source: spec.source,
            program: program.clone(),
            log_path: log_path.clone(),
        },
        monitor: CompanionMonitor {
            child,
            program,
            log_path,
        },
    }))
}

fn open_log(path: &Path) -> Result<File, CompanionError> {
    let parent = path.parent().ok_or_else(|| CompanionError::LogDirectory {
        path: path.to_path_buf(),
        cause: io::Error::new(io::ErrorKind::InvalidInput, "the log path has no parent"),
    })?;
    std::fs::create_dir_all(parent).map_err(|cause| CompanionError::LogDirectory {
        path: parent.to_path_buf(),
        cause,
    })?;
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    }
    let file = options
        .open(path)
        .map_err(|cause| CompanionError::LogFile {
            path: path.to_path_buf(),
            cause,
        })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if !file
            .metadata()
            .map_err(|cause| CompanionError::LogFile {
                path: path.to_path_buf(),
                cause,
            })?
            .is_file()
        {
            return Err(CompanionError::LogFile {
                path: path.to_path_buf(),
                cause: io::Error::new(io::ErrorKind::InvalidInput, "the log is not a regular file"),
            });
        }
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|cause| CompanionError::LogFile {
                path: path.to_path_buf(),
                cause,
            })?;
    }
    Ok(file)
}

fn development_manifest() -> Option<PathBuf> {
    if !cfg!(debug_assertions) {
        return None;
    }
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("Cargo.toml");
    let turnd_manifest = manifest.parent()?.join("crates/turnd/Cargo.toml");
    (manifest.is_file() && turnd_manifest.is_file()).then_some(manifest)
}

fn absolute_from_current_dir(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    }
}

fn non_blank(value: Option<OsString>) -> Option<OsString> {
    value.filter(|value| !value.to_string_lossy().trim().is_empty())
}

fn daemon_file_name() -> &'static OsStr {
    OsStr::new(if cfg!(windows) { "turnd.exe" } else { "turnd" })
}

#[cfg(unix)]
fn detach(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: `setsid` is an async-signal-safe syscall and touches no Rust memory.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
}

#[cfg(not(unix))]
fn detach(_command: &mut Command) {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    fn context(root: &Path) -> LaunchContext {
        LaunchContext {
            current_exe: root.join("bin/turn"),
            configured_binary: None,
            workspace_manifest: None,
            cargo: OsString::from("cargo"),
            log_path: root.join("data/turnd.log"),
        }
    }

    fn ensure_for(socket: &Path, context: &LaunchContext) -> Result<EnsureOutcome, CompanionError> {
        ensure_with(
            socket,
            context
                .log_path
                .parent()
                .expect("the test log has a parent"),
            context,
        )
    }

    #[cfg(unix)]
    fn script(path: &Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, format!("#!/bin/sh\nset -eu\n{body}\n")).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_reachable_socket_is_never_replaced_or_given_to_a_new_process() {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("live.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let mut context = context(temp.path());
        context.configured_binary = Some(temp.path().join("does-not-exist"));

        assert!(matches!(
            ensure_for(&socket, &context).unwrap(),
            EnsureOutcome::EndpointOccupied
        ));
        assert!(!context.log_path.exists());
    }

    #[test]
    fn the_packaged_sibling_wins_without_searching_path() {
        let temp = tempfile::tempdir().unwrap();
        let mut context = context(temp.path());
        std::fs::create_dir_all(context.current_exe.parent().unwrap()).unwrap();
        let sibling = context.current_exe.with_file_name(daemon_file_name());
        std::fs::write(&sibling, b"companion").unwrap();
        context.workspace_manifest = Some(temp.path().join("workspace/Cargo.toml"));

        let spec = resolve_launch(&context).unwrap();
        assert_eq!(spec.program, sibling);
        assert_eq!(spec.source, CompanionSource::PackagedSibling);
        assert!(spec.prefix_args.is_empty());
        assert!(spec.build_args.is_none());
    }

    #[test]
    fn source_development_uses_one_fixed_manifest_and_no_shell() {
        let temp = tempfile::tempdir().unwrap();
        let mut context = context(temp.path());
        let manifest = temp.path().join("workspace/Cargo.toml");
        context.workspace_manifest = Some(manifest.clone());
        context.cargo = OsString::from("/toolchain/bin/cargo");

        let spec = resolve_launch(&context).unwrap();
        assert_eq!(spec.program, PathBuf::from("/toolchain/bin/cargo"));
        assert_eq!(spec.source, CompanionSource::CargoWorkspace);
        assert_eq!(
            spec.prefix_args,
            vec![
                "run",
                "--quiet",
                "--manifest-path",
                manifest.to_str().unwrap(),
                "--bin",
                "turnd",
                "--"
            ]
            .into_iter()
            .map(OsString::from)
            .collect::<Vec<_>>()
        );
        let build_args = spec
            .build_args
            .expect("source fallback builds both companions");
        assert!(build_args.windows(2).any(|pair| pair == ["--bin", "turnd"]));
        assert!(build_args
            .windows(2)
            .any(|pair| pair == ["--bin", "turn-hook"]));
    }

    #[test]
    fn an_explicit_missing_binary_fails_closed_instead_of_falling_back() {
        let temp = tempfile::tempdir().unwrap();
        let mut context = context(temp.path());
        context.configured_binary = Some(temp.path().join("missing-turnd"));
        context.workspace_manifest = Some(temp.path().join("workspace/Cargo.toml"));
        let error = resolve_launch(&context).unwrap_err();
        assert!(matches!(
            error,
            CompanionError::ConfiguredBinaryMissing { .. }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn dropping_the_child_handle_does_not_end_the_detached_companion() {
        let temp = tempfile::tempdir().unwrap();
        let daemon = temp.path().join("fake-turnd");
        script(&daemon, "sleep 0.75\nprintf '%s\\n' \"$@\" > \"$0.args\"");
        let mut context = context(temp.path());
        context.configured_binary = Some(daemon.clone());
        let socket = temp.path().join("socket with spaces.sock");

        let outcome = ensure_for(&socket, &context).unwrap();
        assert!(matches!(outcome, EnsureOutcome::Started(_)));
        drop(outcome);
        let args = daemon.with_extension("args");
        // This test runs alongside more than two hundred GUI tests and the detached
        // shell deliberately sleeps before proving it survived the handle drop. Give
        // a loaded CI machine scheduling headroom; the loop still exits immediately
        // once the proof file appears.
        let deadline = Instant::now() + Duration::from_secs(5);
        while !args.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            std::fs::read_to_string(args).unwrap(),
            format!(
                "--socket\n{}\n--data-dir\n{}\n",
                socket.display(),
                context.log_path.parent().unwrap().display()
            )
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_immediate_companion_failure_is_reported_by_the_monitor() {
        let temp = tempfile::tempdir().unwrap();
        let daemon = temp.path().join("bad-turnd");
        script(&daemon, "echo 'broken package' >&2\nexit 7");
        let mut context = context(temp.path());
        context.configured_binary = Some(daemon.clone());

        let launch = match ensure_for(&temp.path().join("missing.sock"), &context).unwrap() {
            EnsureOutcome::Started(launch) => launch,
            other => panic!("expected a launch, got {other:?}"),
        };
        let events = launch.monitor.watch(Arc::new(|| {}));
        let CompanionEvent::Failed(message) = events.recv_timeout(Duration::from_secs(2)).unwrap()
        else {
            panic!("a non-contention exit must be a failure")
        };
        assert!(message.contains("exit status: 7"), "{message}");
        assert!(
            message.contains(&context.log_path.display().to_string()),
            "{message}"
        );
        assert!(std::fs::read_to_string(&context.log_path)
            .unwrap()
            .contains("broken package"));
    }

    #[cfg(unix)]
    #[test]
    fn a_non_socket_entry_is_not_deleted_or_replaced() {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("turnd.sock");
        std::fs::write(&socket, b"not ours").unwrap();
        let error = socket_needs_companion(&socket).unwrap_err();
        assert!(matches!(error, CompanionError::UnsafeSocketEntry { .. }));
        assert_eq!(std::fs::read(&socket).unwrap(), b"not ours");
    }

    #[cfg(unix)]
    #[test]
    fn the_companion_runs_in_a_session_separate_from_the_gui() {
        let temp = tempfile::tempdir().unwrap();
        let daemon = temp.path().join("fake-turnd");
        script(&daemon, "sleep 1");
        let mut context = context(temp.path());
        context.configured_binary = Some(daemon);
        let launch = match ensure_for(&temp.path().join("missing.sock"), &context).unwrap() {
            EnsureOutcome::Started(launch) => launch,
            other => panic!("expected a launch, got {other:?}"),
        };
        let pid = launch.started.launcher_pid as libc::pid_t;

        // A session leader's SID is its own PID. This is the OS property which keeps
        // closing a terminal/window from delivering the GUI's hangup to turnd.
        assert_eq!(unsafe { libc::getsid(pid) }, pid);
        drop(launch);
    }

    #[cfg(unix)]
    #[test]
    fn a_delayed_exit_is_delivered_to_the_window_and_wakes_it() {
        let temp = tempfile::tempdir().unwrap();
        let daemon = temp.path().join("late-failure-turnd");
        script(&daemon, "sleep 0.25\necho 'late failure' >&2\nexit 7");
        let mut context = context(temp.path());
        context.configured_binary = Some(daemon);
        let launch = match ensure_for(&temp.path().join("missing.sock"), &context).unwrap() {
            EnsureOutcome::Started(launch) => launch,
            other => panic!("expected a launch, got {other:?}"),
        };
        let wake_count = Arc::new(AtomicUsize::new(0));
        let wake_counter = Arc::clone(&wake_count);
        let events = launch.monitor.watch(Arc::new(move || {
            wake_counter.fetch_add(1, Ordering::SeqCst);
        }));

        let CompanionEvent::Failed(message) = events.recv_timeout(Duration::from_secs(2)).unwrap()
        else {
            panic!("a non-contention exit must be a failure")
        };
        assert!(message.contains("exit status: 7"), "{message}");
        assert!(message.contains(&context.log_path.display().to_string()));
        assert_eq!(wake_count.load(Ordering::SeqCst), 1);
        assert!(std::fs::read_to_string(context.log_path)
            .unwrap()
            .contains("late failure"));
    }

    #[cfg(unix)]
    #[test]
    fn exit_three_is_provisional_contention_not_a_launch_failure() {
        let temp = tempfile::tempdir().unwrap();
        let daemon = temp.path().join("contended-turnd");
        script(&daemon, "exit 3");
        let mut context = context(temp.path());
        context.configured_binary = Some(daemon);

        let launch = match ensure_for(&temp.path().join("missing.sock"), &context).unwrap() {
            EnsureOutcome::Started(launch) => launch,
            other => panic!("expected a launch, got {other:?}"),
        };
        let events = launch.monitor.watch(Arc::new(|| {}));
        let event = events.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(matches!(event, CompanionEvent::Contended(_)));
    }

    #[cfg(unix)]
    #[test]
    fn the_log_refuses_symlinks_and_is_owner_only() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target");
        std::fs::write(&target, b"do not append").unwrap();
        let link = temp.path().join("turnd.log");
        symlink(&target, &link).unwrap();
        assert!(matches!(
            open_log(&link).unwrap_err(),
            CompanionError::LogFile { .. }
        ));
        assert_eq!(std::fs::read(&target).unwrap(), b"do not append");

        std::fs::remove_file(link).unwrap();
        let file = open_log(&temp.path().join("safe.log")).unwrap();
        assert_eq!(file.metadata().unwrap().permissions().mode() & 0o777, 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn source_preparation_builds_turnd_and_turn_hook_without_a_shell() {
        let temp = tempfile::tempdir().unwrap();
        let cargo = temp.path().join("fake-cargo");
        script(&cargo, "printf '%s\\n' \"$@\" > \"$0.args\"");
        let mut context = context(temp.path());
        context.cargo = cargo.clone().into_os_string();
        let manifest = temp.path().join("workspace/Cargo.toml");
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        std::fs::write(&manifest, b"[workspace]").unwrap();
        context.workspace_manifest = Some(manifest);
        let spec = resolve_launch(&context).unwrap();

        prepare_development_companions(&spec, &context.log_path).unwrap();
        let args = std::fs::read_to_string(cargo.with_extension("args")).unwrap();
        assert!(args.contains("build\n"), "{args}");
        assert!(args.contains("turnd\n"), "{args}");
        assert!(args.contains("turn-hook\n"), "{args}");
    }
}
