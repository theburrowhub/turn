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
//!
//! # An agent is not the pane's process
//!
//! Step 4 spawns the *pane's* process, and for an agent that process is the user's
//! shell: the agent's command line is written to the pty, exactly as if the user had
//! typed it. See [`super::hosting`] for the shape, and for the measurement that chose
//! it over putting the command in the shell's `argv`. It changes what a pane is: a
//! terminal that outlives the programs run in it, rather than a window onto one program.
//! An agent that ends leaves a prompt behind instead of a dead pane, which is what every
//! terminal the user already owns does.
//!
//! The cost is that the agent's process is one Turn started but does not hold, so its
//! identity has to be carried deliberately. Two nodes come out of one launch: a
//! [`NodeKind::Shell`] node that owns the pty and the pane, and an agent node holding
//! the [`turn_core::model::AgentInfo`], the adapter, the achieved integration level
//! and the hook token, linked to the shell with [`Relation::Confirmed`] — confirmed
//! because Turn typed the command itself, which is knowledge, not an inference from
//! the process table. [`Process::hosted`](super::Process::hosted) is where that
//! knowledge lives, and `super::supervise` uses it to identify the pid rather than
//! guess at one.

use super::{hosting, Core, Process};
use crate::paths;
use std::path::{Path, PathBuf};
use turn_agents::{
    AdapterRegistry, IntegrationLevel, LaunchContext, LaunchPlan, OutputHeuristic, Selection,
};
use turn_core::event::{AgentRef, Confidence, EventKind, EventSource, TurnEvent};
use turn_core::ids::{NodeId, PaneId, SessionId};
use turn_core::model::{
    NodeKind, PaneKind, ProcessNode, Relation, Session, SessionMode, WorkspaceCheckout,
};
use turn_core::state::Lifecycle;
use turn_proto::{ErrorCode, ProtoError};
use turn_pty::{ExitInfo, ProcessSpec, PtyProcess, ReadOnlySandbox, ScreenSize};

/// The size a pane starts at before a client tells us what it is rendering.
///
/// A pty must have a size from the moment it exists — a program asks for one
/// immediately — and 24x80 is the size every terminal program copes with. An attach
/// replaces it with the client's real geometry before taking the replay.
const INITIAL_SIZE: ScreenSize = ScreenSize { rows: 24, cols: 80 };

/// What a launch needs from the Session's checkout.
///
/// The distinction exists because "this Session may write to the shared checkout" and
/// "this Session may open a terminal" are different permissions, and treating them as one
/// meant a Session awaiting confirmation could not even give the user a shell — including
/// the shell they need to go and stop the process Turn is asking them about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LaunchAuthority {
    /// Turn runs something against the shared checkout on the user's behalf: an agent, or
    /// a command a pane names. This is what the exclusive write lease protects, and it is
    /// the default for anything whose shape cannot be established.
    CheckoutWrite,
    /// Turn opens the user's own interactive shell and from then on only relays their
    /// keystrokes. Turn writes nothing through it and starts nothing in it, so it needs no
    /// authority of its own. What the user types next is theirs, decided with the
    /// unanswered confirmation still in front of them.
    InteractiveShell,
}

impl Core {
    /// The checks that do not depend on what a launch would run.
    ///
    /// Archive state and a read-only Session's missing write guard refuse every process,
    /// including a bare shell: an archived Session is not a place to work, and a read-only
    /// Session with no technical guard has promised something it cannot enforce.
    pub(crate) fn require_session_launchable(
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
        if session.mode == SessionMode::ReadOnly && !session.read_only_enforced {
            return Err(ProtoError::refused(
                "This read-only Session has no technical write guard, so Turn will not launch a process in it",
            ));
        }
        Ok(())
    }

    /// Enforces checkout ownership at the final launch boundary. Every route that
    /// can create a process reaches this check, including later splits, relaunches
    /// and start-up commands; creation-time validation alone is not sufficient.
    ///
    /// `needs` is what *this* launch would do with the checkout. A Main Checkout Session
    /// whose authority is fenced or withheld still opens terminals; it does not run
    /// agents or commands until the user confirms.
    pub(crate) fn require_session_launch_allowed(
        &self,
        session_id: &SessionId,
        needs: LaunchAuthority,
    ) -> std::result::Result<(), ProtoError> {
        self.require_session_launchable(session_id)?;
        let session = self.session(session_id)?;
        match (session.mode, needs) {
            (SessionMode::MainCheckout, LaunchAuthority::CheckoutWrite) => {
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
                            "This Session must have confirmed write access to the main checkout \
                        before Turn runs an agent or a command against it; a terminal can \
                         be opened without it",
                        )
                        .with_detail(error.to_string())
                    })?;
                self.require_checkout_write_lock(session, &lease)
            }
            (SessionMode::MainCheckout, LaunchAuthority::InteractiveShell) => Ok(()),
            (SessionMode::ReadOnly | SessionMode::IsolatedWorktree, _) => Ok(()),
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
        needs: LaunchAuthority,
    ) -> std::result::Result<String, ProtoError> {
        self.require_session_launch_allowed(session_id, needs)?;
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
    /// Returns `None` only for one of Turn's own non-terminal views. A terminal pane with no
    /// command opens the user's configured shell: presenting an empty terminal and asking the
    /// user to start it is not a useful state.
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
        let Some((launch, request)) = self.resolve_pane_launch(session_id, pane_id, extra_args)?
        else {
            return Ok(None);
        };

        match launch {
            PaneLaunch::Direct { command } => {
                self.spawn_pane_command(session_id, pane_id, &command, request, now_ms)
            }
            PaneLaunch::Hosted { shell, command } => {
                self.spawn_hosted_agent(session_id, pane_id, &shell, &command, request, now_ms)
            }
            PaneLaunch::Unhosted { shell, note } => {
                self.spawn_bare_shell(session_id, pane_id, &shell, &note, request, now_ms)
            }
        }
    }

    /// Works out what a pane would run and gathers what it needs to run it.
    ///
    /// Everything here is read-only and every check that can refuse happens before any
    /// process exists: the launch shape, and the working directory resolved through the
    /// filesystem and proved to be inside the Session's checkout.
    fn resolve_pane_launch(
        &self,
        session_id: &SessionId,
        pane_id: &PaneId,
        extra_args: &[String],
    ) -> std::result::Result<Option<(PaneLaunch, PaneRequest)>, ProtoError> {
        let session = self.session(session_id)?;
        let pane = session
            .layout
            .get(pane_id)
            .ok_or_else(|| ProtoError::not_found("pane", pane_id.as_str()))?;
        if !pane.kind.is_terminal() {
            return Ok(None);
        }
        let workspace = self.workspaces.get(&session.workspace_id);
        let Some(launch) = pane_launch(
            pane.kind,
            pane.command.as_deref(),
            workspace,
            self.shell_for(Some(session_id)),
            &self.registry,
        ) else {
            return Ok(None);
        };
        let mut env: Vec<(String, String)> = Vec::new();
        if let Some(workspace) = workspace {
            env.extend(workspace.env.iter().cloned());
        }
        env.extend(session.env.iter().cloned());
        env.extend(pane.env.iter().cloned());
        let args: Vec<String> = pane
            .args
            .iter()
            .cloned()
            .chain(extra_args.iter().cloned())
            .collect();
        let needs = launch_authority(&launch, pane.kind, &args);
        let request = PaneRequest {
            kind: pane.kind,
            title: pane.title.clone(),
            args,
            env,
            // Resolved at the last boundary before a pty, which is where it has to
            // happen whatever shape the launch takes: a shell is a more general thing to
            // start than one command, so the rule that a process begins inside the
            // Session's checkout is proved for it too.
            cwd: self.resolve_authorized_launch_cwd(session_id, pane.cwd.as_deref(), needs)?,
        };
        Ok(Some((launch, request)))
    }

    /// What starting this pane's process would need from the Session's checkout.
    ///
    /// Fails closed: a pane whose launch cannot be resolved at all is treated as needing
    /// write authority, so a shape nobody anticipated is refused rather than allowed.
    pub(crate) fn pane_launch_authority(
        &self,
        session_id: &SessionId,
        pane_id: &PaneId,
    ) -> LaunchAuthority {
        let Ok(session) = self.session(session_id) else {
            return LaunchAuthority::CheckoutWrite;
        };
        let Some(pane) = session.layout.get(pane_id) else {
            return LaunchAuthority::CheckoutWrite;
        };
        let Some(launch) = pane_launch(
            pane.kind,
            pane.command.as_deref(),
            self.workspaces.get(&session.workspace_id),
            self.shell_for(Some(session_id)),
            &self.registry,
        ) else {
            return LaunchAuthority::CheckoutWrite;
        };
        launch_authority(&launch, pane.kind, &pane.args)
    }

    /// The agent a pane would host, for starting one again in a shell that never died.
    ///
    /// `None` when the pane would not host an agent at all, which is the difference
    /// between "start it again in the shell that is still there" and "the pane's own
    /// process has to be replaced".
    pub(crate) fn hosted_agent_request(
        &self,
        session_id: &SessionId,
        pane_id: &PaneId,
        extra_args: &[String],
    ) -> std::result::Result<Option<(String, PaneRequest)>, ProtoError> {
        Ok(
            match self.resolve_pane_launch(session_id, pane_id, extra_args)? {
                Some((PaneLaunch::Hosted { command, .. }, request)) => Some((command, request)),
                _ => None,
            },
        )
    }

    /// Starts a pane's command as the pane's own process.
    ///
    /// The shape for anything that is a job rather than a place to work: a dev server,
    /// a test run, a file browser. Its exit is the point, so nothing outlives it.
    fn spawn_pane_command(
        &mut self,
        session_id: &SessionId,
        pane_id: &PaneId,
        command: &str,
        request: PaneRequest,
        now_ms: i64,
    ) -> std::result::Result<Option<NodeId>, ProtoError> {
        let selection = self.select_installed(command, &request.args)?;
        let kind = node_kind(request.kind, selection.level);
        let mut node = new_node(session_id, kind, command, &request.cwd, now_ms);
        node.title = request
            .title
            .clone()
            .unwrap_or_else(|| executable_name(command).to_string());
        node.args = request.args.clone();
        describe_agent(&mut node, &selection);
        let node_id = node.id.clone();

        let (endpoint, token) = self.hook_endpoint(session_id, &node_id, &selection);
        let plan = self.prepare_launch(
            session_id,
            &node_id,
            command,
            &request,
            endpoint,
            &selection,
            token.as_deref(),
        )?;

        let mut env = request.env.clone();
        // The adapter's own environment goes last so it wins: a hook URL it just
        // generated must not be shadowed by a stale value in the workspace.
        env.extend(plan.env.iter().cloned());
        let read_only_sandbox = self.process_sandbox(self.session(session_id)?)?;

        let spec = ProcessSpec {
            command: plan.command.clone(),
            args: plan.args.clone(),
            cwd: request.cwd.clone(),
            env,
            size: INITIAL_SIZE,
            clean_env: false,
            read_only_sandbox,
        };
        let pty = self.open_pty(
            session_id,
            &node_id,
            spec,
            command,
            token.as_deref(),
            LaunchAuthority::CheckoutWrite,
            now_ms,
        )?;
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

        let fallback_title = node.title.clone();
        let fallback_agent_name = node.agent.as_ref().map(|agent| agent.name.clone());
        self.hold_process(
            &node_id,
            pty,
            &selection,
            &plan,
            token,
            None,
            fallback_title,
            fallback_agent_name,
            session_id,
        );
        {
            let session = self.session_mut(session_id)?;
            session.tree.insert(node);
            if let Some(pane) = session.layout.get_mut(pane_id) {
                pane.node_id = Some(node_id.clone());
            }
            session.touch(now_ms);
        }
        self.announce_start(
            session_id,
            &node_id,
            pid,
            &plan.command.clone(),
            &plan,
            selection.adapter.id(),
            now_ms,
        );
        Ok(Some(node_id))
    }

    /// Starts the pane's shell and runs an agent inside it.
    ///
    /// Two nodes, because two things are running and only one of them is Turn's: the
    /// shell node owns the pty and the pane, and the agent node owns everything that
    /// makes an agent an agent. The agent's edge to the shell is
    /// [`Relation::Confirmed`] because Turn wrote the command line itself — there is
    /// nothing to infer.
    fn spawn_hosted_agent(
        &mut self,
        session_id: &SessionId,
        pane_id: &PaneId,
        shell: &str,
        command: &str,
        request: PaneRequest,
        now_ms: i64,
    ) -> std::result::Result<Option<NodeId>, ProtoError> {
        let selection = self.select_installed(command, &request.args)?;

        let mut shell_node = new_node(session_id, NodeKind::Shell, shell, &request.cwd, now_ms);
        shell_node.title = executable_name(shell).to_string();
        let shell_id = shell_node.id.clone();

        let (mut agent, plan, token) =
            self.prepare_hosted_agent(session_id, command, &request, &selection, now_ms)?;
        let agent_id = agent.id.clone();

        let mut env = request.env.clone();
        // The adapter's own environment goes into the shell Turn is about to start, where
        // it is invisible and inherited by the agent — rather than onto the input line,
        // which would put a token on screen, in the scrollback and in the user's history.
        // The adapter's environment goes last so it wins: a hook URL it just generated
        // must not be shadowed by a stale value in the workspace.
        env.extend(plan.env.iter().cloned());
        let shell_args = hosting::interactive();
        let read_only_sandbox = self.process_sandbox(self.session(session_id)?)?;

        let spec = ProcessSpec {
            command: shell.to_string(),
            args: shell_args.clone(),
            cwd: request.cwd.clone(),
            env,
            size: INITIAL_SIZE,
            clean_env: false,
            read_only_sandbox,
        };
        let pty = self.open_pty(
            session_id,
            &shell_id,
            spec,
            shell,
            token.as_deref(),
            LaunchAuthority::CheckoutWrite,
            now_ms,
        )?;
        let pid = pty.pid();

        // Written now, before anything else can reach this terminal. The bytes wait in
        // the input queue until the shell starts reading, which is what makes this safe
        // against a slow rc file — see [`hosting`].
        let line = hosting::command_line(&plan.command, &plan.args);
        if let Err(error) = pty.write(&hosting::typed(&line, None)) {
            // The pty is dropped with this error, which ends the shell that was never
            // given anything to run.
            self.revoke(token.as_deref());
            paths::remove_node_scratch(&self.data_dir, session_id, &agent_id);
            return Err(ProtoError::new(
                ErrorCode::Unavailable,
                format!("Could not start `{command}` in the pane's shell"),
            )
            .with_detail(error.to_string()));
        }

        shell_node.args = shell_args;
        shell_node.lifecycle = Lifecycle::Spawning;
        // Alive with no pid of its own. Turn started this command, so there is nothing
        // provisional about it being here — but the pid belongs to a process the shell
        // forked, and `identify_hosted_process` is what learns it. A node that claims a
        // pid it has not seen would be a number in the UI matching nothing.
        agent.lifecycle = Lifecycle::Alive;
        agent.link_to(shell_id.clone(), Relation::Confirmed);

        let fallback_title = shell_node.title.clone();
        self.hold_process(
            &shell_id,
            pty,
            &selection,
            &plan,
            token,
            Some(agent_id.clone()),
            fallback_title,
            None,
            session_id,
        );
        {
            let session = self.session_mut(session_id)?;
            session.tree.insert(shell_node);
            session.tree.insert(agent);
            if let Some(pane) = session.layout.get_mut(pane_id) {
                pane.node_id = Some(shell_id.clone());
            }
            session.touch(now_ms);
        }
        tracing::info!(
            %session_id, shell = %shell_id, agent = %agent_id, pid,
            adapter = selection.adapter.id(),
            level = plan.level.label(),
            command = %plan.command,
            "started an agent in a pane's shell"
        );
        self.announce_start(
            session_id,
            &shell_id,
            pid,
            shell,
            &plan,
            selection.adapter.id(),
            now_ms,
        );
        Ok(Some(agent_id))
    }

    /// Opens the pane's shell with no agent in it, and prints why.
    ///
    /// The report this answers: `+ Pane Agent` opening a pane that does nothing and
    /// says nothing. A shell is still a useful pane, and the sentence is printed where
    /// the user is already looking.
    fn spawn_bare_shell(
        &mut self,
        session_id: &SessionId,
        pane_id: &PaneId,
        shell: &str,
        note: &str,
        request: PaneRequest,
        now_ms: i64,
    ) -> std::result::Result<Option<NodeId>, ProtoError> {
        let mut node = new_node(session_id, NodeKind::Shell, shell, &request.cwd, now_ms);
        // The shell's own name, not the pane's title: a pane titled "claude" that is
        // running a shell because claude is not installed must not be labelled claude.
        node.title = executable_name(shell).to_string();
        let node_id = node.id.clone();
        let args = hosting::interactive();
        let read_only_sandbox = self.process_sandbox(self.session(session_id)?)?;

        let spec = ProcessSpec {
            command: shell.to_string(),
            args: args.clone(),
            cwd: request.cwd.clone(),
            env: request.env.clone(),
            size: INITIAL_SIZE,
            clean_env: false,
            read_only_sandbox,
        };
        let pty = self.open_pty(
            session_id,
            &node_id,
            spec,
            shell,
            None,
            LaunchAuthority::InteractiveShell,
            now_ms,
        )?;
        let pid = pty.pid();
        // The sentence is printed by the shell itself, because the shell is the only
        // thing that can put text on this screen. A failure to write it is not a reason
        // to refuse the pane: the pane is a working shell either way, and it is better to
        // log the unexplained one than to take it away.
        if let Err(error) = pty.write(&hosting::typed(&hosting::notice(note), None)) {
            tracing::warn!(%session_id, %node_id, %error, "could not tell a pane why it has no agent");
        }
        node.args = args;
        node.lifecycle = Lifecycle::Spawning;

        self.watch_exit(&node_id, &pty);
        self.processes.insert(
            node_id.clone(),
            Process {
                pty,
                process_title: None,
                fallback_title: node.title.clone(),
                fallback_agent_name: None,
                // A shell is a terminal Turn makes no claims about, whatever the
                // registry would say about the agent that is not running in it.
                adapter_id: "generic-terminal".to_string(),
                level: IntegrationLevel::GenericTerminal,
                hook_token: None,
                heuristic: None,
                size: INITIAL_SIZE,
                session_id: session_id.clone(),
                exited_ms: None,
                title_generation: 0,
                hosted: None,
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
        self.request_sweep(now_ms);
        tracing::info!(%session_id, %node_id, pid, note, "opened a pane's shell with no agent in it");
        let started = TurnEvent::new(
            session_id.clone(),
            EventKind::ProcessStarted {
                pid,
                command: shell.to_string(),
            },
            EventSource::Supervisor,
            Confidence::Explicit,
            now_ms,
        )
        .with_node(node_id.clone());
        self.ingest(started, now_ms);
        Ok(Some(node_id))
    }

    /// Builds the agent node for a hosted launch, registers it with the hook server
    /// and asks its adapter to prepare the launch.
    ///
    /// Shared with the relaunch path, which starts the same agent again in a shell that
    /// never died — so the two cannot drift on which node the token was issued to or
    /// where the injected configuration was written.
    pub(crate) fn prepare_hosted_agent(
        &mut self,
        session_id: &SessionId,
        command: &str,
        request: &PaneRequest,
        selection: &Selection,
        now_ms: i64,
    ) -> std::result::Result<(ProcessNode, LaunchPlan, Option<String>), ProtoError> {
        let kind = node_kind(request.kind, selection.level);
        let mut agent = new_node(session_id, kind, command, &request.cwd, now_ms);
        agent.title = request
            .title
            .clone()
            .unwrap_or_else(|| executable_name(command).to_string());
        agent.args = request.args.clone();
        describe_agent(&mut agent, selection);
        let agent_id = agent.id.clone();

        let (endpoint, token) = self.hook_endpoint(session_id, &agent_id, selection);
        let plan = self.prepare_launch(
            session_id,
            &agent_id,
            command,
            request,
            endpoint,
            selection,
            token.as_deref(),
        )?;
        // What actually runs, which is not what the user typed: the adapter has added
        // the flags that make the tool report back. The tree shows the truth.
        agent.command = plan.command.clone();
        agent.args = plan.args.clone();
        agent.env_highlights.insert(
            "TURN_INTEGRATION".to_string(),
            plan.level.label().to_string(),
        );
        Ok((agent, plan, token))
    }

    /// Picks the adapter for a command and refuses if the program is not there.
    pub(crate) fn select_installed(
        &self,
        command: &str,
        args: &[String],
    ) -> std::result::Result<Selection, ProtoError> {
        let command_line = if args.is_empty() {
            command.to_string()
        } else {
            format!("{command} {}", args.join(" "))
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
        Ok(selection)
    }

    /// Registers a node with the hook server, when its adapter has something to report.
    ///
    /// A token is only issued to an adapter that has somewhere to report from. Handing
    /// one to a plain terminal would be a credential for a channel nothing will ever
    /// use.
    fn hook_endpoint(
        &self,
        session_id: &SessionId,
        node_id: &NodeId,
        selection: &Selection,
    ) -> (turn_agents::HookEndpoint, Option<String>) {
        if selection.level >= IntegrationLevel::Wrapper {
            let endpoint = self.hooks.register(
                session_id.clone(),
                node_id.clone(),
                std::sync::Arc::clone(&selection.adapter),
            );
            let token = endpoint.token.clone();
            (endpoint, Some(token))
        } else {
            (
                turn_agents::HookEndpoint {
                    base_url: self.hooks.base_url().to_string(),
                    token: String::new(),
                    helper_path: None,
                },
                None,
            )
        }
    }

    /// Asks an adapter to write its throwaway configuration and say what to run.
    #[allow(clippy::too_many_arguments)]
    fn prepare_launch(
        &self,
        session_id: &SessionId,
        node_id: &NodeId,
        command: &str,
        request: &PaneRequest,
        endpoint: turn_agents::HookEndpoint,
        selection: &Selection,
        token: Option<&str>,
    ) -> std::result::Result<LaunchPlan, ProtoError> {
        let launch = LaunchContext {
            session_id: session_id.clone(),
            node_id: node_id.clone(),
            cwd: request.cwd.clone(),
            command: command.to_string(),
            user_args: request.args.clone(),
            endpoint,
            scratch_dir: paths::node_scratch(&self.data_dir, session_id, node_id),
        };
        selection.adapter.prepare(&launch).map_err(|error| {
            self.revoke(token);
            ProtoError::new(
                ErrorCode::Unavailable,
                "Turn could not write the configuration this agent needs",
            )
            .with_detail(error.to_string())
        })
    }

    /// Opens the pty, revoking the launch's token if it cannot be opened.
    ///
    /// These arguments deliberately keep the launch authority, durable history identity,
    /// human-facing command and token visible at the single process-creation boundary.
    #[allow(clippy::too_many_arguments)]
    fn open_pty(
        &mut self,
        session_id: &SessionId,
        node_id: &NodeId,
        spec: ProcessSpec,
        command: &str,
        token: Option<&str>,
        needs: LaunchAuthority,
        now_ms: i64,
    ) -> std::result::Result<PtyProcess, ProtoError> {
        let journal_dir = self
            .terminal_history_enabled(session_id)
            .then(|| paths::node_terminal_history(&self.data_dir, session_id, node_id));
        let checkout_lock_inheritance = if needs == LaunchAuthority::CheckoutWrite {
            self.checkout_lock_inheritance(session_id)?
        } else {
            None
        };
        #[cfg(unix)]
        let preserved_fds: Vec<_> = checkout_lock_inheritance
            .iter()
            .map(|lock| lock.raw_fd())
            .collect();
        #[cfg(not(unix))]
        let preserved_fds = Vec::new();
        let result = match &journal_dir {
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
        match result {
            Ok(pty) => {
                self.recovered_terminals.remove(node_id);
                Ok(pty)
            }
            Err(error) => {
                self.revoke(token);
                if journal_dir.is_some() {
                    paths::remove_node_terminal_history(&self.data_dir, session_id, node_id);
                }
                Err(ProtoError::new(
                    ErrorCode::Unavailable,
                    format!("Could not start `{command}`"),
                )
                .with_detail(error.to_string()))
            }
        }
    }

    /// Takes ownership of a launched pty and starts watching it.
    #[allow(clippy::too_many_arguments)]
    fn hold_process(
        &mut self,
        node_id: &NodeId,
        pty: PtyProcess,
        selection: &Selection,
        plan: &LaunchPlan,
        token: Option<String>,
        hosted: Option<NodeId>,
        fallback_title: String,
        fallback_agent_name: Option<turn_core::model::AgentName>,
        session_id: &SessionId,
    ) {
        self.watch_exit(node_id, &pty);
        // The heuristic tier is defined by the level a launch achieved, not by one
        // adapter's name: anything that ends up inferring from output needs the
        // observer, and nothing above that tier should have one.
        let heuristic = (plan.level == IntegrationLevel::Heuristic).then(OutputHeuristic::new);
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
                title_generation: 0,
                hosted,
            },
        );
    }

    /// Records a start in the log and asks for a sweep once children could exist.
    ///
    /// `command` is the command line of the process that was started, which for a hosted
    /// launch is the *shell* rather than the agent inside it: applying the event writes
    /// this onto the node, and a shell node claiming to be `claude` would be the tree
    /// saying something Turn knows is untrue.
    #[allow(clippy::too_many_arguments)]
    fn announce_start(
        &mut self,
        session_id: &SessionId,
        node_id: &NodeId,
        pid: u32,
        command: &str,
        plan: &LaunchPlan,
        adapter: &str,
        now_ms: i64,
    ) {
        // A new process is a reason to look for children — shortly, not now: it has not
        // had time to start any yet.
        self.request_sweep(now_ms);
        tracing::info!(
            %session_id, %node_id, pid, adapter,
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
                command: command.to_string(),
            },
            EventSource::Supervisor,
            Confidence::Explicit,
            now_ms,
        )
        .with_node(node_id.clone());
        self.ingest(started, now_ms);
        self.refresh_checkout_lock_owner(session_id);
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
        // A configured start-up command runs against the checkout, so it waits for
        // confirmed write access exactly as a pane's command does.
        let cwd =
            self.resolve_authorized_launch_cwd(session_id, None, LaunchAuthority::CheckoutWrite)?;
        let read_only_sandbox = self.process_sandbox(session)?;
        let mut env: Vec<(String, String)> = Vec::new();
        if let Some(workspace) = self.workspaces.get(&session.workspace_id) {
            env.extend(workspace.env.iter().cloned());
        }
        env.extend(session.env.iter().cloned());
        let shell = self.shell_for(Some(session_id));

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
        let pty = self.open_pty(
            session_id,
            &node_id,
            spec,
            command,
            None,
            LaunchAuthority::CheckoutWrite,
            now_ms,
        )?;
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
                title_generation: 0,
                hosted: None,
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
    pub(crate) fn revoke(&self, token: Option<&str>) {
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

/// What the pane and its session contribute to a launch, taken once so nothing holds
/// a borrow of the session across a spawn.
pub(crate) struct PaneRequest {
    pub kind: PaneKind,
    /// The name the user gave the pane, if any. It describes what they asked to run,
    /// so on a hosted launch it names the agent rather than the shell around it.
    pub title: Option<String>,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    /// Already resolved and proved to be inside the Session's checkout.
    pub cwd: String,
}

/// What a terminal pane runs, and whether a shell stands between Turn and it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PaneLaunch {
    /// The command *is* the pane's process. What a job wants: `npm run dev` ends, and
    /// its ending is the thing the pane exists to show.
    Direct { command: String },
    /// The pane's process is an interactive shell, with this command running in it.
    /// What a terminal is: the pane outlives the programs run inside it.
    Hosted { shell: String, command: String },
    /// The pane's process is an interactive shell with no agent in it, and the
    /// sentence that says why.
    Unhosted { shell: String, note: String },
}

/// How a pane's launch is resolved, before anything is started.
///
/// Three rules, in this order:
///
/// * A command the registry recognises as an agent is hosted in a shell, whatever kind
///   of pane it was asked for. The adapter decides what an agent is — `claude` typed
///   into a plain terminal pane is one — and an agent is a program you run in a
///   terminal, not the terminal itself.
/// * Any terminal-backed pane with no command is the shell, run directly. It already is a
///   terminal; there is nothing to host, and an empty panel would only turn recovery into a
///   user action.
/// * An agent pane with no command has to resolve one, and always resolves to
///   *something*: the workspace's configured agent, else the first agent CLI on the
///   user's PATH, else a shell that says why it is only a shell. `+ Pane Agent`
///   opening an empty pane with no explanation is the failure this rules out.
///
/// `shell` is resolved by the caller rather than here, because the answer depends on
/// settings the caller has the Session for — a Session-level `shell.command` beats its
/// Workspace's, and this function is given only the Workspace.
fn pane_launch(
    kind: PaneKind,
    declared: Option<&str>,
    workspace: Option<&turn_core::model::Workspace>,
    shell: String,
    registry: &AdapterRegistry,
) -> Option<PaneLaunch> {
    if let Some(command) = declared.filter(|c| !c.trim().is_empty()) {
        // A command that is not installed still resolves here and is refused, with the
        // adapter's own explanation, by the launch itself. Substituting a different
        // program for the one the user wrote would be worse than saying it is missing.
        return Some(if is_agent(registry, command) {
            PaneLaunch::Hosted {
                shell,
                command: command.to_string(),
            }
        } else {
            PaneLaunch::Direct {
                command: command.to_string(),
            }
        });
    }
    match kind {
        PaneKind::Agent => Some(match workspace.and_then(default_agent) {
            Some(agent) if registry.select(&agent).is_installed() => PaneLaunch::Hosted {
                shell,
                command: agent,
            },
            // Configured, and not there. Turn does not quietly run a different agent
            // than the one the workspace names: it opens the shell and says what is
            // missing, which is something the user can act on.
            Some(agent) => PaneLaunch::Unhosted {
                note: format!(
                    "Turn could not start \"{agent}\", this workspace's default agent: \
                     it is not on your PATH. This pane is your shell — install it, or \
                     type another agent's name here."
                ),
                shell,
            },
            None => match first_installed_agent(registry) {
                Some(agent) => PaneLaunch::Hosted {
                    shell,
                    command: agent,
                },
                None => PaneLaunch::Unhosted {
                    note: "Turn found no agent CLI on your PATH, so this pane is your \
                           shell. Install one, or set this workspace's default agent."
                        .to_string(),
                    shell,
                },
            },
        }),
        kind if kind.is_terminal() => Some(PaneLaunch::Direct { command: shell }),
        _ => None,
    }
}

/// What a resolved launch needs from the checkout it starts in.
///
/// One shape needs nothing: Turn starting the user's interactive shell, with no command
/// and no arguments, and then only relaying keystrokes to it. Everything else — an agent,
/// a test runner, a shell handed a `-c` script — is Turn running something against the
/// shared checkout on the user's behalf, which is exactly what the write lease protects.
///
/// The shell's own working directory is still inside the checkout, and the user can of
/// course type `git commit` into it. That is not this boundary's business: the lease
/// exists so Turn does not put a *second* automated writer into a checkout that may still
/// have one, and the person typing is the same person the confirmation is waiting on.
fn launch_authority(launch: &PaneLaunch, kind: PaneKind, args: &[String]) -> LaunchAuthority {
    let bare_shell = |command: &str| {
        args.is_empty()
            && command.split_whitespace().count() == 1
            && turn_pty::classify(command) == NodeKind::Shell
    };
    match launch {
        // An agent pane that could not start an agent is a shell with an explanation
        // attached. The explanation is text the pane displays, not a command it runs.
        PaneLaunch::Unhosted { shell, .. } if bare_shell(shell) => {
            LaunchAuthority::InteractiveShell
        }
        PaneLaunch::Direct { command } if kind.is_terminal() && bare_shell(command) => {
            LaunchAuthority::InteractiveShell
        }
        _ => LaunchAuthority::CheckoutWrite,
    }
}

/// Whether the registry recognises a command line as an agent rather than a program
/// it merely runs.
fn is_agent(registry: &AdapterRegistry, command_line: &str) -> bool {
    registry.select(command_line).level >= IntegrationLevel::Heuristic
}

fn default_agent(workspace: &turn_core::model::Workspace) -> Option<String> {
    workspace
        .default_agent
        .as_deref()
        .map(str::trim)
        .filter(|agent| !agent.is_empty())
        .map(str::to_string)
}

/// The first agent CLI the registry can actually find on `PATH`.
///
/// Used only when a workspace has named no default. Adapters are ordered strongest
/// integration first, so the agent Turn understands best wins over one it can only
/// infer about — and an adapter that claims several commands is asked about each,
/// because the answer has to be a command that exists.
fn first_installed_agent(registry: &AdapterRegistry) -> Option<String> {
    registry
        .adapters()
        .iter()
        .filter(|adapter| adapter.best_level() >= IntegrationLevel::Heuristic)
        .flat_map(|adapter| {
            adapter
                .executables()
                .iter()
                .map(move |executable| (adapter, *executable))
        })
        .find(|(adapter, executable)| adapter.detect(executable).is_some())
        .map(|(_, executable)| executable.to_string())
}

/// A node for one launch, agentic or not.
fn new_node(
    session_id: &SessionId,
    kind: NodeKind,
    command: &str,
    cwd: &str,
    now_ms: i64,
) -> ProcessNode {
    if kind.is_agentic() {
        let mut node = ProcessNode::agent(session_id.clone(), command, cwd, now_ms);
        node.kind = kind;
        node
    } else {
        ProcessNode::process(session_id.clone(), kind, command, cwd, now_ms)
    }
}

/// Records which tool an agent node is, and what it can be asked to do.
fn describe_agent(node: &mut ProcessNode, selection: &Selection) {
    if let Some(agent) = node.agent.as_mut() {
        agent.agent = AgentRef {
            provider: Some(selection.adapter.provider().to_string()),
            tool: Some(selection.adapter.id().to_string()),
            model: None,
            external_id: None,
        };
        agent.resumable = selection.capabilities.resumable;
    }
}

/// The program a command names, without its path.
pub(crate) fn executable_name(command: &str) -> &str {
    command
        .split_whitespace()
        .next()
        .unwrap_or(command)
        .rsplit('/')
        .next()
        .unwrap_or(command)
}

impl Core {
    /// The shell to run for one Session, settings included.
    ///
    /// The chain is: the resolved `shell.command` preference, then the Workspace's own
    /// `default_shell` field, then `$SHELL`, then `/bin/sh`. The preference goes first
    /// because it is the more specific statement — it can be made per Session and per
    /// Template, and the field cannot — and the field stays because it is what existing
    /// Workspaces already hold.
    pub(crate) fn shell_for(&self, session_id: Option<&SessionId>) -> String {
        let configured = self
            .setting_for(session_id, "shell.command")
            .as_str()
            .map(str::to_string)
            .filter(|shell| !shell.trim().is_empty());
        if let Some(shell) = configured {
            return shell;
        }
        let workspace = session_id
            .and_then(|id| self.session(id).ok())
            .and_then(|session| self.workspaces.get(&session.workspace_id));
        default_shell(workspace)
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
    use turn_agents::AgentAdapter;
    use turn_core::ids::CheckoutId;
    use turn_core::model::{Layout, Pane, Session, Workspace};

    fn workspace_with_shell(shell: Option<&str>) -> Workspace {
        let mut workspace = Workspace::new("w", "/tmp", 0);
        workspace.default_shell = shell.map(str::to_string);
        workspace
    }

    /// An adapter for a tool that is definitely installed, so "an agent is hosted in a
    /// shell" can be asserted without depending on which agent CLIs this machine has.
    struct InstalledAgent;

    impl turn_agents::AgentAdapter for InstalledAgent {
        fn id(&self) -> &'static str {
            "installed-agent"
        }
        fn provider(&self) -> &'static str {
            "test"
        }
        /// `sh` exists on every unix; the second name is the one no machine has, so the
        /// "not installed" branch is reachable through the same adapter.
        fn executables(&self) -> &'static [&'static str] {
            &["sh", "turn-absent-agent-xyz"]
        }
        fn best_level(&self) -> IntegrationLevel {
            IntegrationLevel::Structured
        }
        fn capabilities(&self) -> turn_agents::Capabilities {
            turn_agents::Capabilities::default()
        }
        fn prepare(
            &self,
            ctx: &LaunchContext,
        ) -> std::result::Result<LaunchPlan, turn_agents::AdapterError> {
            Ok(LaunchPlan {
                command: ctx.command.clone(),
                args: ctx.user_args.clone(),
                env: Vec::new(),
                level: IntegrationLevel::Structured,
                note: String::new(),
            })
        }
        fn normalise(
            &self,
            _payload: &serde_json::Value,
            _ctx: &turn_agents::EventContext,
        ) -> Vec<TurnEvent> {
            Vec::new()
        }
    }

    fn registry_with_an_installed_agent() -> AdapterRegistry {
        let mut registry = AdapterRegistry::bare();
        registry.register(std::sync::Arc::new(InstalledAgent));
        registry
    }

    #[test]
    fn a_shell_pane_with_no_command_runs_the_shell_itself_and_nothing_is_hosted_in_it() {
        let registry = AdapterRegistry::bare();
        let workspace = workspace_with_shell(Some("/bin/zsh"));
        assert_eq!(
            pane_launch(
                PaneKind::Shell,
                None,
                Some(&workspace),
                default_shell(Some(&workspace)),
                &registry,
            ),
            Some(PaneLaunch::Direct {
                command: "/bin/zsh".to_string()
            }),
            "a shell pane is already a terminal; there is nothing to host"
        );
        // With no workspace preference the environment decides, and failing that a
        // shell that exists on every unix.
        let bare = workspace_with_shell(None);
        let Some(PaneLaunch::Direct { command }) = pane_launch(
            PaneKind::Shell,
            None,
            Some(&bare),
            default_shell(Some(&bare)),
            &registry,
        ) else {
            panic!("a shell pane always resolves to a shell")
        };
        assert!(!command.trim().is_empty());
    }

    #[test]
    fn every_commandless_terminal_opens_the_configured_shell() {
        let registry = AdapterRegistry::bare();
        for kind in [
            PaneKind::Terminal,
            PaneKind::Shell,
            PaneKind::Server,
            PaneKind::Logs,
            PaneKind::TestOutput,
            PaneKind::Tui,
            PaneKind::TmuxTerminal,
        ] {
            assert_eq!(
                pane_launch(kind, None, None, default_shell(None), &registry),
                Some(PaneLaunch::Direct {
                    command: default_shell(None),
                }),
                "{kind:?}"
            );
        }
    }

    #[test]
    fn a_non_terminal_view_with_no_command_starts_nothing() {
        let registry = AdapterRegistry::bare();
        for kind in [
            PaneKind::EventLog,
            PaneKind::AgentTree,
            PaneKind::ProcessDetails,
            PaneKind::Preview,
            PaneKind::Placeholder,
        ] {
            assert_eq!(
                pane_launch(kind, None, None, default_shell(None), &registry),
                None,
                "{kind:?}"
            );
        }
    }

    /// The report this answers: `+ Pane Agent` doing nothing at all. An agent pane
    /// always resolves to something the user can see, and when it cannot resolve an
    /// agent it resolves to their shell with the reason in it.
    #[test]
    fn an_agent_pane_with_no_configured_default_falls_back_to_an_agent_on_the_path() {
        let registry = registry_with_an_installed_agent();
        let workspace = workspace_with_shell(Some("/bin/zsh"));
        assert_eq!(
            pane_launch(
                PaneKind::Agent,
                None,
                Some(&workspace),
                default_shell(Some(&workspace)),
                &registry,
            ),
            Some(PaneLaunch::Hosted {
                shell: "/bin/zsh".to_string(),
                command: "sh".to_string()
            }),
            "the first agent CLI the registry can find is better than an empty pane"
        );

        // And with no agent CLI installed at all, the pane is still a working shell
        // that says why it is only a shell.
        let Some(PaneLaunch::Unhosted { shell, note }) = pane_launch(
            PaneKind::Agent,
            None,
            Some(&workspace),
            default_shell(Some(&workspace)),
            &AdapterRegistry::bare(),
        ) else {
            panic!("an agent pane must always resolve to something")
        };
        assert_eq!(shell, "/bin/zsh");
        assert!(note.contains("no agent CLI"), "{note}");
        assert!(note.contains("shell"), "{note}");
    }

    #[test]
    fn a_configured_default_agent_that_is_missing_is_named_rather_than_substituted() {
        let registry = registry_with_an_installed_agent();
        let mut workspace = workspace_with_shell(Some("/bin/zsh"));
        workspace.default_agent = Some("turn-absent-agent-xyz".to_string());
        let Some(PaneLaunch::Unhosted { note, .. }) = pane_launch(
            PaneKind::Agent,
            None,
            Some(&workspace),
            default_shell(Some(&workspace)),
            &registry,
        ) else {
            panic!("a missing default agent must not leave the pane empty")
        };
        assert!(note.contains("turn-absent-agent-xyz"), "{note}");
        assert!(note.contains("PATH"), "{note}");
        assert!(
            !note.contains("\"sh\""),
            "running a different agent than the one configured would be a surprise: {note}"
        );

        // Configured and present: hosted, and the workspace's choice wins over the
        // registry's own first answer.
        workspace.default_agent = Some(" sh ".to_string());
        assert_eq!(
            pane_launch(
                PaneKind::Agent,
                None,
                Some(&workspace),
                default_shell(Some(&workspace)),
                &registry,
            ),
            Some(PaneLaunch::Hosted {
                shell: "/bin/zsh".to_string(),
                command: "sh".to_string()
            }),
            "surrounding whitespace is not part of the command"
        );
    }

    #[test]
    fn a_declared_command_always_wins_and_blank_ones_do_not_count() {
        let registry = AdapterRegistry::bare();
        let workspace = workspace_with_shell(Some("/bin/zsh"));
        assert_eq!(
            pane_launch(
                PaneKind::Shell,
                Some("fish"),
                Some(&workspace),
                default_shell(Some(&workspace)),
                &registry
            ),
            Some(PaneLaunch::Direct {
                command: "fish".to_string()
            })
        );
        assert_eq!(
            pane_launch(
                PaneKind::Shell,
                Some("   "),
                Some(&workspace),
                default_shell(Some(&workspace)),
                &registry
            ),
            Some(PaneLaunch::Direct {
                command: "/bin/zsh".to_string()
            }),
            "whitespace is not a command"
        );
    }

    /// The adapter decides what an agent is, and that decision decides the launch shape:
    /// an agent is hosted in a shell, and a job is the pane's process. `claude` typed
    /// into a plain terminal pane is an agent; `npm run dev` in an agent pane is not.
    #[test]
    fn a_command_the_registry_knows_as_an_agent_is_hosted_whatever_the_pane_is_called() {
        let registry = registry_with_an_installed_agent();
        assert_eq!(
            pane_launch(
                PaneKind::Terminal,
                Some("sh"),
                Some(&workspace_with_shell(Some("/bin/zsh"))),
                "/bin/zsh".to_string(),
                &registry
            ),
            Some(PaneLaunch::Hosted {
                shell: "/bin/zsh".to_string(),
                command: "sh".to_string()
            })
        );
        assert_eq!(
            pane_launch(
                PaneKind::Agent,
                Some("npm run dev"),
                Some(&workspace_with_shell(Some("/bin/zsh"))),
                "/bin/zsh".to_string(),
                &registry
            ),
            Some(PaneLaunch::Direct {
                command: "npm run dev".to_string()
            }),
            "a job's ending is the thing the pane exists to show, so nothing outlives it"
        );
    }

    #[test]
    fn the_first_installed_agent_is_the_strongest_integration_that_is_actually_there() {
        // The built-in registry: whatever it answers has to be a command that exists,
        // because the whole point is that the pane starts something.
        if let Some(agent) = first_installed_agent(&AdapterRegistry::with_builtin()) {
            assert!(
                turn_agents::adapter::which(&agent).is_some(),
                "{agent} was offered as a fallback but is not on PATH"
            );
        }
        // A registry with nothing installed offers nothing rather than guessing.
        assert_eq!(first_installed_agent(&AdapterRegistry::bare()), None);
        assert_eq!(
            first_installed_agent(&registry_with_an_installed_agent()).as_deref(),
            Some("sh")
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

    /// The one thing a shell in the middle must not be allowed to do: reinterpret what
    /// Turn injected. The real Claude Code adapter's `--settings` path and its
    /// `TURN_HOOK_URL` now travel through a shell, and Turn's own data directory can sit
    /// anywhere the user put it — under `My $HOME/`, with a backtick in the name. Every
    /// word has to arrive at the agent exactly as written, and nothing may run.
    #[test]
    fn the_real_claude_injection_survives_the_shell_that_now_hosts_it() {
        let temp = tempfile::tempdir().expect("a temporary directory");
        let scratch = temp.path().join("My $HOME/`id`/$(touch pwned)");
        let plan = turn_agents::ClaudeCodeAdapter::new()
            .prepare(&LaunchContext {
                session_id: SessionId::from_stored("sess_quoted"),
                node_id: NodeId::from_stored("proc_quoted"),
                cwd: temp.path().to_string_lossy().into_owned(),
                command: "claude".to_string(),
                user_args: vec!["--model".to_string(), "opus".to_string()],
                endpoint: turn_agents::HookEndpoint {
                    base_url: "http://127.0.0.1:51234".to_string(),
                    token: "to'ken$(id)".to_string(),
                    helper_path: None,
                },
                scratch_dir: scratch.clone(),
            })
            .expect("the adapter must write its settings");
        let settings = plan
            .args
            .last()
            .expect("the injected settings path is the last argument")
            .clone();
        assert!(
            settings.starts_with(&scratch.to_string_lossy().into_owned()),
            "the path Turn injected is one Turn generated: {settings}"
        );
        let hook_url = plan
            .env
            .iter()
            .find(|(name, _)| name == "TURN_HOOK_URL")
            .map(|(_, value)| value.clone())
            .expect("the hook URL travels in the environment");

        let line = hosting::command_line(&plan.command, &plan.args);
        // What a shell would hand on, taken apart by the inverse of the quoting.
        let words = shell_words::split(&line).expect("the line must be one a shell parses");
        let mut expected: Vec<String> = vec![plan.command.clone()];
        expected.extend(plan.args.iter().cloned());
        assert_eq!(words, expected, "the shell would change the launch");
        assert!(words.contains(&settings), "it survived verbatim: {words:?}");
        // The hook URL is not on the line at all, which is the point: it carries this
        // node's token, and a typed line reaches the screen, the scrollback and the
        // user's history file.
        assert!(
            !line.contains(&hook_url),
            "a token must not travel on an input line: {line}"
        );

        // And a real shell, given the same line with the agent swapped for `printf`,
        // prints those arguments and runs nothing that was hiding in them.
        let echo = hosting::command_line("printf", &plan.args);
        let script = echo.replacen("printf", "printf '%s\\n'", 1);
        let sandbox = tempfile::tempdir().expect("somewhere for a shell to litter");
        let output = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(&script)
            .current_dir(sandbox.path())
            .output()
            .expect("a shell must run");
        let printed = String::from_utf8_lossy(&output.stdout);
        assert!(
            printed.lines().any(|line| line == settings),
            "the shell passed on something else: {printed:?}"
        );
        assert!(
            !sandbox.path().join("pwned").exists(),
            "a command substitution escaped the quoting"
        );
    }

    /// A shell is a more general thing to launch than one command, so the rule that a
    /// process starts inside the Session's checkout is proved again for the hosted shape.
    /// Nothing is started when it fails: not the agent, and not the shell either.
    #[tokio::test]
    async fn a_shell_hosted_agent_is_refused_a_directory_outside_the_checkout() {
        let mut harness = Harness::new().await;
        harness.core.registry = registry_with_an_installed_agent();
        let root = harness._dir.path().join("contained-checkout");
        std::fs::create_dir(&root).expect("the checkout root");
        let workspace = Workspace::new("contained", root.to_string_lossy(), 1);
        harness.core.store.workspaces().save(&workspace).unwrap();
        harness
            .core
            .workspaces
            .insert(workspace.id.clone(), workspace.clone());

        // An agent pane whose directory climbs out of the checkout. `sh` is what this
        // registry calls an agent, so this is the hosted shape.
        let mut session = Session::new(
            workspace.id.clone(),
            "escapes",
            workspace.root.clone(),
            Layout::single(Pane::new(PaneKind::Agent).with_command("sh").with_cwd("..")),
            1,
        );
        session.mode = SessionMode::ReadOnly;
        session.read_only_enforced = true;
        session.checkout_id = CheckoutId::primary_for(&workspace.id);
        harness
            .core
            .store
            .hierarchy()
            .create_session(&session, 1)
            .unwrap();
        let pane = session.layout.panes()[0].id.clone();
        harness
            .core
            .sessions
            .insert(session.id.clone(), session.clone());

        let error = harness
            .core
            .materialise_pane(&session.id, &pane, 2)
            .expect_err("a directory outside the checkout is not a place to start a shell");
        assert_eq!(error.code, ErrorCode::Refused);
        assert!(
            error.message.contains("outside the Session checkout"),
            "{error}"
        );
        assert!(
            harness.core.processes.is_empty(),
            "the refusal has to come before anything is started"
        );
        assert!(harness.core.sessions[&session.id].tree.is_empty());

        // The same pane inside the checkout is allowed, and gets both nodes: the shell
        // the pane runs and the agent running in it.
        harness
            .core
            .sessions
            .get_mut(&session.id)
            .unwrap()
            .layout
            .get_mut(&pane)
            .unwrap()
            .cwd = None;
        let started = harness
            .core
            .materialise_pane(&session.id, &pane, 3)
            .expect("a contained directory starts")
            .expect("a hosted agent answers with the agent's node");
        let session = &harness.core.sessions[&session.id];
        let agent = session.tree.get(&started).expect("the agent node");
        assert_eq!(agent.kind, NodeKind::Agent);
        assert_eq!(agent.relation, Relation::Confirmed);
        let shell = agent.parent.clone().expect("the shell it runs in");
        assert_eq!(session.tree.get(&shell).unwrap().kind, NodeKind::Shell);
        assert_eq!(
            session.layout.get(&pane).unwrap().node_id.as_ref(),
            Some(&shell),
            "the pane shows the process it runs, which is the shell"
        );
        assert_eq!(
            harness.core.processes[&shell].hosted.as_ref(),
            Some(&started),
            "and Turn knows what it started in that shell"
        );

        // Each node says what it is. Caught by running the real daemon: the start event
        // writes its command onto the node, so a hosted launch that announced the agent's
        // command line left the shell node claiming to be the agent.
        let shell_node = session.tree.get(&shell).unwrap();
        assert_eq!(
            shell_node.command,
            default_shell(Some(&workspace)),
            "the shell node is the shell, whatever is running inside it"
        );
        assert_eq!(shell_node.args, vec!["-i".to_string()]);
        assert_eq!(shell_node.title, executable_name(&shell_node.command));
        assert_eq!(agent.command, "sh", "and the agent node is the agent");

        // Both directories are the checkout root, resolved rather than taken as written.
        let canonical = std::fs::canonicalize(&root).unwrap();
        assert_eq!(agent.cwd, canonical.to_string_lossy());
        assert_eq!(shell_node.cwd, canonical.to_string_lossy());
    }

    #[tokio::test]
    async fn a_recovery_lease_stops_agents_and_commands_but_not_a_terminal() {
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
        let pane_id = session.layout.panes()[0].id.clone();
        let missing_host_lock = harness
            .core
            .require_session_launch_allowed(&session.id, LaunchAuthority::CheckoutWrite)
            .expect_err("SQLite authority alone must never launch a writer");
        assert_eq!(missing_host_lock.code, ErrorCode::Refused);
        assert!(missing_host_lock.message.contains("host-wide"));
        harness
            .core
            .require_session_launch_allowed(&session.id, LaunchAuthority::InteractiveShell)
            .expect("an interactive shell does not inherit checkout write authority");
        harness
            .core
            .install_checkout_write_lock(&session.id, &lease, checkout_lock);
        harness
            .core
            .require_session_launch_allowed(&session.id, LaunchAuthority::CheckoutWrite)
            .unwrap();

        assert!(harness
            .core
            .store
            .hierarchy()
            .require_recovery(&lease.id, 2)
            .unwrap());

        let error = harness
            .core
            .require_session_launch_allowed(&session.id, LaunchAuthority::CheckoutWrite)
            .expect_err("recovery-required is not authority to run anything at the checkout");
        assert_eq!(error.code, ErrorCode::Refused);
        assert!(error
            .detail
            .unwrap()
            .contains("requires write-lease reconciliation"));

        // The other half of the rule, and the reason this Session is still usable while
        // it waits: opening the user's own shell writes nothing to the checkout, so it is
        // not gated on write access — including the shell they need to go and stop the
        // process the confirmation is asking them about.
        assert_eq!(
            harness.core.pane_launch_authority(&session.id, &pane_id),
            LaunchAuthority::InteractiveShell
        );
        harness
            .core
            .require_session_launch_allowed(&session.id, LaunchAuthority::InteractiveShell)
            .expect("a terminal is not a write to the shared checkout");
    }

    /// The classification part 3 rests on. Anything that is not plainly "the user's own
    /// shell, with nothing in it" has to keep needing confirmed write access, because the
    /// alternative is Turn running a build or an agent in a checkout that may still have
    /// a writer.
    #[test]
    fn only_a_bare_interactive_shell_escapes_the_checkout_write_gate() {
        let shell = |command: &str| PaneLaunch::Direct {
            command: command.to_string(),
        };
        assert_eq!(
            launch_authority(&shell("/bin/zsh"), PaneKind::Shell, &[]),
            LaunchAuthority::InteractiveShell
        );
        assert_eq!(
            launch_authority(
                &PaneLaunch::Unhosted {
                    shell: "/bin/zsh".into(),
                    note: "no agent CLI is installed".into(),
                },
                PaneKind::Agent,
                &[]
            ),
            LaunchAuthority::InteractiveShell,
            "an agent pane that could not start an agent is a shell with a sentence in it"
        );

        for (launch, kind, args) in [
            (shell("cargo test"), PaneKind::Shell, Vec::new()),
            (shell("npm run dev"), PaneKind::Terminal, Vec::new()),
            // A shell handed a script is a command wearing a shell's name.
            (
                shell("/bin/sh"),
                PaneKind::Shell,
                vec!["-c".to_string(), "rm -rf target".to_string()],
            ),
            (
                shell("/bin/sh -c 'make release'"),
                PaneKind::Shell,
                Vec::new(),
            ),
            // A shell pane is a place to type; a terminal pane running one is a job.
            (shell("/bin/zsh"), PaneKind::Terminal, Vec::new()),
            (
                PaneLaunch::Hosted {
                    shell: "/bin/zsh".into(),
                    command: "claude".into(),
                },
                PaneKind::Agent,
                Vec::new(),
            ),
        ] {
            assert_eq!(
                launch_authority(&launch, kind, &args),
                LaunchAuthority::CheckoutWrite,
                "{launch:?} in a {kind:?} pane with {args:?}"
            );
        }
    }
}
