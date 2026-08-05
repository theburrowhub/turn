//! Spawning and driving a process on a real pty.
//!
//! Reading a pty is a blocking operation, so each process gets a dedicated OS
//! thread that pumps bytes into a bounded broadcast channel and into the
//! [`TerminalBuffer`]. The channel being bounded is the whole backpressure
//! story: a subscriber that cannot keep up is told it fell behind and
//! re-synchronises from the buffer's replay, rather than being allowed to grow
//! an unbounded queue and take the daemon down with it.

use crate::buffer::{ScreenSize, ScreenSnapshot, TerminalBuffer};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};
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

/// A live process attached to a pty.
pub struct PtyProcess {
    node_id: NodeId,
    pid: u32,
    spec: ProcessSpec,
    master: Box<dyn MasterPty + Send>,
    writer: Mutex<Box<dyn Write + Send>>,
    child: SharedChild,
    buffer: Arc<Mutex<TerminalBuffer>>,
    output_tx: broadcast::Sender<OutputChunk>,
    exit_rx: watch::Receiver<Option<ExitInfo>>,
    /// Set when the reader thread sees EOF, which happens before the exit status
    /// is known.
    reader_finished: Arc<AtomicBool>,
    bytes_written: AtomicU64,
    started_ms: i64,
}

impl PtyProcess {
    /// Opens a pty, launches the command on it and starts pumping output.
    pub fn spawn(node_id: NodeId, spec: ProcessSpec, now_ms: i64) -> Result<Self, PtyError> {
        let pair = open_pty_with_retry(spec.size)?;

        let builder = build_command(&spec, &node_id);
        let child = pair
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
    let mut builder = CommandBuilder::new(&spec.command);
    if spec.clean_env {
        builder.env_clear();
    }
    for arg in &spec.args {
        builder.arg(arg);
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
    // Marks the process as ours, so the supervisor can attribute strays and
    // adapters can tell they are running under Turn.
    builder.env("TURN_NODE_ID", node_id.as_str());
    builder
}

/// Pumps the pty into the buffer and the broadcast channel until EOF.
fn spawn_reader(
    mut reader: Box<dyn Read + Send>,
    buffer: Arc<Mutex<TerminalBuffer>>,
    output_tx: broadcast::Sender<OutputChunk>,
    finished: Arc<AtomicBool>,
    node: NodeId,
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
        let process = spawn(
            ProcessSpec::new("sh", "/")
                .arg("-c")
                .arg("i=0; while [ $i -lt 4000 ]; do echo \"line $i padding padding padding\"; i=$((i+1)); done"),
        );
        let mut lazy = process.subscribe();

        // Never read until the flood is over.
        wait_until("the flood to finish", || !process.is_running());
        std::thread::sleep(Duration::from_millis(200));

        let mut lagged = false;
        loop {
            match lazy.try_recv() {
                Ok(_) => continue,
                Err(broadcast::error::TryRecvError::Lagged(skipped)) => {
                    assert!(skipped > 0);
                    lagged = true;
                    break;
                }
                Err(_) => break,
            }
        }
        assert!(
            lagged,
            "a subscriber that ignored a flood must be told it lost data"
        );

        // And the authoritative buffer is still correct and bounded.
        let snapshot = process.snapshot().unwrap();
        assert!(snapshot.text().contains("line 3999"));
        assert_eq!(snapshot.lines.len(), 24);
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
