//! The state owner.
//!
//! One task holds every session, every pty handle and the attention manager, and it
//! is the only thing that mutates them. Four sources feed it:
//!
//! 1. **UI requests**, over the unix socket ([`crate::server`]).
//! 2. **Hook callbacks** from agents, over the loopback HTTP server in
//!    [`turn_agents::HookServer`].
//! 3. **Pty exits**, one watcher task per process.
//! 4. **A modest timer**, which releases deferred focus jumps, asks the kernel about the
//!    agents running in panes' shells, observes output for the panes that have a
//!    heuristic, repeats state to any client that fell behind, lets go of the terminals
//!    of processes that have finished, and — only when something has changed — sweeps
//!    the process table.
//!
//! Everything the loop does is synchronous. The store is synchronous, the domain is
//! synchronous, and a handler that cannot await cannot interleave with another
//! handler halfway through a state change. That is worth more than the concurrency
//! it gives up: the daemon's hard problem is that thirty agents change state at once,
//! and a single writer makes every rule in `turn-core` mean what it says.

pub mod attention;
pub mod authority;
mod checkout_authority;
pub mod clients;
pub mod command;
pub mod events;
pub mod hosting;
pub mod output;
pub mod preview;
#[cfg(test)]
mod profile_acceptance;
mod quota;
pub mod requests;
pub mod restore;
pub mod screens;
pub mod spawn;
pub mod supervise;
pub mod titles;
pub mod views;

pub use command::{ClientId, Command};

use crate::checkout_lock::CheckoutWriteLock;
use crate::error::Result;
use crate::instance::DataDirLock;
use clients::Client;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use turn_agents::{AdapterRegistry, HookServer, IntegrationLevel, OutputHeuristic};
use turn_core::attention::Effect;
use turn_core::event::{Confidence, TurnEvent};
use turn_core::ids::{HandoffId, LeaseId, NodeId, PaneId, SessionId, TemplateId, WorkspaceId};
use turn_core::model::{ContextHandoffOutcome, Session, Template, Workspace};
use turn_core::{AttentionManager, UserContext};
use turn_proto::{ErrorCode, Grid, ProtoError, ServerEvent};
use turn_pty::{ExitInfo, ProcessSupervisor, PtyProcess, ScreenSize, TerminalBuffer};
use turn_store::Store;

/// How often the loop wakes up when nothing is happening.
///
/// This is the granularity of a deferred focus jump landing after the user stops
/// typing, so it has to be well under the moment they notice. It is *not* a polling
/// interval for process state: hooks and exit watchers are pushed, and the process
/// table is only swept when something suggests it changed.
pub const TICK_INTERVAL: Duration = Duration::from_millis(500);

/// Work queued for the core task before senders start waiting.
pub const COMMAND_CAPACITY: usize = 2048;

/// Frames queued for one client before it starts losing them.
pub const CLIENT_FRAME_CAPACITY: usize = 1024;

/// How long a finished process's terminal is kept before the handle is let go.
///
/// The daemon is meant to hold thirty sessions for days, and a pty buffer is tens of
/// kilobytes of scrollback plus a parsed screen — so keeping every process that ever
/// ran is a leak with a schedule. Five minutes is well past the moment a user reads
/// the error that killed their build, and a pane whose handle has been reclaimed still
/// shows what happened to it: the node keeps its lifecycle, its exit code and its
/// place in the tree. Only the scrollback goes.
///
/// Nothing anybody is watching is ever reclaimed, whatever this says.
pub const FINISHED_PTY_RETENTION_MS: i64 = 5 * 60 * 1_000;

/// How long a stop the user asked for keeps excusing a process's exit.
///
/// A delivered signal is not a death. `SIGTERM` is a request, and plenty of programs —
/// interactive shells, and routinely the children an agent spawned — catch it and carry
/// on. An expectation with no bound then waits indefinitely for an exit that has nothing
/// to do with it, so the crash that eventually kills that process is filed as a stop the
/// user asked for: recorded as [`turn_core::state::Lifecycle::Stopped`], which is not a
/// failure, raising no trigger and no notification. The log stops being quiet and starts
/// being wrong.
///
/// Half a minute is well past a process that honours the signal — the slowest of them
/// flush state and go within a few seconds — and well short of "later that afternoon".
/// Past it, the process demonstrably chose to keep running, and whatever ends it next
/// ends it for its own reasons.
pub const EXPECTED_EXIT_GRACE_MS: i64 = 30 * 1_000;

/// Retention is enforced in the background as well as immediately after a setting write.
pub const PRIVACY_MAINTENANCE_INTERVAL_MS: i64 = 60 * 1_000;

/// A live process and what Turn knows about how it was launched.
pub struct Process {
    pub pty: PtyProcess,
    /// Last sanitised OSC 0/2 title observed from this exact PTY.
    pub process_title: Option<String>,
    /// Title to restore when the process clears its OSC title.
    pub fallback_title: String,
    /// Agent name to restore when a low-priority process title is cleared.
    pub fallback_agent_name: Option<turn_core::model::AgentName>,
    /// Adapter currently observing the foreground terminal subject.
    ///
    /// A hosted launch token has independent lifecycle authority; this field may
    /// follow another Agent that takes the same Shell PTY's foreground.
    pub adapter_id: String,
    /// The integration the launch actually achieved, which may be lower than the
    /// adapter's best if something was missing.
    pub level: IntegrationLevel,
    /// The hook token issued for this node, revoked when the process goes.
    pub hook_token: Option<String>,
    /// Output inference state, present only for panes running a tool that has no way
    /// to report to Turn.
    pub heuristic: Option<OutputHeuristic>,
    pub size: ScreenSize,
    pub session_id: SessionId,
    /// The last title generation this node was seen at.
    ///
    /// Compared as an integer on every coalesced read, so the common case — a shell
    /// re-sending the same title on every prompt, `vim` on every file — costs a
    /// comparison and produces no push.
    pub title_generation: u64,
    /// When this process's exit was recorded, if it has ended.
    ///
    /// The pty handle outlives the process on purpose — its buffer still holds what
    /// the process printed, and a user whose build just failed wants to read the
    /// error — so this is what says when "a while" started. See
    /// [`FINISHED_PTY_RETENTION_MS`].
    pub exited_ms: Option<i64>,
    /// The node of the command Turn started *inside* this process, when this process
    /// is a shell hosting one.
    ///
    /// This is the whole of Turn's claim about the parent/child link: the launch was
    /// Turn's own, so the edge is knowledge rather than something inferred from the
    /// process table, and the agent's adapter, integration level, hook token and turn
    /// state all belong to this node rather than to the shell. It is also what tells
    /// the supervisor which process in the table is the one Turn typed, so a sweep
    /// identifies it instead of adopting it a second time as an anonymous child.
    pub hosted: Option<NodeId>,
    /// Integration facts for the command Turn launched as `hosted`.
    ///
    /// Foreground observation fields may temporarily follow another Agent sharing
    /// this PTY. These two values restore the launched Agent's own observation tier
    /// when job control returns it to the foreground.
    pub hosted_adapter_id: Option<String>,
    pub hosted_level: Option<IntegrationLevel>,
    /// The Agent that currently owns this Shell PTY's foreground process group.
    ///
    /// This is terminal/presentation authority only, regardless of whether Turn
    /// launched the Agent. `hosted` separately records lifecycle/relaunch authority.
    /// Keeping both facts means Ctrl-Z can remove A's terminal without pretending A
    /// ended, and `fg` can restore the same Agent without relaunching it.
    pub observed_subject: Option<NodeId>,
    /// The original Pane whose subject follows this PTY's foreground job.
    ///
    /// Duplicated or explicitly opened Panes are exact views of their bound Node and
    /// must not be retargeted when job control changes this runtime's foreground.
    pub foreground_pane: Option<PaneId>,
}

/// Sensitive handoff material is ephemeral: it is never put in SQLite or an event.
#[derive(Clone)]
pub(crate) struct PendingContextHandoff {
    pub owner_client: ClientId,
    pub session_id: SessionId,
    pub source_node_id: NodeId,
    pub target_node_id: NodeId,
    pub mode: turn_core::model::ContextHandoffMode,
    pub body: turn_proto::ContextHandoffText,
    pub includes_activity: bool,
    pub created_ms: i64,
}

pub(crate) struct FinishedContextHandoff {
    pub owner_client: ClientId,
    pub session_id: SessionId,
    pub finished_ms: i64,
    pub outcome: ContextHandoffOutcome,
}

/// An applied event waiting for its durable boundary before publication.
pub(crate) struct FailedIngestCheckpoint {
    event: TurnEvent,
    effects: Vec<Effect>,
}

/// One source fact held behind an older failed runtime checkpoint.
pub(crate) enum DeferredRuntimeInput {
    Event {
        event: Box<TurnEvent>,
        now_ms: i64,
    },
    /// Exit observation has richer information than `TurnEvent` (notably the
    /// platform signal name), so retain the source fact rather than partially
    /// mutating the Session and trying to reconstruct it later.
    Exit {
        session_id: SessionId,
        node_id: NodeId,
        info: ExitInfo,
        now_ms: i64,
    },
}

/// Everything the daemon owns.
pub struct Core {
    /// Shared with the public daemon handle so the store cannot lose its process
    /// ownership guard while either the API handle or the detached core task lives.
    pub(crate) _data_dir_lock: Arc<DataDirLock>,
    pub(crate) store: Store,
    pub(crate) hooks: Arc<HookServer>,
    pub(crate) registry: AdapterRegistry,
    pub(crate) data_dir: PathBuf,
    /// A clone handed to every pump and exit watcher.
    pub(crate) commands: mpsc::Sender<Command>,

    pub(crate) workspaces: HashMap<WorkspaceId, Workspace>,
    pub(crate) sessions: HashMap<SessionId, Session>,
    pub(crate) templates: HashMap<TemplateId, Template>,
    pub(crate) processes: HashMap<NodeId, Process>,
    /// Parsed terminal history recovered after the daemon restart. These buffers are
    /// display-only: their nodes remain Orphaned/Lost and never pretend a PTY survived.
    pub(crate) recovered_terminals: HashMap<NodeId, TerminalBuffer>,
    pub(crate) pumps: HashMap<NodeId, JoinHandle<()>>,
    pub(crate) clients: HashMap<ClientId, Client>,

    /// The screen every cells attachment on a node is in step with, kept only while
    /// somebody is watching it.
    ///
    /// One grid per *watched* node rather than one per client: every attachment to a
    /// node renders the same geometry, so a shared baseline is what a diff is computed
    /// against, and a client that fell behind is repaired with a whole screen rather
    /// than with a copy of its own. Thirty idle sessions hold no grids at all — see
    /// [`screens`].
    pub(crate) screens: HashMap<NodeId, Grid>,

    pub(crate) attention: AttentionManager,
    pub(crate) user: UserContext,

    /// Events and UI effects whose atomic Session + event + attention checkpoint failed.
    ///
    /// They are retried in arrival order before periodic semantic work and before
    /// the next event. Until a retry commits, no client receives a projection for
    /// that event. The event id makes the retry idempotent if SQLite committed but
    /// the caller observed an ambiguous error.
    pub(crate) failed_ingest_checkpoints: VecDeque<FailedIngestCheckpoint>,

    /// Runtime events held behind the failed-checkpoint barrier, still unapplied.
    /// Applying them early could let a later successful checkpoint persist the
    /// global attention changes of an older event whose own transaction failed.
    pub(crate) deferred_runtime_inputs: VecDeque<DeferredRuntimeInput>,

    /// How trustworthy the source of each node's *current turn state* was.
    ///
    /// This is what stops a heuristic from overwriting a fact. A pane running Claude
    /// Code reports its own turn boundaries at [`Confidence::Explicit`]; if output
    /// inference later decides the same pane looks idle, that guess must not win. A
    /// user correction sets the same authority, which is why a corrected state stays
    /// corrected.
    pub(crate) turn_authority: HashMap<NodeId, Confidence>,

    /// Work an agent left running when its turn ended, per node.
    ///
    /// Kept because "the turn is over" and "the work is finished" are different
    /// claims and Claude Code tells us which one it means. A non-zero count asks the
    /// supervisor to look for those children so they appear in the tree, rather than
    /// letting a session read as finished while a test run continues.
    pub(crate) background_tasks: HashMap<NodeId, usize>,

    /// Per-node stability/rate-limit state for compact hierarchy previews. Raw
    /// bytes never live here; only a candidate row and scheduling metadata do.
    pub(crate) preview_probes: HashMap<NodeId, preview::PreviewProbe>,

    /// One provider/account quota cache shared by every Codex node.
    ///
    /// Account limits belong to the authenticated local CLI account, not to a
    /// conversation. Keeping the coordinator here prevents a large Session tree
    /// from starting one provider process per node.
    pub(crate) account_quota: quota::AccountQuotaCoordinator,
    /// The sole detached account-quota probe, retained so shutdown can cancel it
    /// and the child process's kill-on-drop guard can run immediately.
    pub(crate) account_quota_probe: Option<JoinHandle<()>>,

    /// Review-before-send drafts and a bounded replay fence. Bodies exist only in
    /// `pending_context_handoffs`; delivered entries retain metadata, never text.
    pub(crate) pending_context_handoffs: HashMap<HandoffId, PendingContextHandoff>,
    pub(crate) finished_context_handoffs: HashMap<HandoffId, FinishedContextHandoff>,

    /// Monotonic revision of the single Workspace -> Session -> Process
    /// navigation projection. Clients never apply a structural delta across a
    /// gap: they request a complete snapshot at the newest revision.
    pub(crate) hierarchy_revision: u64,

    /// Lease heartbeats are durable fencing evidence, not a 500 ms polling
    /// workload. The core tick uses this timestamp to coalesce writes.
    pub(crate) last_lease_heartbeat_ms: i64,

    /// Last bounded retention pass. Kept separate from lease/process clocks so a
    /// failure in one subsystem cannot postpone privacy enforcement indefinitely.
    pub(crate) last_privacy_maintenance_ms: i64,

    /// Kernel ownership independent of the configured SQLite data directory. Each
    /// active durable lease must have exactly one matching host-wide checkout lock.
    pub(crate) checkout_write_locks: HashMap<LeaseId, CheckoutWriteLock>,

    /// Stable per-user lock root shared by daemons with different SQLite stores.
    pub(crate) checkout_lock_dir: PathBuf,

    /// Nodes whose next exit was asked for, against the moment the request stops
    /// applying — see [`EXPECTED_EXIT_GRACE_MS`] for why it has to stop.
    ///
    /// A process the user killed has not failed, so its exit does not raise the failure
    /// trigger, does not notify them about something they did on purpose, and is not
    /// projected as `Failed`. What the platform reported is still on the node and in the
    /// event log — see `events::exit`.
    pub(crate) expected_exits: HashMap<NodeId, i64>,

    /// Restore reports, replayed to each client that connects.
    pub(crate) restore_reports: Vec<ServerEvent>,

    pub(crate) supervisor: ProcessSupervisor,
    pub(crate) last_sweep_ms: i64,
    /// When the process table is next worth looking at, if something has suggested it.
    ///
    /// A timestamp rather than a flag because the useful moment is never *now*: a process
    /// that has just started has not had time to start anything, and a sweep fired the
    /// instant it spawned would find nothing and clear the request.
    pub(crate) sweep_due_ms: Option<i64>,
    /// Foreground groups already given the pre-output reconciliation barrier for the
    /// current deferred sweep request, keyed by the Shell runtime.
    ///
    /// Output is allowed one eager process-table refresh per distinct foreground job:
    /// enough to fence B's first byte before publication, without turning every batch
    /// from a verbose ordinary command into another full system scan.
    pub(crate) eager_sweep_observations: HashMap<NodeId, (i64, u32)>,
}

impl Core {
    /// Builds the core and loads what is on disk. Nothing is launched here.
    pub fn new(
        data_dir_lock: Arc<DataDirLock>,
        store: Store,
        hooks: Arc<HookServer>,
        registry: AdapterRegistry,
        data_dir: PathBuf,
        checkout_lock_dir: PathBuf,
        commands: mpsc::Sender<Command>,
    ) -> Result<Self> {
        let mut core = Self {
            _data_dir_lock: data_dir_lock,
            store,
            hooks,
            registry,
            data_dir,
            commands,
            workspaces: HashMap::new(),
            sessions: HashMap::new(),
            templates: HashMap::new(),
            processes: HashMap::new(),
            recovered_terminals: HashMap::new(),
            pumps: HashMap::new(),
            clients: HashMap::new(),
            screens: HashMap::new(),
            attention: AttentionManager::new(),
            user: UserContext::default(),
            failed_ingest_checkpoints: VecDeque::new(),
            deferred_runtime_inputs: VecDeque::new(),
            turn_authority: HashMap::new(),
            background_tasks: HashMap::new(),
            preview_probes: HashMap::new(),
            account_quota: quota::AccountQuotaCoordinator::default(),
            account_quota_probe: None,
            pending_context_handoffs: HashMap::new(),
            finished_context_handoffs: HashMap::new(),
            hierarchy_revision: 1,
            last_lease_heartbeat_ms: 0,
            last_privacy_maintenance_ms: 0,
            checkout_write_locks: HashMap::new(),
            checkout_lock_dir,
            expected_exits: HashMap::new(),
            restore_reports: Vec::new(),
            supervisor: ProcessSupervisor::new(),
            // A daemon that has just started has not gone without sweeping for any time
            // at all, so the slow safety net does not fire on the first tick.
            last_sweep_ms: turn_core::now_ms(),
            sweep_due_ms: None,
            eager_sweep_observations: HashMap::new(),
        };
        let now_ms = turn_core::now_ms();
        core.restore(now_ms)?;
        core.maintain_privacy(now_ms);
        Ok(core)
    }

    /// Runs until a [`Command::Shutdown`] arrives or every sender is gone.
    pub async fn run(
        mut self,
        mut commands: mpsc::Receiver<Command>,
        mut hook_events: mpsc::Receiver<TurnEvent>,
    ) {
        let mut ticker = tokio::time::interval(TICK_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut shutdown_done = None;

        loop {
            tokio::select! {
                command = commands.recv() => match command {
                    Some(Command::Shutdown { done }) => {
                        shutdown_done = Some(done);
                        break;
                    }
                    Some(command) => {
                        if self.handle(command) {
                            break;
                        }
                    }
                    None => break,
                },
                // An agent reporting its own state. The channel is bounded inside the
                // hook server, which drops rather than making an agent wait on us.
                Some(event) = hook_events.recv() => {
                    self.ingest(event, turn_core::now_ms());
                }
                _ = ticker.tick() => self.tick(turn_core::now_ms()),
            }
        }

        self.shutdown(turn_core::now_ms());
        // A clean-shutdown acknowledgement is a durability boundary. Sending it from
        // `handle` used to happen before the run loop's final flush and before Core
        // dropped its data-directory lock, so an immediate restart could race the old
        // owner under load.
        if let Some(done) = shutdown_done {
            let _ = done.send(());
        }
    }

    /// Handles one command. Returns true when the loop should end.
    fn handle(&mut self, command: Command) -> bool {
        let now = turn_core::now_ms();
        match command {
            Command::ClientOpened {
                client,
                agreed_version,
                frames,
                ready,
            } => {
                self.client_opened(client, agreed_version, frames);
                let _ = ready.send(());
            }
            Command::ClientClosed { client } => self.client_closed(client),
            Command::Request {
                client,
                id,
                request,
                reply,
            } => {
                let op = request.op();
                let outcome = self.dispatch(client, *request, now);
                if let Err(error) = &outcome {
                    tracing::debug!(%client, %id, op, %error, "request failed");
                }
                let _ = reply.send(outcome);
            }
            Command::Output {
                node,
                data,
                dropped,
            } => {
                self.reconcile_before_output(&node, now);
                self.deliver_output(&node, data, dropped, now);
            }
            Command::Exited { node, info } => self.node_exited(&node, info, now),
            Command::AccountQuotaProbeFinished { result } => {
                self.account_quota_probe_finished(result, now)
            }
            Command::Shutdown { .. } => unreachable!("shutdown is handled by the run loop"),
        }
        false
    }

    /// Periodic work: deferred focus, output inference, repairing clients that fell
    /// behind, forgetting stop requests nothing answered, reclaiming finished terminals,
    /// and a process sweep only when something has suggested one is worth doing.
    fn tick(&mut self, now_ms: i64) {
        self.expire_context_handoffs(now_ms);
        self.retry_failed_ingest_checkpoints(now_ms);
        if !self.failed_ingest_checkpoints.is_empty() {
            return;
        }
        let effects = self.attention.tick(&self.user.clone(), now_ms);
        self.emit_effects(effects, now_ms);
        self.observe_hosted_agents(now_ms);
        self.observe_heuristics(now_ms);
        self.observe_process_titles(now_ms);
        self.observe_activity_previews(now_ms);
        self.observe_account_quotas(now_ms);
        self.heartbeat_workspace_leases(now_ms);
        if now_ms.saturating_sub(self.last_privacy_maintenance_ms)
            >= PRIVACY_MAINTENANCE_INTERVAL_MS
        {
            self.maintain_privacy(now_ms);
        }
        self.resync_clients(now_ms);
        self.forget_stale_stop_requests(now_ms);
        self.reap_finished_processes(now_ms);
        self.maybe_sweep(now_ms);
    }

    /// Drops stop requests whose process is still running well after being signalled.
    ///
    /// The process ignoring the signal is the observation that matters: it is alive, so
    /// the exit the request was waiting for never happened and never will. Leaving the
    /// entry would spend it on the process's eventual real death. Kept off `record_exit`'s
    /// critical path deliberately — this only bounds the table's size; the guarantee
    /// itself is the deadline check in `record_exit`, which holds whether or not a tick
    /// has run in between.
    fn forget_stale_stop_requests(&mut self, now_ms: i64) {
        self.expected_exits.retain(|node, applies_until| {
            let live = now_ms <= *applies_until;
            if !live {
                tracing::debug!(
                    %node,
                    "a signalled process outlived the stop request; its next exit is its own"
                );
            }
            live
        });
    }

    /// Flushes everything worth keeping and gives the checkout back.
    ///
    /// Node lifecycles are left exactly as they are: a session whose processes were
    /// alive when the daemon stopped is stored as alive, so the next start reads it
    /// as "was running", checks the process table and reports honestly. Rewriting
    /// them to `exited` here would erase the difference between a process that ended
    /// and one we lost.
    ///
    /// The write lease is the one thing that *is* rewritten, and it has to be: a lease
    /// left `active` by a daemon that stopped on purpose is indistinguishable from one
    /// left by a daemon that crashed, and the next start can only respond to that by
    /// asking the user to confirm write access they never gave up. See
    /// [`authority`](super::authority).
    fn shutdown(&mut self, now_ms: i64) {
        tracing::info!(
            sessions = self.sessions.len(),
            processes = self.processes.len(),
            "shutting down"
        );
        for pump in self.pumps.values() {
            pump.abort();
        }
        self.pumps.clear();
        if let Some(probe) = self.account_quota_probe.take() {
            probe.abort();
        }
        self.flush();
        self.release_write_authority(now_ms);
    }

    /// Writes in-memory state through to the store.
    pub(crate) fn flush(&mut self) {
        self.retry_failed_ingest_checkpoints(turn_core::now_ms());
        for workspace in self.workspaces.values() {
            if let Err(error) = self.store.workspaces().save(workspace) {
                tracing::error!(%error, workspace = %workspace.id, "could not save a workspace");
            }
        }
        for session in self.sessions.values() {
            if self
                .failed_ingest_checkpoints
                .iter()
                .any(|checkpoint| checkpoint.event.session_id == session.id)
            {
                tracing::error!(
                    session = %session.id,
                    "skipped a standalone Session flush behind a failed atomic checkpoint"
                );
                continue;
            }
            if let Err(error) = self.store.sessions().save(session) {
                tracing::error!(%error, session = %session.id, "could not save a session");
            }
        }
        if self.failed_ingest_checkpoints.is_empty() {
            if let Err(error) = self.store.attention().replace_all(self.attention.queue()) {
                tracing::error!(%error, "could not save the attention queue");
            }
        } else {
            tracing::error!(
                pending = self.failed_ingest_checkpoints.len(),
                deferred = self.deferred_runtime_inputs.len(),
                "runtime events remain uncheckpointed after the final retry; skipped standalone attention flush"
            );
        }
    }

    // ------------------------------------------------------------------ lookups

    pub(crate) fn workspace(
        &self,
        id: &WorkspaceId,
    ) -> std::result::Result<&Workspace, ProtoError> {
        self.workspaces
            .get(id)
            .ok_or_else(|| ProtoError::not_found("workspace", id.as_str()))
    }

    pub(crate) fn session(&self, id: &SessionId) -> std::result::Result<&Session, ProtoError> {
        self.sessions
            .get(id)
            .ok_or_else(|| ProtoError::not_found("session", id.as_str()))
    }

    /// Whether this durable session opts into raw terminal history.
    pub(crate) fn terminal_history_enabled(&self, id: &SessionId) -> bool {
        if self.store.path().is_none() {
            return false;
        }
        let Some(session) = self.sessions.get(id) else {
            return false;
        };
        let configured = self
            .setting_for(Some(id), turn_core::privacy::TERMINAL_HISTORY_KEY)
            .as_bool()
            .unwrap_or(true);
        configured
            && !session.env.iter().any(|(key, value)| {
                key.eq_ignore_ascii_case("TURN_TERMINAL_HISTORY")
                    && matches!(
                        value.trim().to_ascii_lowercase().as_str(),
                        "0" | "false" | "off" | "no" | "disabled"
                    )
            })
    }

    pub(crate) fn journal_config(&self) -> turn_pty::JournalConfig {
        let policy = self.privacy_policy();
        turn_pty::JournalConfig {
            max_journal_bytes: policy.terminal_journal_bytes,
            max_checkpoint_bytes: usize::try_from(policy.terminal_checkpoint_bytes)
                .unwrap_or(usize::MAX),
        }
    }

    pub(crate) fn session_mut(
        &mut self,
        id: &SessionId,
    ) -> std::result::Result<&mut Session, ProtoError> {
        self.sessions
            .get_mut(id)
            .ok_or_else(|| ProtoError::not_found("session", id.as_str()))
    }

    /// Writes one session through to the store.
    ///
    /// A failure is reported to the caller as `unavailable` rather than swallowed:
    /// the in-memory state is correct, but the user is about to be told their work is
    /// safe when it is not on disk.
    pub(crate) fn persist_session(&self, id: &SessionId) -> std::result::Result<(), ProtoError> {
        if self
            .failed_ingest_checkpoints
            .iter()
            .any(|checkpoint| &checkpoint.event.session_id == id)
        {
            tracing::warn!(
                session = %id,
                "deferred a standalone Session write behind a failed atomic checkpoint"
            );
            return Err(ProtoError::new(
                ErrorCode::Unavailable,
                "The Session has an event checkpoint waiting to be written to disk",
            ));
        }
        let Some(session) = self.sessions.get(id) else {
            return Ok(());
        };
        self.store.sessions().save(session).map_err(|error| {
            tracing::error!(%error, session = %id, "could not save a session");
            ProtoError::new(
                ErrorCode::Unavailable,
                "The change was applied but could not be written to disk",
            )
            .with_detail(error.to_string())
        })
    }

    /// Persists a session from a background path, where there is nobody to tell.
    pub(crate) fn persist_session_quietly(&self, id: &SessionId) {
        let _ = self.persist_session(id);
    }

    /// Records an event in the log.
    pub(crate) fn persist_event(&self, event: &TurnEvent) {
        if let Err(error) = self.store.events().append(event) {
            tracing::warn!(%error, kind = turn_core::event::event_name(&event.kind), "could not record an event");
        }
    }
}

/// A core built for a test, with the same wiring the daemon gives the real one.
///
/// Some of the daemon's rules are about *when* something happens — a terminal reclaimed
/// five minutes after its process ended, a client repaired on the next tick — and those
/// cannot be driven over a socket without sleeping through the interval. Every function
/// they exercise takes `now_ms`, so a harness that owns a `Core` directly can assert "an
/// hour later" as an integer. The store is in-memory and the hook server is the real one
/// on loopback; nothing here is a stand-in for daemon code.
#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use turn_core::ids::PaneId;
    use turn_core::model::{Layout, Pane, PaneKind, Workspace};
    use turn_proto::ServerFrame;

    pub(crate) struct Harness {
        pub core: Core,
        /// Held so the channel the core sends on stays open.
        pub commands: mpsc::Receiver<Command>,
        pub _hook_events: mpsc::Receiver<TurnEvent>,
        /// Held so the data directory outlives the core.
        pub _dir: tempfile::TempDir,
    }

    impl Harness {
        pub async fn new() -> Self {
            let dir = tempfile::tempdir().expect("a temporary directory");
            let store = Store::open_in_memory().expect("an in-memory store");
            let (hooks, hook_events) = HookServer::start().await.expect("the hook server");
            let (commands, inbox) = mpsc::channel(COMMAND_CAPACITY);
            let data_dir_lock = Arc::new(
                DataDirLock::acquire(dir.path()).expect("the harness data directory lock"),
            );
            let core = Core::new(
                data_dir_lock,
                store,
                Arc::new(hooks),
                AdapterRegistry::bare(),
                dir.path().to_path_buf(),
                dir.path().join(crate::paths::CHECKOUT_LOCKS_DIR),
                commands,
            )
            .expect("the core must build");
            Self {
                core,
                commands: inbox,
                _hook_events: hook_events,
                _dir: dir,
            }
        }

        /// Adds a session with one pane, whose ids the caller chooses.
        ///
        /// Inserted directly rather than through `create_session`, because these tests
        /// are about state the daemon holds and not about launching anything: a real
        /// creation would spawn a shell per pane.
        pub fn add_session(&mut self, session_id: SessionId, pane_id: PaneId, now_ms: i64) {
            let workspace = self
                .core
                .workspaces
                .values()
                .next()
                .cloned()
                .unwrap_or_else(|| Workspace::new("harness", "/tmp", now_ms));
            let mut pane = Pane::new(PaneKind::Shell);
            pane.id = pane_id;
            let mut session = turn_core::model::Session::new(
                workspace.id.clone(),
                session_id.to_string(),
                "/tmp",
                Layout::single(pane),
                now_ms,
            );
            session.id = session_id.clone();
            // Tests using this direct insertion helper deliberately bypass the
            // production read-only sandbox setup; mark the synthetic Session as
            // guarded so process-lifecycle tests can reach the code they exercise.
            session.read_only_enforced = true;
            // Written through as well as held, because the event log has a foreign key
            // to the session: an event for a session the store has never heard of is
            // dropped, which would make a test about the log pass for the wrong reason.
            if !self.core.workspaces.contains_key(&workspace.id) {
                let store = self.core.store.workspaces();
                store.save(&workspace).expect("the workspace must save");
            }
            self.core
                .store
                .sessions()
                .save(&session)
                .expect("the session must save");
            self.core.workspaces.insert(workspace.id.clone(), workspace);
            self.core.sessions.insert(session_id, session);
        }

        /// Marks a harness-only read-only Session as technically guarded so tests that
        /// exercise real child-process lifecycle code can cross the production launch
        /// boundary. Production Session creation deliberately leaves this false until
        /// an actual filesystem guard exists.
        pub fn allow_test_processes(&mut self, session_id: &SessionId) {
            self.core
                .sessions
                .get_mut(session_id)
                .expect("the harness session")
                .read_only_enforced = true;
        }

        /// Puts a real process on a real pty behind a pane.
        ///
        /// Spawns `cat` with the terminal's own echo turned off, so what reaches the
        /// screen is what a test asked the process to print and not the keystrokes that
        /// asked for it. Everything else is production code: a real pty, the real
        /// [`turn_pty::TerminalBuffer`], and the same `Process` record
        /// `materialise_pane` builds — which is what makes a screen taken from it the
        /// screen the daemon would really send.
        ///
        /// Returns once the process has printed its marker, so a later write cannot
        /// race the `stty` that silences the echo.
        pub async fn spawn_process(
            &mut self,
            session_id: &SessionId,
            pane_id: &PaneId,
            now_ms: i64,
        ) -> NodeId {
            let node_id = NodeId::new();
            let size = ScreenSize::new(24, 80);
            let spec = turn_pty::ProcessSpec::new("sh", "/tmp")
                .args(["-c", "stty -echo; printf 'READY\\r\\n'; exec cat"])
                .size(size);
            let pty = PtyProcess::spawn(node_id.clone(), spec, now_ms).expect("a pty must open");

            let mut node = turn_core::model::ProcessNode::process(
                session_id.clone(),
                turn_core::model::NodeKind::Terminal,
                "cat",
                "/tmp",
                now_ms,
            );
            node.id = node_id.clone();
            node.pid = Some(pty.pid());
            node.lifecycle = turn_core::state::Lifecycle::Alive;

            self.core.processes.insert(
                node_id.clone(),
                Process {
                    pty,
                    process_title: None,
                    fallback_title: node.title.clone(),
                    fallback_agent_name: node.agent.as_ref().map(|agent| agent.name.clone()),
                    adapter_id: "terminal".to_string(),
                    level: IntegrationLevel::GenericTerminal,
                    hook_token: None,
                    heuristic: None,
                    size,
                    session_id: session_id.clone(),
                    exited_ms: None,
                    title_generation: 0,
                    hosted: None,
                    hosted_adapter_id: None,
                    hosted_level: None,
                    observed_subject: None,
                    foreground_pane: Some(pane_id.clone()),
                },
            );
            if let Some(session) = self.core.sessions.get_mut(session_id) {
                session.tree.insert(node);
                if let Some(pane) = session.layout.get_mut(pane_id) {
                    pane.node_id = Some(node_id.clone());
                }
            }

            self.wait_for_output(&node_id, "READY").await;
            node_id
        }

        /// Makes a process print something, and delivers what it printed the way the
        /// pump does.
        ///
        /// The bytes handed to `deliver_output` are the process's *output*, collected
        /// from the same broadcast the pump subscribes to — not the bytes written in.
        /// That distinction is what stops a test about the byte stream from asserting
        /// against its own input.
        pub async fn feed(&mut self, node: &NodeId, data: &[u8]) {
            let (mut receiver, ()) = {
                let process = self.core.processes.get(node).expect("a live process");
                (process.pty.subscribe(), ())
            };
            self.core
                .processes
                .get(node)
                .expect("a live process")
                .pty
                .write(data)
                .expect("the pty must accept a write");

            let mut batch: Vec<u8> = Vec::new();
            let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
            while batch.is_empty() {
                if tokio::time::Instant::now() > deadline {
                    panic!("the process printed nothing after being written to");
                }
                if let Ok(Ok(chunk)) =
                    tokio::time::timeout(Duration::from_millis(200), receiver.recv()).await
                {
                    batch.extend_from_slice(&chunk);
                }
            }
            // Whatever else the same write produced, coalesced into one batch exactly
            // as the pump would.
            while let Ok(chunk) = receiver.try_recv() {
                batch.extend_from_slice(&chunk);
            }
            self.core
                .deliver_output(node, batch, 0, turn_core::now_ms());
        }

        /// Waits until a node's screen contains some text, so a test never races a
        /// process's start-up.
        pub async fn wait_for_output(&self, node: &NodeId, needle: &str) {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
            loop {
                let seen = self
                    .core
                    .processes
                    .get(node)
                    .and_then(|process| process.pty.snapshot())
                    .map(|snapshot| snapshot.text())
                    .unwrap_or_default();
                if seen.contains(needle) {
                    return;
                }
                if tokio::time::Instant::now() > deadline {
                    panic!("timed out waiting for {needle:?}; saw {seen:?}");
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }

        /// Registers a client whose channel holds `capacity` frames, and hands back the
        /// receiving end so a test can decide when — and whether — to drain it.
        pub fn add_client(&mut self, capacity: usize) -> (ClientId, mpsc::Receiver<ServerFrame>) {
            let (sender, receiver) = mpsc::channel(capacity);
            let id = ClientId(self.core.clients.len() as u64 + 1);
            self.core
                .client_opened(id, turn_proto::PROTOCOL_VERSION, sender);
            (id, receiver)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The core owns pty handles, which are `Send` but not `Sync`, and it is spawned
    /// onto tokio's multi-threaded runtime. If either property ever stops holding,
    /// this fails at compile time rather than as a confusing error inside
    /// `tokio::spawn`.
    #[test]
    fn the_core_can_be_moved_onto_the_runtime() {
        fn assert_send<T: Send>() {}
        assert_send::<Core>();
        assert_send::<Process>();
        assert_send::<Command>();
    }
}
