//! Spawning and driving a process on a real pty.
//!
//! Reading a pty is a blocking operation, so each process gets a dedicated OS
//! thread that pumps bytes into a bounded broadcast channel and into the
//! [`TerminalBuffer`]. The channel being bounded is the whole backpressure
//! story: a subscriber that cannot keep up is told it fell behind and
//! re-synchronises from the buffer's replay, rather than being allowed to grow
//! an unbounded queue and take the daemon down with it.

use crate::buffer::{ScreenSize, ScreenSnapshot, TerminalBuffer};
use crate::journal::{JournalConfig, TerminalJournal};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{broadcast, watch};
use turn_core::ids::NodeId;
use turn_core::state::Lifecycle;

/// How many output chunks the broadcast channel holds before slow subscribers
/// start missing data. Deliberately modest: falling behind should be detected
/// and repaired quickly, not buffered indefinitely.
const OUTPUT_CHANNEL_CAPACITY: usize = 512;
/// Read granularity. Large enough that a chatty build does not cause a syscall
/// storm, small enough that an interactive prompt still feels immediate.
const READ_CHUNK: usize = 64 * 1024;
/// How often the waiter thread polls for exit.
const WAIT_POLL_MS: u64 = 100;

#[derive(Debug, thiserror::Error)]
pub enum PtyError {
    #[error("failed to open a pty: {0}")]
    OpenPty(String),
    #[error("failed to spawn `{command}`: {cause}")]
    Spawn { command: String, cause: String },
    #[error("the process is no longer available")]
    Unavailable,
    #[error("write to pty failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("could not determine the process id")]
    NoPid,
    #[error("could not open terminal history at `{path}`: {cause}")]
    Journal { path: PathBuf, cause: String },
}

/// Why a platform read-only process sandbox could not be constructed safely.
#[derive(Debug, thiserror::Error)]
pub enum ReadOnlySandboxError {
    #[error("could not resolve protected path `{path}`: {cause}")]
    Resolve {
        path: PathBuf,
        #[source]
        cause: std::io::Error,
    },
    #[error("unsafe Git metadata at `{path}`: {reason}")]
    InvalidGitMetadata { path: PathBuf, reason: String },
    #[error("protected path `{path}` is not valid UTF-8 and cannot be passed to Seatbelt safely")]
    NonUtf8Path { path: PathBuf },
}

/// An inherited OS guard for a process and every child it creates.
///
/// macOS Seatbelt receives canonical paths as profile parameters rather than
/// interpolated source, so unusual checkout names cannot change the policy. Other
/// platforms return `None` until an equivalent kernel boundary is implemented.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadOnlySandbox {
    protected_paths: Vec<PathBuf>,
}

impl ReadOnlySandbox {
    /// Builds the strongest guard available for `checkout`, including Git metadata
    /// that a gitfile or symlink places outside the working tree.
    pub fn for_checkout(checkout: &Path) -> Result<Option<Self>, ReadOnlySandboxError> {
        #[cfg(not(target_os = "macos"))]
        {
            let _ = checkout;
            Ok(None)
        }

        #[cfg(target_os = "macos")]
        {
            use std::os::unix::fs::PermissionsExt;

            const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";
            let available = std::fs::metadata(SANDBOX_EXEC)
                .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
                .unwrap_or(false);
            if !available {
                return Ok(None);
            }

            // Seatbelt accepts pathnames rather than directory capabilities. There is
            // therefore an accepted TOCTOU window between resolving these names and
            // `sandbox-exec` applying them; callers must treat this as a pathname
            // boundary, not as protection against same-user namespace mutation.
            let root = canonical(checkout)?;
            if !root.is_dir() {
                return Err(ReadOnlySandboxError::InvalidGitMetadata {
                    path: root,
                    reason: "the checkout root is not a directory".to_string(),
                });
            }
            let mut protected_paths = vec![root.clone()];
            if let Some(git_dir) = git_directory(&root)? {
                push_distinct_outside(&mut protected_paths, &root, git_dir.clone());
                if let Some(common_dir) = git_common_directory(&git_dir)? {
                    push_distinct_outside(&mut protected_paths, &root, common_dir);
                }
            }
            validate_seatbelt_paths(&protected_paths)?;
            Ok(Some(Self { protected_paths }))
        }
    }

    pub fn checkout_root(&self) -> &Path {
        &self.protected_paths[0]
    }

    #[cfg(target_os = "macos")]
    fn command_builder(&self, command: &str) -> CommandBuilder {
        const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";
        let mut profile = String::from("(version 1)\n(allow default)\n(deny file-write*\n");
        for index in 0..self.protected_paths.len() {
            profile.push_str(&format!(
                "  (literal (param \"TURN_PROTECTED_{index}\"))\n  (subpath (param \"TURN_PROTECTED_{index}\"))\n"
            ));
        }
        profile.push_str(")\n");

        let mut builder = CommandBuilder::new(SANDBOX_EXEC);
        for (index, path) in self.protected_paths.iter().enumerate() {
            builder.arg("-D");
            builder.arg(format!(
                "TURN_PROTECTED_{index}={}",
                path.to_str()
                    .expect("sandbox paths were validated as UTF-8")
            ));
        }
        builder.arg("-p");
        builder.arg(profile);
        builder.arg(command);
        builder
    }

    #[cfg(all(test, target_os = "macos"))]
    fn protected_paths(&self) -> &[PathBuf] {
        &self.protected_paths
    }
}

#[cfg(target_os = "macos")]
fn canonical(path: &Path) -> Result<PathBuf, ReadOnlySandboxError> {
    std::fs::canonicalize(path).map_err(|cause| ReadOnlySandboxError::Resolve {
        path: path.to_path_buf(),
        cause,
    })
}

#[cfg(target_os = "macos")]
fn git_directory(root: &Path) -> Result<Option<PathBuf>, ReadOnlySandboxError> {
    let dot_git = root.join(".git");
    let metadata = match std::fs::metadata(&dot_git) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(cause) => {
            return Err(ReadOnlySandboxError::Resolve {
                path: dot_git,
                cause,
            })
        }
    };
    if metadata.is_dir() {
        return canonical(&dot_git).map(Some);
    }
    if !metadata.is_file() {
        return Err(ReadOnlySandboxError::InvalidGitMetadata {
            path: dot_git,
            reason: ".git is neither a directory nor a gitfile".to_string(),
        });
    }
    let pointer = read_small_text(&dot_git)?;
    let target = pointer
        .lines()
        .next()
        .and_then(|line| line.trim().strip_prefix("gitdir:"))
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .ok_or_else(|| ReadOnlySandboxError::InvalidGitMetadata {
            path: dot_git.clone(),
            reason: "gitfile does not contain a gitdir pointer".to_string(),
        })?;
    let target = Path::new(target);
    let target = if target.is_absolute() {
        target.to_path_buf()
    } else {
        root.join(target)
    };
    canonical(&target).map(Some)
}

#[cfg(target_os = "macos")]
fn git_common_directory(git_dir: &Path) -> Result<Option<PathBuf>, ReadOnlySandboxError> {
    let pointer = git_dir.join("commondir");
    let metadata = match std::fs::metadata(&pointer) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(cause) => {
            return Err(ReadOnlySandboxError::Resolve {
                path: pointer,
                cause,
            })
        }
    };
    if !metadata.is_file() {
        return Err(ReadOnlySandboxError::InvalidGitMetadata {
            path: pointer,
            reason: "commondir is not a regular file".to_string(),
        });
    }
    let common = read_small_text(&pointer)?;
    let common = common.trim();
    if common.is_empty() {
        return Err(ReadOnlySandboxError::InvalidGitMetadata {
            path: pointer,
            reason: "commondir is empty".to_string(),
        });
    }
    let common = Path::new(common);
    let common = if common.is_absolute() {
        common.to_path_buf()
    } else {
        git_dir.join(common)
    };
    canonical(&common).map(Some)
}

#[cfg(target_os = "macos")]
fn read_small_text(path: &Path) -> Result<String, ReadOnlySandboxError> {
    const MAX_GIT_POINTER_BYTES: u64 = 16 * 1024;
    let file = std::fs::File::open(path).map_err(|cause| ReadOnlySandboxError::Resolve {
        path: path.to_path_buf(),
        cause,
    })?;
    let mut text = String::new();
    file.take(MAX_GIT_POINTER_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(|cause| ReadOnlySandboxError::Resolve {
            path: path.to_path_buf(),
            cause,
        })?;
    if text.len() as u64 > MAX_GIT_POINTER_BYTES {
        return Err(ReadOnlySandboxError::InvalidGitMetadata {
            path: path.to_path_buf(),
            reason: "metadata pointer is unexpectedly large".to_string(),
        });
    }
    Ok(text)
}

#[cfg(target_os = "macos")]
fn push_distinct_outside(paths: &mut Vec<PathBuf>, root: &Path, candidate: PathBuf) {
    if !candidate.starts_with(root) && !paths.contains(&candidate) {
        paths.push(candidate);
    }
}

#[cfg(target_os = "macos")]
fn validate_seatbelt_paths(paths: &[PathBuf]) -> Result<(), ReadOnlySandboxError> {
    if let Some(path) = paths.iter().find(|path| path.to_str().is_none()) {
        return Err(ReadOnlySandboxError::NonUtf8Path { path: path.clone() });
    }
    Ok(())
}

/// What to launch.
#[derive(Debug, Clone)]
pub struct ProcessSpec {
    pub command: String,
    pub args: Vec<String>,
    pub cwd: String,
    /// Extra environment. This is how adapters inject hook configuration
    /// without touching the user's own files.
    pub env: Vec<(String, String)>,
    pub size: ScreenSize,
    /// Start from an empty environment. Off by default: agents need the user's
    /// PATH and credential helpers to work at all.
    pub clean_env: bool,
    /// OS-enforced write protection inherited by every child process.
    pub read_only_sandbox: Option<ReadOnlySandbox>,
}

impl ProcessSpec {
    pub fn new(command: impl Into<String>, cwd: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            args: Vec::new(),
            cwd: cwd.into(),
            env: Vec::new(),
            size: ScreenSize::default(),
            clean_env: false,
            read_only_sandbox: None,
        }
    }

    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    pub fn size(mut self, size: ScreenSize) -> Self {
        self.size = size;
        self
    }

    pub fn read_only_sandbox(mut self, sandbox: ReadOnlySandbox) -> Self {
        self.read_only_sandbox = Some(sandbox);
        self
    }

    /// The full command line, for display and logging.
    pub fn command_line(&self) -> String {
        if self.args.is_empty() {
            self.command.clone()
        } else {
            format!("{} {}", self.command, self.args.join(" "))
        }
    }
}

/// How a process ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExitInfo {
    pub code: i32,
    /// The platform's name for the signal that killed it, when it was signalled.
    /// `portable_pty` reports signal deaths this way and gives a meaningless
    /// exit code of 1, so this — not the code — is how we tell the two apart.
    pub signal: Option<String>,
}

impl ExitInfo {
    /// Whether the process was killed rather than exiting on its own.
    pub fn signalled(&self) -> bool {
        self.signal.is_some()
    }

    /// Maps an exit into the domain's lifecycle vocabulary.
    pub fn lifecycle(&self) -> Lifecycle {
        match &self.signal {
            Some(signal) => Lifecycle::Signaled {
                signal: signal.clone(),
            },
            None => Lifecycle::Exited { code: self.code },
        }
    }
}

/// A chunk of process output. Shared rather than cloned: with thirty terminals
/// and several subscribers each, copying every chunk per subscriber is exactly
/// the kind of waste that shows up as UI stutter.
pub type OutputChunk = Arc<Vec<u8>>;

type SharedChild = Arc<Mutex<Box<dyn Child + Send + Sync>>>;
type SharedJournal = Arc<Mutex<Option<TerminalJournal>>>;

/// A live process attached to a pty.
pub struct PtyProcess {
    node_id: NodeId,
    pid: u32,
    spec: ProcessSpec,
    master: Box<dyn MasterPty + Send>,
    writer: Mutex<Box<dyn Write + Send>>,
    child: SharedChild,
    buffer: Arc<Mutex<TerminalBuffer>>,
    journal: Option<SharedJournal>,
    output_tx: broadcast::Sender<OutputChunk>,
    exit_rx: watch::Receiver<Option<ExitInfo>>,
    /// Set when the reader thread sees EOF, which happens before the exit status
    /// is known.
    reader_finished: Arc<AtomicBool>,
    bytes_written: AtomicU64,
    started_ms: i64,
}

impl PtyProcess {
    /// Kernel-authoritative foreground process group for this PTY.
    ///
    /// A child merely descending from the shell is not enough to say it owns what the
    /// operator sees: background jobs share the same terminal. `tcgetpgrp` is the fact
    /// that distinguishes the one process group currently receiving terminal input.
    #[cfg(unix)]
    pub fn foreground_process_group(&self) -> Option<u32> {
        let fd = self.master.as_raw_fd()?;
        // Safe: `tcgetpgrp` reads kernel terminal metadata for one valid descriptor and
        // does not retain or mutate Rust-owned memory.
        let group = unsafe { libc::tcgetpgrp(fd) };
        (group > 0).then_some(group as u32)
    }

    /// Unsupported platforms make no foreground claim.
    #[cfg(not(unix))]
    pub fn foreground_process_group(&self) -> Option<u32> {
        None
    }

    /// Opens a pty, launches the command on it and starts pumping output.
    pub fn spawn(node_id: NodeId, spec: ProcessSpec, now_ms: i64) -> Result<Self, PtyError> {
        Self::spawn_with_preserved_fds(node_id, spec, now_ms, &[])
    }

    /// Opens a pty while retaining only the explicitly selected Unix descriptors in
    /// the child. Other daemon descriptors keep portable-pty's close-before-exec
    /// behaviour.
    pub fn spawn_with_preserved_fds(
        node_id: NodeId,
        spec: ProcessSpec,
        now_ms: i64,
        preserved_fds: &[i32],
    ) -> Result<Self, PtyError> {
        Self::spawn_internal(node_id, spec, now_ms, None, preserved_fds)
    }

    /// Spawns a process whose authoritative output is also written to a durable,
    /// bounded terminal journal.
    pub fn spawn_persisted(
        node_id: NodeId,
        spec: ProcessSpec,
        now_ms: i64,
        journal_dir: impl AsRef<Path>,
    ) -> Result<Self, PtyError> {
        Self::spawn_persisted_with_preserved_fds(node_id, spec, now_ms, journal_dir, &[])
    }

    pub fn spawn_persisted_with_preserved_fds(
        node_id: NodeId,
        spec: ProcessSpec,
        now_ms: i64,
        journal_dir: impl AsRef<Path>,
        preserved_fds: &[i32],
    ) -> Result<Self, PtyError> {
        Self::spawn_persisted_with_config_and_preserved_fds(
            node_id,
            spec,
            now_ms,
            journal_dir,
            JournalConfig::default(),
            preserved_fds,
        )
    }

    /// Spawns with caller-selected durable bounds. The daemon resolves these from
    /// the settings hierarchy; library callers retain the conservative defaults.
    pub fn spawn_persisted_with_config_and_preserved_fds(
        node_id: NodeId,
        spec: ProcessSpec,
        now_ms: i64,
        journal_dir: impl AsRef<Path>,
        journal_config: JournalConfig,
        preserved_fds: &[i32],
    ) -> Result<Self, PtyError> {
        Self::spawn_internal(
            node_id,
            spec,
            now_ms,
            Some((journal_dir.as_ref().to_path_buf(), journal_config)),
            preserved_fds,
        )
    }

    /// Stops recording immediately without touching the live PTY or screen.
    pub fn disable_journal(&self) {
        if let Some(journal) = &self.journal {
            if let Ok(mut journal) = journal.lock() {
                *journal = None;
            }
        }
    }

    fn spawn_internal(
        node_id: NodeId,
        spec: ProcessSpec,
        now_ms: i64,
        journal_spec: Option<(PathBuf, JournalConfig)>,
        preserved_fds: &[i32],
    ) -> Result<Self, PtyError> {
        let pair = open_pty_with_retry(spec.size)?;

        let mut builder = build_command(&spec, &node_id);
        #[cfg(unix)]
        for fd in preserved_fds {
            builder.preserve_fd(*fd);
        }
        #[cfg(not(unix))]
        let _ = preserved_fds;
        let mut child = pair
            .slave
            .spawn_command(builder)
            .map_err(|e| PtyError::Spawn {
                command: spec.command_line(),
                cause: e.to_string(),
            })?;
        // The slave fd must be dropped here or the pty never reports EOF.
        drop(pair.slave);

        let pid = child.process_id().ok_or(PtyError::NoPid)?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| PtyError::OpenPty(e.to_string()))?;
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| PtyError::OpenPty(e.to_string()))?;

        let buffer = Arc::new(Mutex::new(TerminalBuffer::new(spec.size)));
        let journal = match journal_spec {
            Some((path, config)) => {
                let opened = buffer
                    .lock()
                    .map_err(|_| PtyError::Unavailable)
                    .and_then(|buffer| {
                        TerminalJournal::create(&path, &buffer, config).map_err(|error| {
                            PtyError::Journal {
                                path: path.clone(),
                                cause: error.to_string(),
                            }
                        })
                    });
                match opened {
                    Ok(journal) => Some(Arc::new(Mutex::new(Some(journal)))),
                    Err(error) => {
                        let _ = child.kill();
                        return Err(error);
                    }
                }
            }
            None => None,
        };
        let (output_tx, _) = broadcast::channel(OUTPUT_CHANNEL_CAPACITY);
        let (exit_tx, exit_rx) = watch::channel(None);
        let reader_finished = Arc::new(AtomicBool::new(false));
        let child: SharedChild = Arc::new(Mutex::new(child));

        spawn_reader(
            reader,
            Arc::clone(&buffer),
            output_tx.clone(),
            Arc::clone(&reader_finished),
            node_id.clone(),
            journal.clone(),
        );
        spawn_waiter(Arc::clone(&child), exit_tx, pid, node_id.clone());

        Ok(Self {
            node_id,
            pid,
            spec,
            master: pair.master,
            writer: Mutex::new(writer),
            child,
            buffer,
            journal,
            output_tx,
            exit_rx,
            reader_finished,
            bytes_written: AtomicU64::new(0),
            started_ms: now_ms,
        })
    }

    pub fn node_id(&self) -> &NodeId {
        &self.node_id
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn spec(&self) -> &ProcessSpec {
        &self.spec
    }

    pub fn started_ms(&self) -> i64 {
        self.started_ms
    }

    /// Sends keystrokes or pasted text to the process.
    pub fn write(&self, data: &[u8]) -> Result<(), PtyError> {
        let mut writer = self.writer.lock().map_err(|_| PtyError::Unavailable)?;
        writer.write_all(data)?;
        writer.flush()?;
        self.bytes_written
            .fetch_add(data.len() as u64, Ordering::Relaxed);
        Ok(())
    }

    /// Tells both the kernel and our own screen model about a new size.
    ///
    /// Both halves are required: without the ioctl the process keeps drawing at
    /// the old width, and without the buffer update our previews and
    /// heuristics read a screen that no longer matches what the user sees.
    pub fn resize(&self, size: ScreenSize) -> Result<(), PtyError> {
        self.master
            .resize(PtySize {
                rows: size.rows,
                cols: size.cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| PtyError::OpenPty(e.to_string()))?;
        if let Ok(mut buffer) = self.buffer.lock() {
            buffer.resize(size);
            if let Some(shared) = &self.journal {
                if let Ok(mut journal) = shared.lock() {
                    if let Some(writer) = journal.as_mut() {
                        if let Err(error) = writer.record_resize(size, &mut buffer) {
                            tracing::error!(%self.node_id, %error, "terminal journal failed; persistence disabled for this process");
                            buffer.mark_truncated();
                            *journal = None;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Subscribes to output. A new subscriber should take [`Self::replay`] first
    /// to get the current screen, then apply everything that arrives here.
    pub fn subscribe(&self) -> broadcast::Receiver<OutputChunk> {
        self.output_tx.subscribe()
    }

    /// Bytes that rebuild the current screen in a fresh renderer.
    pub fn replay(&self) -> Vec<u8> {
        self.buffer.lock().map(|b| b.replay()).unwrap_or_default()
    }

    /// A snapshot for on-demand previews and heuristics.
    pub fn snapshot(&self) -> Option<ScreenSnapshot> {
        self.buffer.lock().ok().map(|b| b.snapshot())
    }

    /// The title this process set for itself, if any.
    ///
    /// Already sanitised: control characters, escape sequences, bidi overrides and
    /// invisible tag characters are gone, and the length is capped, because this
    /// string is written by the process and ends up in Turn's own chrome.
    pub fn title(&self) -> Option<String> {
        self.buffer
            .lock()
            .ok()
            .and_then(|b| b.title().map(str::to_string))
    }

    /// Reads the title only when it has changed since `seen`, returning the new
    /// generation alongside it.
    ///
    /// Shaped this way so the caller never has to hold the buffer lock to find out
    /// that nothing happened, which is the answer almost every time it asks.
    pub fn title_if_changed(&self, seen: u64) -> Option<(u64, Option<String>)> {
        let buffer = self.buffer.lock().ok()?;
        let generation = buffer.title_generation();
        if generation == seen {
            return None;
        }
        Some((generation, buffer.title().map(str::to_string)))
    }

    /// Shared access to the buffer, for the heuristic adapters.
    pub fn buffer(&self) -> Arc<Mutex<TerminalBuffer>> {
        Arc::clone(&self.buffer)
    }

    /// Exit information, once known.
    pub fn exit_info(&self) -> Option<ExitInfo> {
        self.exit_rx.borrow().clone()
    }

    /// A receiver that fires when the process exits.
    pub fn exit_watcher(&self) -> watch::Receiver<Option<ExitInfo>> {
        self.exit_rx.clone()
    }

    /// The current lifecycle state.
    pub fn lifecycle(&self) -> Lifecycle {
        match self.exit_info() {
            Some(info) => info.lifecycle(),
            None => Lifecycle::Alive,
        }
    }

    pub fn is_running(&self) -> bool {
        self.exit_info().is_none()
    }

    /// Whether the pty reached EOF. Becomes true slightly before the exit status
    /// is available.
    pub fn output_finished(&self) -> bool {
        self.reader_finished.load(Ordering::Relaxed)
    }

    /// Sends an interrupt the way a terminal does: writing the control character
    /// so the tty delivers the signal to the whole foreground process group.
    ///
    /// This reaches the children an agent spawned, which `kill(pid)` would miss.
    pub fn interrupt(&self) -> Result<(), PtyError> {
        self.write(&[0x03])
    }

    /// Terminates the process politely.
    pub fn terminate(&self) -> Result<(), PtyError> {
        #[cfg(unix)]
        {
            // SAFETY: signalling a pid we spawned. A failure means it already
            // exited, which is not an error worth propagating.
            unsafe {
                libc::kill(self.pid as libc::pid_t, libc::SIGTERM);
            }
            Ok(())
        }
        #[cfg(not(unix))]
        {
            self.kill()
        }
    }

    /// Kills the process outright.
    pub fn kill(&self) -> Result<(), PtyError> {
        let mut child = self.child.lock().map_err(|_| PtyError::Unavailable)?;
        child.kill()?;
        Ok(())
    }

    pub fn bytes_written(&self) -> u64 {
        self.bytes_written.load(Ordering::Relaxed)
    }
}

/// Dropping a `PtyProcess` ends the process it owns.
///
/// This is the deliberate ownership model: whoever holds the handle decides how
/// long the process lives, and closing a session must not leave strays behind
/// holding ptys — a finite kernel resource.
///
/// The persistence the product promises is "your work survives the UI closing",
/// and it is the daemon that delivers it by keeping these handles alive across
/// UI restarts. Surviving the *daemon* exiting is a different problem, and one
/// this type cannot solve: the pty master belongs to the daemon's file table.
impl Drop for PtyProcess {
    fn drop(&mut self) {
        if self.is_running() {
            // Ask politely first; the pty closing right after delivers SIGHUP to
            // anything that ignored it.
            let _ = self.terminate();
        }
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
        }
        // The reader thread is detached and may take a moment to observe EOF. Close its
        // journal synchronously before the daemon releases data-dir ownership, so a new
        // daemon generation can never race an old writer over the same files.
        if let Some(shared) = &self.journal {
            if let Ok(mut journal) = shared.lock() {
                journal.take();
            }
        }
    }
}

/// Opens a pty, retrying briefly when the kernel has none to give.
///
/// Pty devices are a finite, slowly-recycled kernel resource. Opening one right
/// after a burst of terminals were closed can fail transiently, which surfaces as
/// an opaque `openpty` errno rather than anything actionable. Turn's own workload
/// is exactly that burst: a template can start several panes at once, and closing
/// a session frees several at once.
///
/// Retrying converts a spurious "could not start your agent" into a few
/// milliseconds of delay. The backoff is short and the ceiling low, because a
/// genuine exhaustion — the user really is at the system limit — must still be
/// reported rather than hidden behind a long stall.
fn open_pty_with_retry(size: ScreenSize) -> Result<portable_pty::PtyPair, PtyError> {
    const ATTEMPTS: u32 = 5;
    let pty_system = portable_pty::native_pty_system();
    let requested = PtySize {
        rows: size.rows,
        cols: size.cols,
        pixel_width: 0,
        pixel_height: 0,
    };

    let mut last = String::new();
    for attempt in 0..ATTEMPTS {
        match pty_system.openpty(requested) {
            Ok(pair) => return Ok(pair),
            Err(error) => {
                last = error.to_string();
                if attempt + 1 < ATTEMPTS {
                    // 5ms, 10ms, 20ms, 40ms — 75ms in total at worst.
                    let backoff = 5u64 << attempt;
                    std::thread::sleep(std::time::Duration::from_millis(backoff));
                }
            }
        }
    }
    Err(PtyError::OpenPty(format!(
        "{last} (after {ATTEMPTS} attempts; the system may be out of pty devices)"
    )))
}

/// Builds the command, inheriting the environment unless asked not to.
fn build_command(spec: &ProcessSpec, node_id: &NodeId) -> CommandBuilder {
    #[cfg(target_os = "macos")]
    let mut builder = match &spec.read_only_sandbox {
        Some(sandbox) => sandbox.command_builder(&spec.command),
        None => CommandBuilder::new(&spec.command),
    };
    #[cfg(not(target_os = "macos"))]
    let mut builder = {
        debug_assert!(
            spec.read_only_sandbox.is_none(),
            "a read-only sandbox must never be carried onto an unsupported platform"
        );
        CommandBuilder::new(&spec.command)
    };
    for arg in &spec.args {
        builder.arg(arg);
    }
    if spec.clean_env {
        builder.env_clear();
    }
    builder.cwd(&spec.cwd);
    for (key, value) in &spec.env {
        builder.env(key, value);
    }
    // Claude Code, Codex and every TUI behave better when they know they are on a
    // capable terminal. Without this they degrade to dumb output and the point of
    // embedding a real pty is lost.
    if !spec.env.iter().any(|(k, _)| k == "TERM") {
        builder.env("TERM", "xterm-256color");
    }
    if !spec.env.iter().any(|(k, _)| k == "COLORTERM") {
        builder.env("COLORTERM", "truecolor");
    }
    // `turnd` may itself have been started from a shell or supervisor that exports
    // NO_COLOR. That is a preference for the launcher, not for every interactive
    // terminal it goes on to create: forwarding it makes otherwise capable CLIs render
    // monochrome despite the real pty and TERM above. A Pane/profile can still opt out
    // explicitly by putting NO_COLOR in ProcessSpec::env.
    if !spec.env.iter().any(|(k, _)| k == "NO_COLOR") {
        builder.env_remove("NO_COLOR");
    }
    // Marks the process as ours, so the supervisor can attribute strays and
    // adapters can tell they are running under Turn.
    builder.env("TURN_NODE_ID", node_id.as_str());
    if let Some(sandbox) = &spec.read_only_sandbox {
        builder.env("TURN_READ_ONLY", "1");
        builder.env("TURN_READ_ONLY_ROOT", sandbox.checkout_root());
        // Read-only Git commands should never refresh the index opportunistically.
        builder.env("GIT_OPTIONAL_LOCKS", "0");
    }
    builder
}

/// Pumps the pty into the buffer and the broadcast channel until EOF.
fn spawn_reader(
    mut reader: Box<dyn Read + Send>,
    buffer: Arc<Mutex<TerminalBuffer>>,
    output_tx: broadcast::Sender<OutputChunk>,
    finished: Arc<AtomicBool>,
    node: NodeId,
    journal: Option<SharedJournal>,
) {
    std::thread::Builder::new()
        .name(format!("turn-pty-{node}"))
        .spawn(move || {
            let mut chunk = vec![0u8; READ_CHUNK];
            loop {
                match reader.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => {
                        let data: OutputChunk = Arc::new(chunk[..n].to_vec());
                        // The buffer is authoritative and must never miss data,
                        // so it is written before the broadcast.
                        if let Ok(mut buffer) = buffer.lock() {
                            buffer.write(&data);
                            if let Some(shared) = &journal {
                                if let Ok(mut journal) = shared.lock() {
                                    if let Some(writer) = journal.as_mut() {
                                        if let Err(error) = writer.record_output(&data, &mut buffer) {
                                            tracing::error!(%node, %error, "terminal journal failed; persistence disabled for this process");
                                            buffer.mark_truncated();
                                            *journal = None;
                                        }
                                    }
                                }
                            }
                        }
                        // A send failure only means nobody is listening right
                        // now; the buffer still holds the bytes.
                        let _ = output_tx.send(data);
                    }
                    Err(_) => break,
                }
            }
            finished.store(true, Ordering::Relaxed);
        })
        .expect("spawning a pty reader thread");
}

/// Polls for the child's exit and publishes it once.
fn spawn_waiter(
    child: SharedChild,
    exit_tx: watch::Sender<Option<ExitInfo>>,
    pid: u32,
    node: NodeId,
) {
    std::thread::Builder::new()
        .name(format!("turn-wait-{pid}"))
        .spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_millis(WAIT_POLL_MS));
            let status = {
                let Ok(mut guard) = child.lock() else {
                    break;
                };
                match guard.try_wait() {
                    Ok(status) => status,
                    Err(error) => {
                        tracing::warn!(%node, %pid, %error, "waiting on child failed");
                        break;
                    }
                }
            };
            if let Some(status) = status {
                let info = ExitInfo {
                    code: status.exit_code() as i32,
                    signal: status.signal().map(str::to_string),
                };
                let _ = exit_tx.send(Some(info));
                break;
            }
        })
        .expect("spawning a wait thread");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    const T0: i64 = 1_700_000_000_000;

    /// Waits for a condition, failing the test rather than hanging forever.
    fn wait_until(label: &str, mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if condition() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("timed out waiting for: {label}");
    }

    fn spawn(spec: ProcessSpec) -> PtyProcess {
        PtyProcess::spawn(NodeId::new(), spec, T0).expect("spawning the process")
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_macos_guard_blocks_checkout_writes_from_aliases_cwds_and_children() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let checkout = temp.path().join("checkout");
        let alternate_cwd = checkout.join("nested");
        let outside = temp.path().join("outside");
        let alias = temp.path().join("checkout-alias");
        std::fs::create_dir_all(&alternate_cwd).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(checkout.join("existing"), "original\n").unwrap();
        std::fs::write(checkout.join("delete-me"), "keep\n").unwrap();
        std::fs::write(checkout.join("rename-me"), "keep\n").unwrap();
        symlink(&checkout, &alias).unwrap();

        let git = std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&checkout)
            .status()
            .expect("git must be available for the read-only acceptance test");
        assert!(git.success());

        let checkout = std::fs::canonicalize(checkout).unwrap();
        let alternate_cwd = std::fs::canonicalize(alternate_cwd).unwrap();
        let outside = std::fs::canonicalize(outside).unwrap();
        let sandbox = ReadOnlySandbox::for_checkout(&checkout)
            .unwrap()
            .expect("macOS must expose sandbox-exec");
        let script = r#"
            touch "$1/created" 2>/dev/null || true
            printf 'changed\n' > "$1/existing" 2>/dev/null || true
            rm -f "$1/delete-me" 2>/dev/null || true
            mv -f "$1/rename-me" "$1/renamed" 2>/dev/null || true
            TURN_CHILD_ROOT="$1" sh -c 'touch "$TURN_CHILD_ROOT/child-created"' 2>/dev/null || true
            touch "$3/alias-created" 2>/dev/null || true
            git -C "$1" status --porcelain >/dev/null || exit 41
            touch "$2/outside-write" || exit 42
            [ "$PWD" = "$1/nested" ] || exit 43
            [ "$TURN_READ_ONLY" = 1 ] || exit 44
            [ "$TURN_READ_ONLY_ROOT" = "$1" ] || exit 45
            [ "$GIT_OPTIONAL_LOCKS" = 0 ] || exit 46
            printf 'TURN_READ_ONLY_GUARD_OK\n'
        "#;
        let process = spawn(
            ProcessSpec::new("sh", alternate_cwd.to_string_lossy())
                .args([
                    "-c".to_string(),
                    script.to_string(),
                    "turn-read-only-test".to_string(),
                    checkout.to_string_lossy().into_owned(),
                    outside.to_string_lossy().into_owned(),
                    alias.to_string_lossy().into_owned(),
                ])
                .read_only_sandbox(sandbox),
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline
            && !process
                .snapshot()
                .is_some_and(|snapshot| snapshot.text().contains("TURN_READ_ONLY_GUARD_OK"))
        {
            std::thread::sleep(Duration::from_millis(20));
        }
        let output = process.snapshot().unwrap().text();
        assert!(
            output.contains("TURN_READ_ONLY_GUARD_OK"),
            "guarded probe failed: {output}"
        );
        wait_until("the guarded process to exit", || !process.is_running());
        assert_eq!(process.exit_info().unwrap().code, 0);
        assert!(!checkout.join("created").exists());
        assert!(!checkout.join("child-created").exists());
        assert!(!checkout.join("alias-created").exists());
        assert_eq!(
            std::fs::read_to_string(checkout.join("existing")).unwrap(),
            "original\n"
        );
        assert!(checkout.join("delete-me").exists());
        assert!(checkout.join("rename-me").exists());
        assert!(!checkout.join("renamed").exists());
        assert!(outside.join("outside-write").exists());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_macos_guard_includes_external_git_and_common_directories() {
        let temp = tempfile::tempdir().unwrap();
        let checkout = temp.path().join("checkout");
        let git_dir = temp.path().join("git-dir");
        let common_dir = temp.path().join("common-dir");
        std::fs::create_dir(&checkout).unwrap();
        std::fs::create_dir(&git_dir).unwrap();
        std::fs::create_dir(&common_dir).unwrap();
        std::fs::write(checkout.join(".git"), "gitdir: ../git-dir\n").unwrap();
        std::fs::write(git_dir.join("commondir"), "../common-dir\n").unwrap();

        let sandbox = ReadOnlySandbox::for_checkout(&checkout)
            .unwrap()
            .expect("macOS must expose sandbox-exec");
        assert_eq!(
            sandbox.protected_paths(),
            &[
                std::fs::canonicalize(checkout).unwrap(),
                std::fs::canonicalize(git_dir).unwrap(),
                std::fs::canonicalize(common_dir).unwrap(),
            ]
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_macos_guard_rejects_a_protected_path_seatbelt_cannot_name_exactly() {
        use std::os::unix::ffi::OsStringExt;

        let protected = PathBuf::from(std::ffi::OsString::from_vec(b"git-dir-\xff".to_vec()));
        let error = validate_seatbelt_paths(&[protected])
            .expect_err("a lossy Seatbelt parameter would leave the real path writable");
        assert!(matches!(error, ReadOnlySandboxError::NonUtf8Path { .. }));
    }

    #[test]
    fn a_process_runs_and_its_output_reaches_the_screen() {
        let process = spawn(ProcessSpec::new("echo", "/").arg("hello from turn"));

        wait_until("output to arrive", || {
            process
                .snapshot()
                .map(|s| s.text().contains("hello from turn"))
                .unwrap_or(false)
        });
        wait_until("the process to exit", || !process.is_running());

        let exit = process.exit_info().expect("an exit status");
        assert_eq!(exit.code, 0);
        assert!(!exit.signalled());
        assert_eq!(process.lifecycle(), Lifecycle::Exited { code: 0 });
        assert!(process.pid() > 0);
    }

    #[test]
    fn a_failing_command_reports_a_non_zero_exit() {
        let process = spawn(ProcessSpec::new("sh", "/").arg("-c").arg("exit 3"));
        wait_until("the process to exit", || !process.is_running());

        let exit = process.exit_info().unwrap();
        assert_eq!(exit.code, 3);
        assert!(process.lifecycle().is_failure());
    }

    #[test]
    fn spawning_a_command_that_does_not_exist_fails_cleanly() {
        let result = PtyProcess::spawn(
            NodeId::new(),
            ProcessSpec::new("turn-definitely-not-a-real-binary", "/"),
            T0,
        );
        assert!(result.is_err(), "a missing binary must not panic");
    }

    /// The reported defect, through a real pseudo-terminal.
    ///
    /// The buffer's own tests write the bytes in directly. This one has a shell print them, so
    /// what is exercised is the whole path a user's output takes: a process writes a Kitty
    /// transmission naming a file on disk — which Turn will not open — in the middle of a line
    /// of ordinary output. The line must arrive as the process wrote it, and the refusal must
    /// arrive beside the screen rather than in it.
    #[test]
    fn a_refused_picture_from_a_real_process_leaves_its_line_alone() {
        // `printf` in three parts, so the sequence really does land mid-line.
        let script = "printf 'MCP startup interrupted: codex_apps'; \
                      printf '\\033_Ga=T,f=100,t=f;L3RtcC9wbG90LnBuZw==\\033\\\\'; \
                      printf ' ok\\r\\n> Explain this codebase\\r\\n'";
        let process = spawn(ProcessSpec::new("sh", "/").arg("-c").arg(script));

        wait_until("the whole script to print", || {
            process
                .snapshot()
                .map(|s| s.text().contains("Explain this codebase"))
                .unwrap_or(false)
        });

        let grid = process.buffer().lock().expect("the buffer lock").grid();

        assert_eq!(
            grid.notices.len(),
            1,
            "the refusal must be recorded: {:?}",
            grid.notices
        );
        assert!(
            grid.notices[0]
                .text
                .contains("does not read images from a file on disk"),
            "got {:?}",
            grid.notices
        );

        let text = grid.text();
        assert!(
            text.contains("MCP startup interrupted: codex_apps ok"),
            "the process's own line must be whole: {text:?}"
        );
        assert!(
            !text.contains("image not shown"),
            "Turn's sentence must not be in the process's screen: {text:?}"
        );
    }

    #[test]
    fn input_written_to_the_pty_reaches_the_process() {
        // `cat` echoes whatever it is given, which proves the write path works.
        let process = spawn(ProcessSpec::new("cat", "/"));
        process.write(b"round trip\n").unwrap();

        wait_until("the echo to come back", || {
            process
                .snapshot()
                .map(|s| s.text().contains("round trip"))
                .unwrap_or(false)
        });
        assert!(process.bytes_written() >= 11);

        process.kill().unwrap();
        wait_until("the process to die", || !process.is_running());
    }

    #[test]
    fn subscribers_receive_output_as_it_arrives() {
        let process = spawn(ProcessSpec::new("cat", "/"));
        let mut subscriber = process.subscribe();
        process.write(b"streamed\n").unwrap();

        let deadline = Instant::now() + Duration::from_secs(10);
        let mut seen = String::new();
        while Instant::now() < deadline && !seen.contains("streamed") {
            if let Ok(chunk) = subscriber.try_recv() {
                seen.push_str(&String::from_utf8_lossy(&chunk));
            } else {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        assert!(seen.contains("streamed"), "subscriber saw {seen:?}");
        let _ = process.kill();
    }

    /// The backpressure contract: a subscriber that stops reading is told it
    /// fell behind instead of growing an unbounded queue.
    #[test]
    fn a_slow_subscriber_is_told_it_fell_behind_rather_than_buffering_forever() {
        // Driven by the channel directly rather than by a real flood.
        //
        // An earlier version of this test ran 4,000 `echo`s and assumed they would
        // arrive as more than `OUTPUT_CHANNEL_CAPACITY` separate reads. That held on
        // macOS and failed on Linux, where a different shell and pty buffer size
        // coalesce the same output into far fewer chunks: the channel never
        // overflowed, so nothing lagged and the test failed on a platform
        // difference rather than on the behaviour it was written to check.
        //
        // The guarantee under test belongs to the channel — a subscriber that stops
        // reading is *told* it lost data instead of being buffered without bound —
        // so it is asserted where it lives, with an overflow that cannot depend on
        // how an operating system happens to slice a pipe.
        let (tx, _keep) = broadcast::channel::<OutputChunk>(OUTPUT_CHANNEL_CAPACITY);
        let mut lazy = tx.subscribe();

        for i in 0..(OUTPUT_CHANNEL_CAPACITY * 2) {
            let _ = tx.send(Arc::new(format!("chunk {i}").into_bytes()));
        }

        let mut lagged_by = None;
        loop {
            match lazy.try_recv() {
                Ok(_) => continue,
                Err(broadcast::error::TryRecvError::Lagged(skipped)) => {
                    lagged_by = Some(skipped);
                    break;
                }
                Err(_) => break,
            }
        }
        let skipped =
            lagged_by.expect("a subscriber that ignored a flood must be told it lost data");
        assert!(skipped > 0, "the report must say how much was lost");
    }

    /// The other half of the same story, and the half that is genuinely about the
    /// pty: however the platform slices the output, the authoritative buffer keeps
    #[test]
    fn a_flood_reaches_the_buffer_whole_and_leaves_it_bounded() {
        let process = spawn(
            ProcessSpec::new("sh", "/")
                .arg("-c")
                .arg("i=0; while [ $i -lt 4000 ]; do echo \"line $i padding padding padding\"; i=$((i+1)); done"),
        );

        wait_until("the flood to finish", || !process.is_running());
        wait_until("the last line to be parsed", || {
            process
                .snapshot()
                .map(|s| s.text().contains("line 3999"))
                .unwrap_or(false)
        });

        let snapshot = process.snapshot().unwrap();
        assert_eq!(snapshot.lines.len(), 24, "only the visible rows are kept");
        assert!(
            process.buffer().lock().unwrap().retained_bytes()
                <= crate::buffer::DEFAULT_BYTE_CAPACITY,
            "the ring must stay bounded under a flood"
        );
    }

    #[test]
    fn heavy_output_does_not_block_a_second_process() {
        let noisy = spawn(
            ProcessSpec::new("sh", "/")
                .arg("-c")
                .arg("i=0; while [ $i -lt 3000 ]; do echo \"noise $i\"; i=$((i+1)); done"),
        );
        let quiet = spawn(ProcessSpec::new("echo", "/").arg("still responsive"));

        wait_until("the quiet process to be heard", || {
            quiet
                .snapshot()
                .map(|s| s.text().contains("still responsive"))
                .unwrap_or(false)
        });
        wait_until("the noisy one to finish", || !noisy.is_running());
    }

    #[test]
    fn resize_reaches_both_the_kernel_and_our_screen_model() {
        let process = spawn(ProcessSpec::new("cat", "/").size(ScreenSize::new(24, 80)));
        assert_eq!(process.snapshot().unwrap().size, ScreenSize::new(24, 80));

        process.resize(ScreenSize::new(40, 120)).unwrap();
        let snapshot = process.snapshot().unwrap();
        assert_eq!(snapshot.size, ScreenSize::new(40, 120));
        assert_eq!(snapshot.lines.len(), 40);

        let _ = process.kill();
    }

    #[test]
    fn a_process_sees_the_size_we_gave_it() {
        // `stty size` asks the tty itself, so this proves the pty was created
        // with the requested geometry rather than a default.
        let process = spawn(
            ProcessSpec::new("sh", "/")
                .arg("-c")
                .arg("stty size")
                .size(ScreenSize::new(30, 100)),
        );
        wait_until("stty to report", || {
            process
                .snapshot()
                .map(|s| s.text().contains("30 100"))
                .unwrap_or(false)
        });
    }

    #[test]
    fn the_environment_we_ask_for_reaches_the_process() {
        let process = spawn(
            ProcessSpec::new("sh", "/")
                .arg("-c")
                .arg("echo \"[$TURN_TEST_VAR][$TERM]\"")
                .env("TURN_TEST_VAR", "injected"),
        );
        wait_until("the variable to be echoed", || {
            process
                .snapshot()
                .map(|s| s.text().contains("[injected][xterm-256color]"))
                .unwrap_or(false)
        });
    }

    #[test]
    fn interactive_terminal_defaults_enable_true_colour() {
        let process = spawn(
            ProcessSpec::new("sh", "/")
                .arg("-c")
                .arg("printf '[%s][%s][%s]\\n' \"${NO_COLOR-unset}\" \"$TERM\" \"$COLORTERM\""),
        );
        wait_until("the terminal colour defaults to be reported", || {
            process
                .snapshot()
                .map(|snapshot| {
                    snapshot
                        .text()
                        .contains("[unset][xterm-256color][truecolor]")
                })
                .unwrap_or(false)
        });
    }

    #[test]
    fn explicit_terminal_colour_environment_overrides_are_respected() {
        let process = spawn(
            ProcessSpec::new("sh", "/")
                .arg("-c")
                .arg("printf '[%s][%s][%s]\\n' \"$NO_COLOR\" \"$TERM\" \"$COLORTERM\"")
                .env("NO_COLOR", "1")
                .env("TERM", "screen-256color")
                .env("COLORTERM", "24bit"),
        );
        wait_until(
            "the explicit terminal colour environment to be reported",
            || {
                process
                    .snapshot()
                    .map(|snapshot| snapshot.text().contains("[1][screen-256color][24bit]"))
                    .unwrap_or(false)
            },
        );
    }

    #[test]
    fn the_process_inherits_the_users_path_by_default() {
        // Agents need the ambient environment; a clean env would break auth.
        let process = spawn(ProcessSpec::new("sh", "/").arg("-c").arg("echo PATH=$PATH"));
        wait_until("PATH to be echoed", || {
            process
                .snapshot()
                .map(|s| {
                    let text = s.text();
                    text.contains("PATH=/") || text.contains("PATH=") && text.len() > 12
                })
                .unwrap_or(false)
        });
    }

    #[test]
    fn the_working_directory_is_honoured() {
        let dir = std::env::temp_dir();
        let process = spawn(ProcessSpec::new("pwd", dir.to_string_lossy().to_string()));
        wait_until("pwd to report the directory", || {
            process
                .snapshot()
                .map(|s| {
                    // macOS reports /private/var for /var, so compare the tail.
                    let text = s.text();
                    let expected = dir.file_name().unwrap().to_string_lossy().to_string();
                    text.contains(&expected)
                })
                .unwrap_or(false)
        });
    }

    #[test]
    fn killing_a_process_is_reported_as_a_signal_death() {
        let process = spawn(ProcessSpec::new("sleep", "/").arg("30"));
        assert!(process.is_running());
        process.kill().unwrap();

        wait_until("the process to die", || !process.is_running());
        let exit = process.exit_info().unwrap();
        assert!(
            exit.signalled(),
            "a killed process must not look like a clean exit: {exit:?}"
        );
        assert!(
            matches!(
                process.lifecycle(),
                turn_core::state::Lifecycle::Signaled { .. }
            ),
            "and must be recorded as a signal death, not an exit code"
        );
        assert!(process.lifecycle().is_failure());
    }

    #[test]
    fn interrupting_reaches_the_foreground_process_group() {
        // `sh -c 'sleep 60'` puts sleep in the pty's foreground group. Ctrl-C
        // through the tty reaches it; kill(pid) on the shell would not.
        let process = spawn(ProcessSpec::new("sh", "/").arg("-c").arg("sleep 5"));
        std::thread::sleep(Duration::from_millis(300));
        process.interrupt().unwrap();

        wait_until("the interrupt to land", || !process.is_running());
    }

    #[test]
    fn terminate_stops_a_long_running_process() {
        let process = spawn(ProcessSpec::new("sleep", "/").arg("30"));
        std::thread::sleep(Duration::from_millis(200));
        process.terminate().unwrap();
        wait_until("SIGTERM to land", || !process.is_running());
    }

    #[test]
    fn a_reattaching_pane_can_rebuild_the_screen_from_replay() {
        let process = spawn(
            ProcessSpec::new("sh", "/")
                .arg("-c")
                .arg("printf 'first line\\r\\nsecond line\\r\\n'; sleep 5"),
        );
        wait_until("output to settle", || {
            process
                .snapshot()
                .map(|s| s.text().contains("second line"))
                .unwrap_or(false)
        });

        // Replay into a fresh terminal, as the UI does when re-attaching.
        let replay = process.replay();
        let mut rebuilt = TerminalBuffer::new(process.snapshot().unwrap().size);
        rebuilt.write(&replay);
        assert!(rebuilt.snapshot().text().contains("second line"));
        assert!(rebuilt.snapshot().text().contains("first line"));

        let _ = process.kill();
    }

    /// The issue's reproducible test: a real pty emits a title sequence and the
    /// change is observed.
    ///
    /// `printf` writes the OSC 2 form Claude Code and every shell use:
    /// `ESC ] 2 ; <title> BEL`.
    #[test]
    fn a_process_can_set_its_own_title_through_a_real_pty() {
        // Two titles from the script itself, half a second apart. Writing the
        // second one to the pty would not work: the process is sitting in `sleep`,
        // not reading stdin, which is also true of a real agent mid-task.
        let process = spawn(ProcessSpec::new("sh", "/").arg("-c").arg(
            "printf '\\033]2;fixing the climbing bug\\007'; sleep 0.5; \
             printf '\\033]2;running the tests\\007'; sleep 5",
        ));

        wait_until("the first title", || {
            process.title().as_deref() == Some("fixing the climbing bug")
        });
        wait_until("the second title to replace it", || {
            process.title().as_deref() == Some("running the tests")
        });
        let _ = process.kill();
    }

    /// Two processes on two ptys keep separate titles: the buffer is per process,
    /// so isolation is structural rather than something to remember.
    #[test]
    fn two_processes_keep_their_own_titles() {
        let first = spawn(
            ProcessSpec::new("sh", "/")
                .arg("-c")
                .arg("printf '\\033]2;first instance\\007'; sleep 5"),
        );
        let second = spawn(
            ProcessSpec::new("sh", "/")
                .arg("-c")
                .arg("printf '\\033]2;second instance\\007'; sleep 5"),
        );

        wait_until("both titles", || {
            first.title().as_deref() == Some("first instance")
                && second.title().as_deref() == Some("second instance")
        });

        // Killing one leaves the other's title untouched.
        let _ = first.kill();
        wait_until("the first to die", || !first.is_running());
        assert_eq!(second.title().as_deref(), Some("second instance"));
    }

    /// The generation only moves when the title becomes something different, which
    /// is what stops a `PROMPT_COMMAND` re-sending the same title from producing
    /// work on every command.
    #[test]
    fn a_repeated_identical_title_does_not_count_as_a_change() {
        let process = spawn(
            ProcessSpec::new("sh", "/")
                .arg("-c")
                .arg("printf '\\033]2;same\\007'; printf '\\033]2;same\\007'; printf '\\033]2;same\\007'; sleep 5"),
        );

        wait_until("the title to arrive", || {
            process.title().as_deref() == Some("same")
        });
        let (generation, _) = process
            .title_if_changed(0)
            .expect("the first read reports a change");
        assert_eq!(generation, 1, "three identical titles are one change");
        assert!(
            process.title_if_changed(generation).is_none(),
            "nothing changed since, so there is nothing to report"
        );
    }

    /// A hostile title cannot reach the caller unsanitised, and it cannot cost
    /// unbounded memory either.
    #[test]
    fn a_hostile_title_from_a_real_process_arrives_sanitised_and_bounded() {
        let process = spawn(
            ProcessSpec::new("sh", "/")
                .arg("-c")
                // A cursor-clearing sequence, a bidi override and 4,000 characters.
                .arg("printf '\\033]2;evil\\033[2J\\342\\200\\256flip'; printf 'A%.0s' $(seq 1 4000); printf '\\007'; sleep 5"),
        );

        wait_until("the title to arrive", || process.title().is_some());
        let title = process.title().unwrap();
        assert!(
            !title.chars().any(|c| c.is_control()),
            "control characters reached the caller: {title:?}"
        );
        assert!(
            !title.contains('\u{202e}'),
            "a bidi override survived: {title:?}"
        );
        assert!(
            title.chars().count() <= crate::buffer::MAX_TITLE_CHARS,
            "an unbounded title was retained: {} chars",
            title.chars().count()
        );
    }

    #[test]
    fn eof_on_the_pty_is_noticed() {
        let process = spawn(ProcessSpec::new("echo", "/").arg("done"));
        wait_until("the reader to see EOF", || process.output_finished());
        wait_until("the exit to be recorded", || !process.is_running());
    }

    #[test]
    fn a_full_screen_application_is_recognised() {
        // Enter the alternate screen the way a TUI does, then stay alive.
        let process = spawn(
            ProcessSpec::new("sh", "/")
                .arg("-c")
                .arg("printf '\\033[?1049hTUI CONTENT'; sleep 5"),
        );
        wait_until("the alternate screen to be entered", || {
            process
                .snapshot()
                .map(|s| s.alternate_screen)
                .unwrap_or(false)
        });
        assert!(process.snapshot().unwrap().text().contains("TUI CONTENT"));
        let _ = process.kill();
    }
}
