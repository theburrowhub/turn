//! A daemon on a real socket, and a client that talks to it.
//!
//! The tests drive `turnd` the way a UI does: a unix socket, the newline-delimited JSON
//! from [`turn_proto`], a real store on disk, real ptys and the real loopback hook
//! server. Nothing here fakes a layer of the daemon — the one substitution is which
//! *program* an agent pane launches, and it is explained on [`FakeAgent`].

#![allow(dead_code)]

pub mod agent;

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use turn_agents::{
    AdapterError, AdapterRegistry, AgentAdapter, Capabilities, ClaudeCodeAdapter, EventContext,
    IntegrationLevel, LaunchContext, LaunchPlan,
};
use turn_core::event::TurnEvent;
use turn_core::ids::{NodeId, PaneId, SessionId};
use turn_proto::envelope::{Hello, ServerMessage, Welcome};
use turn_proto::{
    ClientFrame, Grid, LineDecoder, PaneAttachment, PaneStream, ProtoError, PtySize, Request,
    RequestId, Response, ServerEvent, ServerFrame, TreeNodeView,
};
use turnd::{Config, DaemonHandle};

/// How long a test waits for a frame before failing.
///
/// Long enough to absorb a loaded machine, short enough that a hang is a failure rather
/// than a test run nobody watches to the end.
const TIMEOUT: Duration = Duration::from_secs(10);

/// The file the fake adapter leaves its callback URL in.
pub const HOOK_URL_FILE: &str = "hook-url";

/// A daemon started for one test, with its own directory and socket.
pub struct TestDaemon {
    dir: tempfile::TempDir,
    handle: Option<DaemonHandle>,
    registry: fn() -> AdapterRegistry,
}

impl TestDaemon {
    /// Starts a daemon whose agent panes run [`FakeAgent`].
    pub async fn start() -> Self {
        Self::start_with(fake_registry).await
    }

    /// Starts a daemon with only the built-in adapters, so an unrecognised command runs
    /// as the plain terminal it is.
    pub async fn start_plain() -> Self {
        Self::start_with(AdapterRegistry::with_builtin).await
    }

    pub async fn start_with(registry: fn() -> AdapterRegistry) -> Self {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let config = Config::in_dir(dir.path()).with_registry(registry());
        let handle = turnd::start(config).await.expect("the daemon must start");
        Self {
            dir,
            handle: Some(handle),
            registry,
        }
    }

    /// Starts a daemon over a directory a previous one left behind.
    pub async fn adopt(dir: tempfile::TempDir) -> Self {
        Self::adopt_with(dir, fake_registry).await
    }

    pub async fn adopt_with(dir: tempfile::TempDir, registry: fn() -> AdapterRegistry) -> Self {
        let config = Config::in_dir(dir.path()).with_registry(registry());
        let handle = turnd::start(config)
            .await
            .expect("the daemon must start over an existing store");
        Self {
            dir,
            handle: Some(handle),
            registry,
        }
    }

    /// Stops the daemon and starts another one over the same store, as a restart does.
    pub async fn restart(mut self) -> Self {
        let handle = self.handle.take().expect("a running daemon");
        handle.shutdown().await;
        let config = Config::in_dir(self.dir.path()).with_registry((self.registry)());
        let handle = turnd::start(config)
            .await
            .expect("the daemon must start again over the same store");
        self.handle = Some(handle);
        self
    }

    /// Stops the daemon and hands back its directory, so a test can look at — or
    /// tamper with — what it left on disk before another daemon adopts it.
    pub async fn stop(mut self) -> tempfile::TempDir {
        if let Some(handle) = self.handle.take() {
            handle.shutdown().await;
        }
        self.dir
    }

    pub fn handle(&self) -> &DaemonHandle {
        self.handle.as_ref().expect("a running daemon")
    }

    pub fn socket(&self) -> &Path {
        self.handle().socket_path()
    }

    pub fn data_dir(&self) -> &Path {
        self.dir.path()
    }

    /// Connects a client and completes the handshake.
    pub async fn connect(&self) -> Client {
        Client::connect(self.socket()).await
    }

    pub async fn shutdown(mut self) {
        if let Some(handle) = self.handle.take() {
            handle.shutdown().await;
        }
    }
}

/// A protocol client: framed JSON over the daemon's socket.
pub struct Client {
    stream: UnixStream,
    decoder: LineDecoder,
    events: VecDeque<ServerEvent>,
    next_id: u64,
    /// The screen of each pane attached for cells, kept up to date by applying the
    /// updates the daemon pushes — which is exactly what a real client does, and what
    /// makes an assertion about `screen_text` an assertion about what a user would see.
    screens: HashMap<(SessionId, PaneId), Grid>,
    /// The `seq` each attachment expects next, so a test cannot pass while the daemon
    /// silently skips an update.
    expected_seq: HashMap<(SessionId, PaneId), u64>,
    /// How many updates this client has applied per pane, for the tests that assert
    /// coalescing rather than correctness.
    applied: HashMap<(SessionId, PaneId), usize>,
    pub welcome: Welcome,
}

impl Client {
    pub async fn connect(socket: &Path) -> Self {
        let mut stream = UnixStream::connect(socket)
            .await
            .unwrap_or_else(|error| panic!("could not connect to {}: {error}", socket.display()));

        let hello = ClientFrame::hello(Hello::new("turn-test", "0.1.0"));
        write_frame(&mut stream, &hello).await;

        let mut client = Self {
            stream,
            decoder: LineDecoder::new(),
            events: VecDeque::new(),
            next_id: 1,
            screens: HashMap::new(),
            expected_seq: HashMap::new(),
            applied: HashMap::new(),
            welcome: Welcome::new(turn_proto::PROTOCOL_VERSION, "unknown", 0, 0),
        };
        match client.frame().await.message {
            ServerMessage::Welcome(welcome) => client.welcome = welcome,
            other => panic!("expected a welcome, got {other:?}"),
        }
        client
    }

    /// Sends a request and waits for its answer, failing the test on an error frame.
    pub async fn ask(&mut self, request: Request) -> Response {
        let op = request.op();
        match self.try_ask(request).await {
            Ok(response) => response,
            Err(error) => panic!("{op} failed: {error}"),
        }
    }

    /// Sends a request and returns whatever came back.
    pub async fn try_ask(&mut self, request: Request) -> Result<Response, ProtoError> {
        let expected = request.expected_result();
        let id = RequestId::new(format!("r-{}", self.next_id));
        self.next_id += 1;
        let frame = ClientFrame::request(id.clone(), request);
        write_frame(&mut self.stream, &frame).await;

        loop {
            let frame = self.frame().await;
            match frame.message {
                ServerMessage::Response {
                    id: answered,
                    response,
                } if answered == id => {
                    assert_eq!(
                        response.result_name(),
                        expected,
                        "the daemon answered with a result the request does not promise"
                    );
                    return Ok(response);
                }
                ServerMessage::Error {
                    id: Some(answered),
                    error,
                } if answered == id => return Err(error),
                ServerMessage::Event { event } => self.events.push_back(event),
                other => panic!("unexpected frame while waiting for {id}: {other:?}"),
            }
        }
    }

    /// Sends a hand-written line, for the frames a typed client cannot produce.
    ///
    /// Needed for two real cases: a value `serde_json` will not serialise (an infinite
    /// float becomes `null`), and a line that is not a message at all.
    pub async fn send_raw(&mut self, line: &str) {
        let mut bytes = line.as_bytes().to_vec();
        bytes.push(b'\n');
        self.stream
            .write_all(&bytes)
            .await
            .expect("the socket must stay writable");
        self.stream.flush().await.expect("the socket must flush");
    }

    /// The next error frame, whatever it belongs to.
    pub async fn expect_error(&mut self) -> ProtoError {
        loop {
            match self.frame().await.message {
                ServerMessage::Error { error, .. } => return error,
                ServerMessage::Event { event } => self.events.push_back(event),
                other => panic!("expected an error frame, got {other:?}"),
            }
        }
    }

    /// Waits for a pushed event matching a predicate, reading more if needed.
    pub async fn wait_for<T>(
        &mut self,
        what: &str,
        mut matches: impl FnMut(&ServerEvent) -> Option<T>,
    ) -> T {
        for _ in 0..self.events.len() {
            let event = self.events.pop_front().expect("a buffered event");
            if let Some(found) = matches(&event) {
                return found;
            }
        }
        let deadline = tokio::time::Instant::now() + TIMEOUT;
        loop {
            if tokio::time::Instant::now() >= deadline {
                panic!("timed out waiting for {what}");
            }
            let frame = self.frame().await;
            match frame.message {
                ServerMessage::Event { event } => {
                    if let Some(found) = matches(&event) {
                        return found;
                    }
                }
                other => panic!("unexpected frame while waiting for {what}: {other:?}"),
            }
        }
    }

    /// Attaches a pane as cells and remembers the screen it was handed.
    ///
    /// The default stream, so this is also the path the real client takes.
    pub async fn attach_cells(
        &mut self,
        session_id: &SessionId,
        pane_id: &PaneId,
        size: PtySize,
    ) -> PaneAttachment {
        let attachment = attachment_of(
            self.ask(Request::AttachPane {
                session_id: session_id.clone(),
                pane_id: pane_id.clone(),
                size,
                stream: PaneStream::Cells,
            })
            .await,
        );
        assert_eq!(attachment.stream, PaneStream::Cells);
        let screen = attachment
            .screen
            .clone()
            .expect("a cells attachment carries the screen");
        let key = (session_id.clone(), pane_id.clone());
        self.screens.insert(key.clone(), *screen);
        self.expected_seq.insert(key, attachment.next_seq);
        attachment
    }

    /// The current screen of an attached pane, as this client has applied it.
    pub fn screen(&self, session_id: &SessionId, pane_id: &PaneId) -> &Grid {
        self.screens
            .get(&(session_id.clone(), pane_id.clone()))
            .expect("that pane was never attached for cells")
    }

    /// Applies pushed screen updates until some pane's text contains `needle`.
    ///
    /// Checks the sequence as it goes: an update that skips a number would mean the
    /// client is applying rows to a screen that is already out of date, which is the
    /// one failure this whole mechanism exists to make impossible.
    pub async fn wait_for_screen(&mut self, needle: &str) -> String {
        let deadline = tokio::time::Instant::now() + TIMEOUT;
        loop {
            if let Some(text) = self.screen_containing(needle) {
                return text;
            }
            if tokio::time::Instant::now() >= deadline {
                panic!(
                    "timed out waiting for {needle:?} on any attached screen; saw {:?}",
                    self.screens
                        .values()
                        .map(|screen| screen.text())
                        .collect::<Vec<_>>()
                );
            }
            let (key, seq, update) = self
                .wait_for("a screen update", |event| match event {
                    ServerEvent::PaneScreen {
                        session_id,
                        pane_id,
                        seq,
                        update,
                        ..
                    } => Some(((session_id.clone(), pane_id.clone()), *seq, update.clone())),
                    _ => None,
                })
                .await;
            self.apply_screen(key, seq, update);
        }
    }

    /// Reads whatever screen updates have already arrived, without waiting for more.
    pub async fn poll_screens(&mut self) {
        self.poll_events().await;
        let pending: Vec<ServerEvent> = self.events.drain(..).collect();
        let mut kept = VecDeque::new();
        for event in pending {
            match event {
                ServerEvent::PaneScreen {
                    session_id,
                    pane_id,
                    seq,
                    update,
                    ..
                } => self.apply_screen((session_id, pane_id), seq, update),
                other => kept.push_back(other),
            }
        }
        self.events = kept;
    }

    /// Applies one update to the screen it names, checking the sequence.
    fn apply_screen(
        &mut self,
        key: (SessionId, PaneId),
        seq: u64,
        update: turn_proto::ScreenUpdate,
    ) {
        if let Some(expected) = self.expected_seq.get_mut(&key) {
            assert_eq!(
                seq, *expected,
                "a screen update was skipped: the client would be applying rows to a \
                 screen that is already wrong"
            );
            *expected = seq + 1;
        }
        *self.applied.entry(key.clone()).or_default() += 1;
        let screen = self
            .screens
            .get_mut(&key)
            .expect("an update arrived for a pane this client never attached");
        update
            .apply(screen)
            .expect("every update the daemon sends must apply cleanly");
    }

    /// Forgets what a pane looks like and takes the whole screen again.
    ///
    /// The client half of the resync rule, used after deliberately throwing an update
    /// away.
    pub async fn resync(&mut self, session_id: &SessionId, pane_id: &PaneId) -> Grid {
        let response = self
            .ask(Request::ResyncPane {
                session_id: session_id.clone(),
                pane_id: pane_id.clone(),
            })
            .await;
        let (grid, next_seq) = match response {
            Response::Screen { grid, next_seq, .. } => (*grid, next_seq),
            other => panic!("expected a screen, got {other:?}"),
        };
        let key = (session_id.clone(), pane_id.clone());
        self.screens.insert(key.clone(), grid.clone());
        self.expected_seq.insert(key, next_seq);
        grid
    }

    /// How many screen updates this client has applied to a pane.
    pub fn updates_applied(&self, session_id: &SessionId, pane_id: &PaneId) -> usize {
        self.applied
            .get(&(session_id.clone(), pane_id.clone()))
            .copied()
            .unwrap_or(0)
    }

    /// The text of an attached screen that contains `needle`, if any does.
    fn screen_containing(&self, needle: &str) -> Option<String> {
        self.screens
            .values()
            .map(|screen| screen.text())
            .find(|text| text.contains(needle))
    }

    /// Collects the output pushed for a pane until it contains `needle`.
    pub async fn wait_for_output(&mut self, needle: &str) -> String {
        let mut seen = String::new();
        let deadline = tokio::time::Instant::now() + TIMEOUT;
        loop {
            if tokio::time::Instant::now() >= deadline {
                panic!("timed out waiting for {needle:?} in pane output; saw: {seen:?}");
            }
            let chunk = self
                .wait_for("pane output", |event| match event {
                    ServerEvent::PaneOutput { data, .. } => {
                        Some(String::from_utf8_lossy(data.as_slice()).to_string())
                    }
                    _ => None,
                })
                .await;
            seen.push_str(&chunk);
            if seen.contains(needle) {
                return seen;
            }
        }
    }

    /// Events already received, for assertions about what was *not* pushed.
    pub fn buffered(&self) -> impl Iterator<Item = &ServerEvent> {
        self.events.iter()
    }

    /// Reads whatever has already arrived, without waiting for more.
    pub async fn poll_events(&mut self) {
        let _ = tokio::time::timeout(Duration::from_millis(400), async {
            loop {
                let frame = self.frame().await;
                if let ServerMessage::Event { event } = frame.message {
                    self.events.push_back(event);
                }
            }
        })
        .await;
    }

    async fn frame(&mut self) -> ServerFrame {
        loop {
            if let Some(message) = self.decoder.next_message::<ServerFrame>() {
                return message.expect("every frame the daemon sends must decode");
            }
            let mut buffer = vec![0u8; 64 * 1024];
            let read = tokio::time::timeout(TIMEOUT, self.stream.read(&mut buffer))
                .await
                .expect("timed out waiting for a frame")
                .expect("the socket must stay readable");
            assert!(read > 0, "the daemon closed the connection");
            self.decoder.feed(&buffer[..read]);
        }
    }
}

async fn write_frame(stream: &mut UnixStream, frame: &ClientFrame) {
    let bytes = turn_proto::encode(frame).expect("a frame must encode");
    stream
        .write_all(&bytes)
        .await
        .expect("the socket must stay writable");
    stream.flush().await.expect("the socket must flush");
}

// ---------------------------------------------------------------------- assertions

/// The session summary out of a response.
pub fn session_of(response: Response) -> turn_proto::SessionSummary {
    match response {
        Response::Session { session } => *session,
        other => panic!("expected a session, got {other:?}"),
    }
}

pub fn details_of(response: Response) -> turn_proto::SessionDetails {
    match response {
        Response::SessionDetails { details } => *details,
        other => panic!("expected session details, got {other:?}"),
    }
}

pub fn workspace_of(response: Response) -> turn_proto::WorkspaceSummary {
    match response {
        Response::Workspace { workspace } => workspace,
        other => panic!("expected a workspace, got {other:?}"),
    }
}

pub fn layout_of(response: Response) -> turn_core::model::Layout {
    match response {
        Response::Layout { layout, .. } => layout,
        other => panic!("expected a layout, got {other:?}"),
    }
}

pub fn tree_of(response: Response) -> Vec<TreeNodeView> {
    match response {
        Response::Tree { nodes, .. } => nodes,
        other => panic!("expected a tree, got {other:?}"),
    }
}

pub fn attachment_of(response: Response) -> PaneAttachment {
    match response {
        Response::Attached { attachment } => *attachment,
        other => panic!("expected an attachment, got {other:?}"),
    }
}

pub fn attention_of(response: Response) -> Option<turn_proto::AttentionView> {
    match response {
        Response::Attention { entry } => entry,
        other => panic!("expected an attention entry, got {other:?}"),
    }
}

pub fn attention_list_of(response: Response) -> Vec<turn_proto::AttentionView> {
    match response {
        Response::AttentionList { entries } => entries,
        other => panic!("expected an attention list, got {other:?}"),
    }
}

pub fn effects_of(response: Response) -> Vec<turn_core::Effect> {
    match response {
        Response::Effects { effects } => effects,
        other => panic!("expected effects, got {other:?}"),
    }
}

pub fn node_of(response: Response) -> TreeNodeView {
    match response {
        Response::Node { node } => *node,
        other => panic!("expected a node, got {other:?}"),
    }
}

/// Whether a pid is still in the process table. Used to prove a process really is
/// running rather than trusting the daemon's own report of it.
pub fn pid_is_alive(pid: u32) -> bool {
    // Signal 0 asks the kernel whether the process exists without touching it.
    unsafe { libc_kill(pid as i32, 0) == 0 }
}

extern "C" {
    #[link_name = "kill"]
    fn libc_kill(pid: i32, signal: i32) -> i32;
}

// ------------------------------------------------------------------- hook payloads

/// The payloads captured from a real Claude Code run.
pub fn fixtures() -> serde_json::Value {
    let raw = include_str!("../../../turn-agents/tests/fixtures/claude-code-2.1.221.json");
    serde_json::from_str(raw).expect("the fixture must be valid JSON")
}

/// The session id the captured payloads carry, so synthesised payloads correlate with
/// the recorded ones.
pub fn fixture_session_id() -> String {
    fixtures()["Stop"]["session_id"]
        .as_str()
        .expect("the fixture must carry a session_id")
        .to_string()
}

/// A `Notification` payload of the given type.
///
/// Synthesised rather than captured: reproducing a permission prompt or an idle timeout
/// on demand is not something a scripted Claude Code run can be made to do reliably. The
/// field names are the ones the recorded payloads use, and the whole thing is translated
/// by the production adapter.
pub fn notification(notification_type: &str, message: &str) -> serde_json::Value {
    serde_json::json!({
        "hook_event_name": "Notification",
        "notification_type": notification_type,
        "message": message,
        "session_id": fixture_session_id(),
        "cwd": "/private/tmp",
        "permission_mode": "default",
    })
}

/// A `SubagentStart` payload — the confirmed hierarchy Claude Code reports itself.
pub fn subagent_start(agent_type: &str, agent_id: &str) -> serde_json::Value {
    serde_json::json!({
        "hook_event_name": "SubagentStart",
        "agent_type": agent_type,
        "agent_id": agent_id,
        "session_id": fixture_session_id(),
        "cwd": "/private/tmp",
    })
}

/// Posts a payload to the daemon's hook server, as an agent's own hook engine does.
pub async fn post_hook(url: &str, payload: &serde_json::Value) {
    let response = reqwest::Client::new()
        .post(url)
        .json(payload)
        .send()
        .await
        .expect("the hook server must answer");
    assert!(
        response.status().is_success(),
        "the hook server refused a registered token: {}",
        response.status()
    );
}

/// The callback URL the daemon handed to a node's adapter.
///
/// Read out of the scratch directory the daemon created for that node, which is where
/// the real Claude Code adapter also writes its injected settings — so this proves the
/// per-session, per-node scratch plumbing works rather than reaching into the daemon.
pub fn hook_url(data_dir: &Path, session: &SessionId, node: &NodeId) -> String {
    let path = turnd::paths::node_scratch(data_dir, session, node).join(HOOK_URL_FILE);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("no hook url at {}: {error}", path.display()))
        .trim()
        .to_string()
}

// ----------------------------------------------------------------- the fake agent

/// A registry whose agent panes launch `cat` instead of a real coding agent.
pub fn fake_registry() -> AdapterRegistry {
    let mut registry = AdapterRegistry::bare();
    registry.register(Arc::new(FakeAgent::new()));
    registry
}

/// As [`fake_registry`], plus a tool whose state can only be inferred from output.
pub fn inferring_registry() -> AdapterRegistry {
    let mut registry = fake_registry();
    registry.register(Arc::new(InferredAgent));
    registry
}

/// An agent CLI with no way to report to Turn, standing in for the ones that have none.
///
/// It launches a plain shell, which is what makes the test able to put any screen it
/// likes in front of the output heuristic. The heuristic itself, the confidence cap and
/// the policy resolution are all the production ones.
pub struct InferredAgent;

impl AgentAdapter for InferredAgent {
    fn id(&self) -> &'static str {
        "inferred-agent"
    }

    fn provider(&self) -> &'static str {
        "generic"
    }

    fn executables(&self) -> &'static [&'static str] {
        &["sh"]
    }

    fn detect(&self, _executable: &str) -> Option<PathBuf> {
        turn_agents::adapter::which("sh")
    }

    fn best_level(&self) -> IntegrationLevel {
        IntegrationLevel::Heuristic
    }

    fn capabilities(&self) -> Capabilities {
        turn_agents::HeuristicAdapter::new().capabilities()
    }

    fn prepare(&self, ctx: &LaunchContext) -> Result<LaunchPlan, AdapterError> {
        Ok(LaunchPlan {
            command: ctx.command.clone(),
            args: ctx.user_args.clone(),
            env: vec![("TURN_NODE_ID".into(), ctx.node_id.to_string())],
            level: IntegrationLevel::Heuristic,
            note: "State is inferred from output and marked as a guess.".to_string(),
        })
    }

    /// Nothing to translate: this tier has no structured channel at all.
    fn normalise(&self, _payload: &serde_json::Value, _ctx: &EventContext) -> Vec<TurnEvent> {
        Vec::new()
    }
}

/// An adapter that integrates like Claude Code but launches `cat`.
///
/// The substitution is deliberate and it is the only one in these tests. Everything that
/// matters about the integration is real: the hook server registration, the per-node
/// token, the scratch directory, the launch plan, the pty, and — most importantly —
/// [`ClaudeCodeAdapter::normalise`], so the recorded payloads are translated by the code
/// that will translate them in production.
///
/// What is not real is the process. Launching the user's actual agent from a test would
/// start something that talks to a paid API, needs credentials, and writes to their home
/// directory. `cat` is the useful stand-in: it holds a pty open, echoes what is written
/// to it, and ends when it is told to.
pub struct FakeAgent {
    inner: ClaudeCodeAdapter,
}

impl FakeAgent {
    pub fn new() -> Self {
        Self {
            inner: ClaudeCodeAdapter::new(),
        }
    }
}

impl Default for FakeAgent {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentAdapter for FakeAgent {
    fn id(&self) -> &'static str {
        "claude-code"
    }

    fn provider(&self) -> &'static str {
        "anthropic"
    }

    /// `claude` so the built-in templates resolve to this adapter, `cat` so a test can
    /// ask for it by name.
    fn executables(&self) -> &'static [&'static str] {
        &["claude", "cat"]
    }

    /// Always looks for `cat`: what gets launched is what has to exist, whichever
    /// of this adapter's names the pane was asked for.
    fn detect(&self, _executable: &str) -> Option<PathBuf> {
        turn_agents::adapter::which("cat")
    }

    fn best_level(&self) -> IntegrationLevel {
        IntegrationLevel::Structured
    }

    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }

    fn prepare(&self, ctx: &LaunchContext) -> Result<LaunchPlan, AdapterError> {
        // The same scratch directory the real adapter writes its settings into. Leaving
        // the URL here is how the test learns what an agent would have been told.
        std::fs::create_dir_all(&ctx.scratch_dir)?;
        std::fs::write(ctx.scratch_dir.join(HOOK_URL_FILE), ctx.endpoint.url())?;
        Ok(LaunchPlan {
            command: "cat".to_string(),
            args: Vec::new(),
            env: vec![
                ("TURN_SESSION_ID".into(), ctx.session_id.to_string()),
                ("TURN_NODE_ID".into(), ctx.node_id.to_string()),
                ("TURN_HOOK_URL".into(), ctx.endpoint.url()),
            ],
            level: IntegrationLevel::Structured,
            note: "A stand-in agent for tests: real hooks, real pty, no API calls.".to_string(),
        })
    }

    /// The production translation, unchanged. This is the point of the whole harness.
    fn normalise(&self, payload: &serde_json::Value, ctx: &EventContext) -> Vec<TurnEvent> {
        self.inner.normalise(payload, ctx)
    }
}

/// Sends a signal to a process, for the tests that stop the real binary the way an
/// operating system does.
pub fn send_signal(pid: u32, signal: i32) {
    let sent = unsafe { libc_kill(pid as i32, signal) };
    assert_eq!(sent, 0, "could not signal pid {pid}");
}

/// `SIGTERM`. The signal a service manager sends first, and the one a daemon that owns
/// unwritten state has to handle.
pub const SIGTERM: i32 = 15;

/// `SIGKILL`. Used only to prove that kernel-owned daemon locks do not require a
/// cleanup handler to become available after a crash.
pub const SIGKILL: i32 = 9;

/// Waits for a path to appear, so a test does not race a daemon's start-up.
pub async fn wait_for_path(path: &Path) {
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        if path.exists() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("{} never appeared", path.display());
}
