//! Launching a pane's process.
//!
//! The order here is the whole integration story, and every step depends on the one
//! before it:
//!
//! 1. Pick the adapter for the command the user asked for ([`AdapterRegistry`]).
//! 2. Register the node with the hook server, which mints a token and hands back the
//!    URL its callbacks must reach.
//! 3. Ask the adapter to [`prepare`](turn_agents::AgentAdapter::prepare) a launch. It
//!    writes whatever throwaway configuration it needs into a scratch directory Turn
//!    owns, and returns the command line that will actually run.
//! 4. Spawn *that* on a pty.
//!
//! The scratch directory is the reason the user's own configuration is never touched.
//! Claude Code's hooks arrive through `--settings`, which adds a layer over
//! `~/.claude/settings.json` rather than replacing it, and the file it points at lives
//! under Turn's data directory keyed by session — so closing the session takes the
//! configuration with it.

use super::{Core, Process};
use crate::paths;
use std::path::{Path, PathBuf};
use turn_agents::{IntegrationLevel, LaunchContext, OutputHeuristic};
use turn_core::event::{AgentRef, Confidence, EventKind, EventSource, TurnEvent};
use turn_core::ids::{NodeId, PaneId, SessionId};
use turn_core::model::{NodeKind, PaneKind, ProcessNode, Session, SessionMode, WorkspaceCheckout};
use turn_core::state::Lifecycle;
use turn_proto::{ErrorCode, ProtoError};
use turn_pty::{ExitInfo, ProcessSpec, PtyProcess, ReadOnlySandbox, ScreenSize};

/// The size a pane starts at before a client tells us what it is rendering.
///
/// A pty must have a size from the moment it exists — a program asks for one
/// immediately — and 24x80 is the size every terminal program copes with. An attach
/// replaces it with the client's real geometry before taking the replay.
const INITIAL_SIZE: ScreenSize = ScreenSize { rows: 24, cols: 80 };

impl Core {
    /// Enforces checkout ownership at the final launch boundary. Every route that
    /// can create a process reaches this check, including later splits, relaunches
    /// and start-up commands; creation-time validation alone is not sufficient.
    pub(crate) fn require_session_launch_allowed(
        &self,
        session_id: &SessionId,
    ) -> std::result::Result<(), ProtoError> {
        let session = self.session(session_id)?;
        if session.is_archived() {
            return Err(ProtoError::refused(
                "Restore the Session from Archived before starting a process",
            ));
        }
        if self.workspace(&session.workspace_id)?.archived {
            return Err(ProtoError::refused(
                "Restore the Workspace from Archived before starting a process",
            ));
        }
        match session.mode {
            SessionMode::ReadOnly if !session.read_only_enforced => Err(ProtoError::refused(
                "This read-only Session has no technical write guard, so Turn will not launch a process in it",
            )),
            SessionMode::MainCheckout => {
                let lease = self
                .store
                .hierarchy()
                .verify_active_write_lease(
                    &session.workspace_id,
                    session_id,
                    &session.checkout_id,
                )
                .map_err(|error| {
                    // Relaunch/AddPane is not a lease acquisition flow. Missing,
                    // recovery, stale, drifted, or unprovable authority all fail
                    // closed at the last boundary before a PTY can be spawned.
                    ProtoError::refused(
                        "This Session does not have a verified active write lease for the primary checkout",
                    )
                    .with_detail(error.to_string())
                })?;
                self.require_checkout_write_lock(session, &lease)
            }
            SessionMode::ReadOnly | SessionMode::IsolatedWorktree => Ok(()),
        }
    }

    /// Resolves a process working directory at the last boundary before launch.
    ///
    /// Lease ownership answers *who* may write. This answers *where* the process
    /// begins: both the Session cwd and a Pane override are resolved through the
    /// filesystem and must remain below the checkout assigned to the Session.
    /// Returning the canonical path also means the PTY never receives the caller's
    /// symlink/`..` spelling after it has passed validation.
    pub(crate) fn resolve_authorized_launch_cwd(
        &self,
        session_id: &SessionId,
        pane_cwd: Option<&str>,
    ) -> std::result::Result<String, ProtoError> {
        self.require_session_launch_allowed(session_id)?;
        let session = self.session(session_id)?;
        let checkout = self.checkout_for_session(session)?;
        let checkout_root = verified_checkout_root(&checkout)?;
        let session_cwd = resolve_contained_cwd(
            &checkout_root,
            &checkout_root,
            Some(&session.cwd),
            "Session",
        )?;
        resolve_contained_cwd(
            &checkout_root,
            Path::new(&session_cwd),
            pane_cwd,
            "Pane or command",
        )
    }

    /// Validates a Session definition before persistence, lease acquisition or
    /// configured command execution. This is deliberately repeated at launch:
    /// directories and symlinks can change after creation.
    pub(crate) fn validate_session_definition_cwds(
        &self,
        session: &Session,
    ) -> std::result::Result<String, ProtoError> {
        let checkout = self.checkout_for_session(session)?;
        self.validate_session_definition_cwds_for_checkout(session, &checkout)
    }

    /// As [`Self::validate_session_definition_cwds`], for a worktree checkout that
    /// has been created on disk but is not yet registered in SQLite.
    pub(crate) fn validate_session_definition_cwds_for_checkout(
        &self,
        session: &Session,
        checkout: &WorkspaceCheckout,
    ) -> std::result::Result<String, ProtoError> {
        verify_session_checkout_binding(session, checkout)?;
        let checkout_root = verified_checkout_root(checkout)?;
        let session_cwd = resolve_contained_cwd(
            &checkout_root,
            &checkout_root,
            Some(&session.cwd),
            "Session",
        )?;
        for pane in session.layout.panes() {
            if pane
                .cwd
                .as_deref()
                .is_some_and(|cwd| !cwd.trim().is_empty())
            {
                resolve_contained_cwd(
                    &checkout_root,
                    Path::new(&session_cwd),
                    pane.cwd.as_deref(),
                    "Pane",
                )?;
            }
        }
        Ok(session_cwd)
    }

    /// Preflights a newly requested Pane before mutating or persisting its Layout.
    /// It does not acquire or require a lease because a non-terminal Pane is only a
    /// view; any process it describes is authorised again by
    /// [`Self::resolve_authorized_launch_cwd`].
    pub(crate) fn validate_pane_definition_cwd(
        &self,
        session_id: &SessionId,
        pane_cwd: Option<&str>,
    ) -> std::result::Result<(), ProtoError> {
        let session = self.session(session_id)?;
        let checkout = self.checkout_for_session(session)?;
        let checkout_root = verified_checkout_root(&checkout)?;
        let session_cwd = resolve_contained_cwd(
            &checkout_root,
            &checkout_root,
            Some(&session.cwd),
            "Session",
        )?;
        resolve_contained_cwd(&checkout_root, Path::new(&session_cwd), pane_cwd, "Pane")?;
        Ok(())
    }

    pub(crate) fn checkout_for_session(
        &self,
        session: &Session,
    ) -> std::result::Result<WorkspaceCheckout, ProtoError> {
        let checkout = self
            .store
            .hierarchy()
            .checkout(&session.workspace_id, &session.checkout_id)
            .map_err(|error| {
                ProtoError::new(
                    ErrorCode::Unavailable,
                    "Turn could not verify the Session checkout",
                )
                .with_detail(error.to_string())
            })?
            .ok_or_else(|| {
                ProtoError::refused("The Session does not reference a registered checkout")
                    .with_detail(format!(
                        "workspace={} session={} checkout={}",
                        session.workspace_id, session.id, session.checkout_id
                    ))
            })?;
        verify_session_checkout_binding(session, &checkout)?;
        Ok(checkout)
    }

    /// Constructs the platform guard for a read-only Session without trusting its
    /// persisted enforcement flag. Creation uses this to decide that flag; every
    /// later spawn reconstructs the guard so stale metadata fails closed.
    pub(crate) fn read_only_sandbox(
        &self,
        session: &Session,
    ) -> std::result::Result<Option<ReadOnlySandbox>, ProtoError> {
        if session.mode != SessionMode::ReadOnly {
            return Ok(None);
        }
        let checkout = self.checkout_for_session(session)?;
        let checkout_root = verified_checkout_root(&checkout)?;
        ReadOnlySandbox::for_checkout(&checkout_root).map_err(|error| {
            ProtoError::new(
                ErrorCode::Unavailable,
                "Turn could not construct the read-only process guard",
            )
            .with_detail(error.to_string())
        })
    }

    fn process_sandbox(
        &self,
        session: &Session,
    ) -> std::result::Result<Option<ReadOnlySandbox>, ProtoError> {
        let sandbox = self.read_only_sandbox(session)?;
        if session.mode != SessionMode::ReadOnly {
            return Ok(None);
        }
        if !session.read_only_enforced {
            return Err(ProtoError::refused(
                "This read-only Session has no technical write guard, so Turn will not launch a process in it",
            ));
        }
        if sandbox.is_some() {
            return Ok(sandbox);
        }
        // Old unit-test harnesses mark synthetic read-only Sessions as guarded so
        // unrelated PTY lifecycle tests can cross the production launch boundary.
        #[cfg(test)]
        {
            Ok(None)
        }
        #[cfg(not(test))]
        {
            Err(ProtoError::refused(
                "The read-only process guard is unavailable on this platform; no process was started",
            ))
        }
    }

    /// Starts a process for every pane of a session that describes one.
    ///
    /// A pane whose command cannot be launched is left empty and logged rather than
    /// failing the session: a template that mentions a tool the user has not installed
    /// should still give them the rest of their desk.
    pub(crate) fn materialise_session(&mut self, session: &SessionId, now_ms: i64) {
        let panes: Vec<PaneId> = match self.sessions.get(session) {
            Some(session) => session
                .layout
                .panes()
                .iter()
                .map(|p| p.id.clone())
                .collect(),
            None => return,
        };
        for pane in panes {
            match self.materialise_pane(session, &pane, now_ms) {
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(%session, %pane, %error, "could not start a pane's process");
                }
            }
        }
    }

    /// Starts the process a pane describes, if it describes one.
    ///
    /// Returns `None` for a pane that is deliberately empty: one of Turn's own views,
    /// or a terminal pane with no command, which is a placeholder until something is
    /// put in it.
    pub(crate) fn materialise_pane(
        &mut self,
        session_id: &SessionId,
        pane_id: &PaneId,
        now_ms: i64,
    ) -> std::result::Result<Option<NodeId>, ProtoError> {
        self.materialise_pane_with(session_id, pane_id, &[], now_ms)
    }

    /// As [`Self::materialise_pane`], with arguments the pane itself does not carry.
    ///
    /// The only caller that needs this is a relaunch asking to resume a conversation:
    /// `--resume <id>` belongs to one launch, not to the pane's definition, and writing
    /// it into the pane would make every later relaunch resume a conversation that has
    /// moved on.
    pub(crate) fn materialise_pane_with(
        &mut self,
        session_id: &SessionId,
        pane_id: &PaneId,
        extra_args: &[String],
        now_ms: i64,
    ) -> std::result::Result<Option<NodeId>, ProtoError> {
        let session = self.session(session_id)?;
        let pane = session
            .layout
            .get(pane_id)
            .ok_or_else(|| ProtoError::not_found("pane", pane_id.as_str()))?;

        if !pane.kind.is_terminal() {
            return Ok(None);
        }

        let workspace = self.workspaces.get(&session.workspace_id);
        let Some(command) = pane_command(pane.kind, pane.command.as_deref(), workspace) else {
            return Ok(None);
        };
        let cwd = self.resolve_authorized_launch_cwd(session_id, pane.cwd.as_deref())?;
        let read_only_sandbox = self.process_sandbox(session)?;
        let title = pane
            .title
            .clone()
            .unwrap_or_else(|| command.rsplit('/').next().unwrap_or(&command).to_string());
        let mut env: Vec<(String, String)> = Vec::new();
        if let Some(workspace) = workspace {
            env.extend(workspace.env.iter().cloned());
        }
        env.extend(session.env.iter().cloned());
        env.extend(pane.env.iter().cloned());
        let mut user_args = pane.args.clone();
        user_args.extend(extra_args.iter().cloned());

        let command_line = if user_args.is_empty() {
            command.clone()
        } else {
            format!("{command} {}", user_args.join(" "))
        };
        let selection = self.registry.select(&command_line);

        if !selection.is_installed() {
            // Named plainly, with the adapter's own explanation, because the reason
            // has nothing to do with Turn: the program is not there.
            return Err(
                ProtoError::new(ErrorCode::Unavailable, selection.note.clone())
                    .with_detail(command_line),
            );
        }

        let kind = node_kind(pane.kind, selection.level);
        let mut node = if kind.is_agentic() {
            ProcessNode::agent(session_id.clone(), command.clone(), cwd.clone(), now_ms)
        } else {
            ProcessNode::process(
                session_id.clone(),
                kind,
                command.clone(),
                cwd.clone(),
                now_ms,
            )
        };
        node.title = title;
        if let Some(agent) = node.agent.as_mut() {
            // `ProcessNode::agent` starts from the command as its fallback. The
            // Pane's configured label is the better fallback until the PTY emits
            // a process title.
            if matches!(agent.name.source, turn_core::model::NameSource::Fallback) {
                agent.name.display_name = node.title.clone();
            }
        }
        node.args = user_args.clone();
        if let Some(agent) = node.agent.as_mut() {
            agent.agent = AgentRef {
                provider: Some(selection.adapter.provider().to_string()),
                tool: Some(selection.adapter.id().to_string()),
                model: None,
                external_id: None,
            };
            agent.resumable = selection.capabilities.resumable;
        }
        let node_id = node.id.clone();

        // A token is only issued to an adapter that has somewhere to report from.
        // Handing one to a plain terminal would be a credential for a channel nothing
        // will ever use.
        let reports_back = selection.level >= IntegrationLevel::Wrapper;
        let endpoint = if reports_back {
            self.hooks.register(
                session_id.clone(),
                node_id.clone(),
                std::sync::Arc::clone(&selection.adapter),
            )
        } else {
            turn_agents::HookEndpoint {
                base_url: self.hooks.base_url().to_string(),
                token: String::new(),
                helper_path: None,
            }
        };
        let token = reports_back.then(|| endpoint.token.clone());

        let scratch_dir = paths::node_scratch(&self.data_dir, session_id, &node_id);
        let launch = LaunchContext {
            session_id: session_id.clone(),
            node_id: node_id.clone(),
            cwd: cwd.clone(),
            command: command.clone(),
            user_args,
            endpoint,
            scratch_dir,
        };

        let plan = match selection.adapter.prepare(&launch) {
            Ok(plan) => plan,
            Err(error) => {
                self.revoke(token.as_deref());
                return Err(ProtoError::new(
                    ErrorCode::Unavailable,
                    "Turn could not write the configuration this agent needs",
                )
                .with_detail(error.to_string()));
            }
        };

        // The adapter's own environment goes last so it wins: a hook URL it just
        // generated must not be shadowed by a stale value in the workspace.
        env.extend(plan.env.iter().cloned());

        let spec = ProcessSpec {
            command: plan.command.clone(),
            args: plan.args.clone(),
            cwd: cwd.clone(),
            env,
            size: INITIAL_SIZE,
            clean_env: false,
            read_only_sandbox,
        };

        let journal_dir = self
            .terminal_history_enabled(session_id)
            .then(|| paths::node_terminal_history(&self.data_dir, session_id, &node_id));
        let checkout_lock_inheritance = self.checkout_lock_inheritance(session_id)?;
        #[cfg(unix)]
        let preserved_fds: Vec<_> = checkout_lock_inheritance
            .iter()
            .map(|lock| lock.raw_fd())
            .collect();
        #[cfg(not(unix))]
        let preserved_fds = Vec::new();
        let pty_result = match &journal_dir {
            Some(dir) => PtyProcess::spawn_persisted_with_preserved_fds(
                node_id.clone(),
                spec,
                now_ms,
                dir,
                &preserved_fds,
            ),
            None => {
                PtyProcess::spawn_with_preserved_fds(node_id.clone(), spec, now_ms, &preserved_fds)
            }
        };
        drop(checkout_lock_inheritance);
        let pty = match pty_result {
            Ok(pty) => pty,
            Err(error) => {
                self.revoke(token.as_deref());
                if journal_dir.is_some() {
                    paths::remove_node_terminal_history(&self.data_dir, session_id, &node_id);
                }
                return Err(ProtoError::new(
                    ErrorCode::Unavailable,
                    format!("Could not start `{command}`"),
                )
                .with_detail(error.to_string()));
            }
        };
        self.recovered_terminals.remove(&node_id);

        let pid = pty.pid();
        // What actually runs, which is not always what the user typed: an adapter may
        // have appended flags to make the tool report back. The tree shows the truth.
        node.command = plan.command.clone();
        node.args = plan.args.clone();
        node.env_highlights.insert(
            "TURN_INTEGRATION".to_string(),
            plan.level.label().to_string(),
        );
        node.lifecycle = Lifecycle::Spawning;

        self.watch_exit(&node_id, &pty);
        // The heuristic tier is defined by the level a launch achieved, not by one
        // adapter's name: anything that ends up inferring from output needs the
        // observer, and nothing above that tier should have one.
        let heuristic = (plan.level == IntegrationLevel::Heuristic).then(OutputHeuristic::new);
        let fallback_title = node.title.clone();
        let fallback_agent_name = node.agent.as_ref().map(|agent| agent.name.clone());
        self.processes.insert(
            node_id.clone(),
            Process {
                pty,
                process_title: None,
                fallback_title,
                fallback_agent_name,
                adapter_id: selection.adapter.id().to_string(),
                level: plan.level,
                hook_token: token,
                heuristic,
                size: INITIAL_SIZE,
                session_id: session_id.clone(),
                exited_ms: None,
            },
        );

        {
            let session = self.session_mut(session_id)?;
            session.tree.insert(node);
            if let Some(pane) = session.layout.get_mut(pane_id) {
                pane.node_id = Some(node_id.clone());
            }
            session.touch(now_ms);
        }

        // A new process is a reason to look for children — shortly, not now: it has not
        // had time to start any yet.
        self.request_sweep(now_ms);

        tracing::info!(
            %session_id, %node_id, pid,
            adapter = selection.adapter.id(),
            level = plan.level.label(),
            command = %plan.command,
            "started a process"
        );

        // Recorded as an event so the start is in the log with everything else, and so
        // the lifecycle reaches `Alive` through the same path every other state change
        // takes rather than by being assigned twice.
        let started = TurnEvent::new(
            session_id.clone(),
            EventKind::ProcessStarted {
                pid,
                command: plan.command.clone(),
            },
            EventSource::Supervisor,
            Confidence::Explicit,
            now_ms,
        )
        .with_node(node_id.clone());
        self.ingest(started, now_ms);
        self.refresh_checkout_lock_owner(session_id);

        Ok(Some(node_id))
    }

    /// Runs one of the session's configured start-up commands.
    ///
    /// It gets a node but no pane: it is a job, not a place to type. The node is what
    /// makes it visible — a failing `nvm use` should be something the user can see in
    /// the tree, not a silent reason their agent behaves oddly.
    ///
    /// This is not Turn running something it decided to run. The command comes from the
    /// workspace or template the user configured, which is the whole difference between
    /// this and the thing the product refuses to do.
    pub(crate) fn spawn_init_command(
        &mut self,
        session_id: &SessionId,
        command: &str,
        now_ms: i64,
    ) -> std::result::Result<NodeId, ProtoError> {
        let session = self.session(session_id)?;
        let cwd = self.resolve_authorized_launch_cwd(session_id, None)?;
        let read_only_sandbox = self.process_sandbox(session)?;
        let mut env: Vec<(String, String)> = Vec::new();
        if let Some(workspace) = self.workspaces.get(&session.workspace_id) {
            env.extend(workspace.env.iter().cloned());
        }
        env.extend(session.env.iter().cloned());
        let shell = default_shell(self.workspaces.get(&session.workspace_id));

        let mut node = ProcessNode::process(
            session_id.clone(),
            NodeKind::Background,
            command,
            cwd.clone(),
            now_ms,
        );
        node.title = command
            .split_whitespace()
            .next()
            .unwrap_or("init")
            .to_string();
        node.args = vec!["-c".to_string(), command.to_string()];
        let node_id = node.id.clone();

        let spec = ProcessSpec {
            command: shell.clone(),
            args: vec!["-c".to_string(), command.to_string()],
            cwd,
            env,
            size: INITIAL_SIZE,
            clean_env: false,
            read_only_sandbox,
        };
        let journal_dir = self
            .terminal_history_enabled(session_id)
            .then(|| paths::node_terminal_history(&self.data_dir, session_id, &node_id));
        let checkout_lock_inheritance = self.checkout_lock_inheritance(session_id)?;
        #[cfg(unix)]
        let preserved_fds: Vec<_> = checkout_lock_inheritance
            .iter()
            .map(|lock| lock.raw_fd())
            .collect();
        #[cfg(not(unix))]
        let preserved_fds = Vec::new();
        let pty_result = match &journal_dir {
            Some(dir) => PtyProcess::spawn_persisted_with_preserved_fds(
                node_id.clone(),
                spec,
                now_ms,
                dir,
                &preserved_fds,
            ),
            None => {
                PtyProcess::spawn_with_preserved_fds(node_id.clone(), spec, now_ms, &preserved_fds)
            }
        };
        drop(checkout_lock_inheritance);
        let pty = pty_result.map_err(|error| {
            if journal_dir.is_some() {
                paths::remove_node_terminal_history(&self.data_dir, session_id, &node_id);
            }
            ProtoError::new(
                ErrorCode::Unavailable,
                format!("Could not run the start-up command `{command}`"),
            )
            .with_detail(error.to_string())
        })?;
        self.recovered_terminals.remove(&node_id);
        let pid = pty.pid();

        self.watch_exit(&node_id, &pty);
        let fallback_title = node.title.clone();
        let fallback_agent_name = node.agent.as_ref().map(|agent| agent.name.clone());
        self.processes.insert(
            node_id.clone(),
            Process {
                pty,
                process_title: None,
                fallback_title,
                fallback_agent_name,
                adapter_id: "generic-terminal".to_string(),
                level: IntegrationLevel::GenericTerminal,
                hook_token: None,
                heuristic: None,
                size: INITIAL_SIZE,
                session_id: session_id.clone(),
                exited_ms: None,
            },
        );
        self.session_mut(session_id)?.tree.insert(node);

        let started = TurnEvent::new(
            session_id.clone(),
            EventKind::ProcessStarted {
                pid,
                command: command.to_string(),
            },
            EventSource::Supervisor,
            Confidence::Explicit,
            now_ms,
        )
        .with_node(node_id.clone());
        self.ingest(started, now_ms);
        self.refresh_checkout_lock_owner(session_id);
        Ok(node_id)
    }

    /// Revokes a hook token for a launch that did not happen.
    fn revoke(&self, token: Option<&str>) {
        if let Some(token) = token {
            self.hooks.unregister(token);
        }
    }

    /// Watches a process for its exit and reports it to the core loop.
    fn watch_exit(&self, node: &NodeId, pty: &PtyProcess) {
        let mut watcher = pty.exit_watcher();
        let commands = self.commands.clone();
        let node = node.clone();
        tokio::spawn(async move {
            loop {
                // The borrow is scoped so nothing is held across the await.
                let seen = watcher.borrow_and_update().clone();
                if let Some(info) = seen {
                    let _ = commands.send(super::Command::Exited { node, info }).await;
                    return;
                }
                if watcher.changed().await.is_err() {
                    return;
                }
            }
        });
    }

    /// Stops a node's process and forgets our handle on it.
    ///
    /// Dropping a [`PtyProcess`] ends the process it owns — that is the ownership model
    /// `turn-pty` documents — so this is also how a pane's process is stopped.
    pub(crate) fn discard_process(&mut self, node: &NodeId) -> Option<ExitInfo> {
        if let Some(pump) = self.pumps.remove(node) {
            pump.abort();
        }
        let process = self.processes.remove(node)?;
        let session_id = process.session_id.clone();
        // The screen was a view of this pty. With the pty gone there is nothing for it
        // to be a view of, and a stale grid would be diffed against on the next attach.
        self.forget_screen(node);
        self.revoke(process.hook_token.as_deref());
        let info = process.pty.exit_info();
        drop(process);
        if self.terminal_history_enabled(&session_id) {
            let dir = paths::node_terminal_history(&self.data_dir, &session_id, node);
            match turn_pty::TerminalJournal::recover(&dir, turn_pty::JournalConfig::default()) {
                Ok(Some(recovered)) => {
                    self.recovered_terminals
                        .insert(node.clone(), recovered.buffer);
                }
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(%node, %error, "could not retain terminal after releasing its PTY");
                }
            }
        }
        info
    }
}

/// The command a pane will run, if any.
///
/// A shell pane with no command gets the workspace's shell, then `$SHELL`, then
/// `/bin/sh`. Any other terminal pane with no command stays empty: guessing what a
/// pane labelled "server" should run would start something the user did not ask for.
fn pane_command(
    kind: PaneKind,
    declared: Option<&str>,
    workspace: Option<&turn_core::model::Workspace>,
) -> Option<String> {
    if let Some(command) = declared.filter(|c| !c.trim().is_empty()) {
        return Some(command.to_string());
    }
    match kind {
        PaneKind::Shell => Some(default_shell(workspace)),
        PaneKind::Agent => workspace.and_then(|w| w.default_agent.clone()),
        _ => None,
    }
}

/// The shell to run when a pane asks for one without saying which.
pub(crate) fn default_shell(workspace: Option<&turn_core::model::Workspace>) -> String {
    workspace
        .and_then(|w| w.default_shell.clone())
        .or_else(|| std::env::var("SHELL").ok())
        .filter(|shell| !shell.trim().is_empty())
        .unwrap_or_else(|| "/bin/sh".to_string())
}

fn verify_session_checkout_binding(
    session: &Session,
    checkout: &WorkspaceCheckout,
) -> std::result::Result<(), ProtoError> {
    let valid = match session.mode {
        SessionMode::MainCheckout | SessionMode::ReadOnly => {
            checkout.primary && session.worktree_path.is_none()
        }
        SessionMode::IsolatedWorktree => {
            !checkout.primary && session.worktree_path.as_deref() == Some(checkout.path.as_str())
        }
    };
    if checkout.workspace_id != session.workspace_id || checkout.id != session.checkout_id || !valid
    {
        return Err(
            ProtoError::refused("The Session checkout assignment is inconsistent").with_detail(
                format!(
                    "workspace={} session={} checkout={} mode={:?}",
                    session.workspace_id, session.id, session.checkout_id, session.mode
                ),
            ),
        );
    }
    Ok(())
}

fn verified_checkout_root(
    checkout: &WorkspaceCheckout,
) -> std::result::Result<PathBuf, ProtoError> {
    let resolved = std::fs::canonicalize(&checkout.path).map_err(|error| {
        ProtoError::refused("The Session checkout cannot be resolved safely")
            .with_detail(format!("{}: {error}", checkout.path))
    })?;
    if !resolved.is_dir() {
        return Err(
            ProtoError::refused("The Session checkout is not a directory")
                .with_detail(resolved.display().to_string()),
        );
    }
    let stored = PathBuf::from(&checkout.canonical_path);
    if resolved != stored {
        return Err(ProtoError::refused(
            "The Session checkout identity changed and must be reconciled",
        )
        .with_detail(format!(
            "stored={} resolved={}",
            checkout.canonical_path,
            resolved.display()
        )));
    }
    Ok(resolved)
}

/// Resolves one cwd against a canonical base and proves filesystem containment.
///
/// This is launch-root containment, not a process sandbox: once started, a program
/// still has the user's OS authority and may `chdir`, open absolute paths, follow a
/// newly replaced path component, or access non-filesystem resources.
fn resolve_contained_cwd(
    checkout_root: &Path,
    base: &Path,
    requested: Option<&str>,
    subject: &str,
) -> std::result::Result<String, ProtoError> {
    let candidate = match requested.filter(|cwd| !cwd.trim().is_empty()) {
        Some(cwd) if Path::new(cwd).is_absolute() => PathBuf::from(cwd),
        Some(cwd) => base.join(cwd),
        None => base.to_path_buf(),
    };
    let resolved = std::fs::canonicalize(&candidate).map_err(|error| {
        ProtoError::refused(format!(
            "{subject} working directory cannot be resolved safely"
        ))
        .with_detail(format!("{}: {error}", candidate.display()))
    })?;
    if !resolved.is_dir() {
        return Err(
            ProtoError::refused(format!("{subject} working directory is not a directory"))
                .with_detail(resolved.display().to_string()),
        );
    }
    if !resolved.starts_with(checkout_root) {
        return Err(ProtoError::refused(format!(
            "{subject} working directory is outside the Session checkout"
        ))
        .with_detail(format!(
            "checkout={} requested={} resolved={}",
            checkout_root.display(),
            candidate.display(),
            resolved.display()
        )));
    }
    Ok(resolved.to_string_lossy().into_owned())
}

/// What kind of node a pane's process is.
///
/// The adapter has the final say on whether something is an agent: a `Terminal` pane
/// the user typed `claude` into is an agent, and an `Agent` pane running a program
/// Turn has no integration for is not — it gets the turn axis it can actually fill,
/// which is none.
fn node_kind(pane: PaneKind, level: IntegrationLevel) -> NodeKind {
    if level >= IntegrationLevel::Heuristic {
        return NodeKind::Agent;
    }
    match pane {
        PaneKind::Agent | PaneKind::Terminal => NodeKind::Terminal,
        PaneKind::Shell => NodeKind::Shell,
        PaneKind::Tui => NodeKind::Tui,
        PaneKind::Server => NodeKind::Server,
        PaneKind::TestOutput => NodeKind::TestRunner,
        PaneKind::Logs => NodeKind::Background,
        PaneKind::TmuxTerminal => NodeKind::TmuxPane,
        _ => NodeKind::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::testing::Harness;
    use turn_core::ids::CheckoutId;
    use turn_core::model::{Layout, Pane, Session, Workspace};

    fn workspace_with_shell(shell: Option<&str>) -> Workspace {
        let mut workspace = Workspace::new("w", "/tmp", 0);
        workspace.default_shell = shell.map(str::to_string);
        workspace
    }

    #[test]
    fn a_shell_pane_with_no_command_falls_back_through_workspace_then_environment() {
        let workspace = workspace_with_shell(Some("/bin/zsh"));
        assert_eq!(
            pane_command(PaneKind::Shell, None, Some(&workspace)).as_deref(),
            Some("/bin/zsh")
        );
        // With no workspace preference the environment decides, and failing that a
        // shell that exists on every unix.
        let bare = workspace_with_shell(None);
        let resolved = pane_command(PaneKind::Shell, None, Some(&bare)).unwrap();
        assert!(!resolved.trim().is_empty());
    }

    #[test]
    fn a_pane_with_no_command_and_no_default_starts_nothing() {
        // The failure this prevents: a pane labelled "server" quietly running
        // something Turn guessed at.
        for kind in [
            PaneKind::Terminal,
            PaneKind::Server,
            PaneKind::Logs,
            PaneKind::TestOutput,
            PaneKind::Tui,
        ] {
            assert_eq!(pane_command(kind, None, None), None, "{kind:?}");
        }
        assert_eq!(pane_command(PaneKind::Agent, None, None), None);
    }

    #[test]
    fn a_declared_command_always_wins_and_blank_ones_do_not_count() {
        let workspace = workspace_with_shell(Some("/bin/zsh"));
        assert_eq!(
            pane_command(PaneKind::Shell, Some("fish"), Some(&workspace)).as_deref(),
            Some("fish")
        );
        assert_eq!(
            pane_command(PaneKind::Shell, Some("   "), Some(&workspace)).as_deref(),
            Some("/bin/zsh"),
            "whitespace is not a command"
        );
    }

    #[test]
    fn a_relative_pane_directory_is_canonicalised_against_the_session() {
        let temp = tempfile::tempdir().unwrap();
        let checkout = temp.path().join("repo");
        let session = checkout.join("packages/app");
        let pane = session.join("crates/turnd");
        std::fs::create_dir_all(&pane).unwrap();
        let checkout = std::fs::canonicalize(checkout).unwrap();
        let session = std::fs::canonicalize(session).unwrap();
        let pane = std::fs::canonicalize(pane).unwrap();
        assert_eq!(
            resolve_contained_cwd(&checkout, &session, Some("crates/turnd"), "Pane").unwrap(),
            pane.to_string_lossy()
        );
        assert_eq!(
            resolve_contained_cwd(&checkout, &session, None, "Pane").unwrap(),
            session.to_string_lossy()
        );
        assert_eq!(
            resolve_contained_cwd(&checkout, &session, Some(" "), "Pane").unwrap(),
            session.to_string_lossy()
        );
    }

    #[test]
    fn the_adapter_decides_what_counts_as_an_agent_not_the_pane_kind() {
        // `claude` typed into a plain terminal pane is an agent.
        assert_eq!(
            node_kind(PaneKind::Terminal, IntegrationLevel::Structured),
            NodeKind::Agent
        );
        // An "agent" pane running something Turn cannot integrate with does not get a
        // turn axis it would never be able to fill.
        assert_eq!(
            node_kind(PaneKind::Agent, IntegrationLevel::GenericTerminal),
            NodeKind::Terminal
        );
        assert_eq!(
            node_kind(PaneKind::Shell, IntegrationLevel::GenericTerminal),
            NodeKind::Shell
        );
        assert!(node_kind(PaneKind::Terminal, IntegrationLevel::Heuristic).is_agentic());
    }

    #[tokio::test]
    async fn a_recovery_lease_cannot_authorise_add_pane_or_relaunch() {
        let mut harness = Harness::new().await;
        let root = harness._dir.path().join("recovery-checkout");
        std::fs::create_dir(&root).unwrap();
        let workspace = Workspace::new("legacy", root.to_string_lossy(), 1);
        harness.core.store.workspaces().save(&workspace).unwrap();
        harness
            .core
            .workspaces
            .insert(workspace.id.clone(), workspace.clone());

        let mut session = Session::new(
            workspace.id.clone(),
            "legacy writer",
            workspace.root.clone(),
            Layout::single(Pane::new(PaneKind::Shell).with_command("/bin/sh")),
            1,
        );
        session.mode = SessionMode::MainCheckout;
        session.checkout_id = CheckoutId::primary_for(&workspace.id);
        let lease = harness
            .core
            .store
            .hierarchy()
            .create_session(&session, 1)
            .unwrap()
            .unwrap();
        let checkout_lock = harness.core.checkout_lock_claim(&session, &lease).unwrap();
        harness
            .core
            .sessions
            .insert(session.id.clone(), session.clone());
        let missing_host_lock = harness
            .core
            .require_session_launch_allowed(&session.id)
            .expect_err("SQLite authority alone must never launch a writer");
        assert_eq!(missing_host_lock.code, ErrorCode::Refused);
        assert!(missing_host_lock.message.contains("host-wide"));
        harness
            .core
            .install_checkout_write_lock(&session.id, &lease, checkout_lock);
        harness
            .core
            .require_session_launch_allowed(&session.id)
            .unwrap();

        assert!(harness
            .core
            .store
            .hierarchy()
            .require_recovery(&lease.id, 2)
            .unwrap());

        let error = harness
            .core
            .require_session_launch_allowed(&session.id)
            .expect_err("recovery-required is not authority to start another process");
        assert_eq!(error.code, ErrorCode::Refused);
        assert!(error
            .detail
            .unwrap()
            .contains("requires write-lease reconciliation"));
    }
}
