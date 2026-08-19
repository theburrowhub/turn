//! Processes ending, and the guesses made about the ones we never held.

use crate::core::{Core, DeferredRuntimeInput, FINISHED_PTY_RETENTION_MS};
use std::collections::HashSet;
use turn_agents::IntegrationLevel;
use turn_core::event::{Confidence, EventKind, EventSource, TurnEvent};
use turn_core::ids::{NodeId, PaneId, SessionId};
use turn_core::model::{NodeKind, PaneKind, Relation};
use turn_core::state::Lifecycle;
use turn_pty::ExitInfo;

impl Core {
    /// Records a process ending.
    ///
    /// The pty handle is kept for now. Its buffer still holds what the process printed,
    /// and a user whose build just failed wants to read the error — throwing the screen
    /// away the moment the process dies would be the least useful possible moment. It is
    /// let go later, by [`Self::reap_finished_processes`], once nobody is watching and
    /// enough time has passed that nobody is about to.
    pub(crate) fn node_exited(&mut self, node: &NodeId, info: ExitInfo, now_ms: i64) {
        let Some(process) = self.processes.get_mut(node) else {
            return;
        };
        if process.exited_ms.is_some() {
            // Already accounted for — a watcher's report arriving after
            // [`Self::reap_finished_processes`] noticed the same death. Recording it twice
            // would put two endings in the log for one process.
            return;
        }
        let session_id = process.session_id.clone();
        // When the retention window starts, for the reaper below.
        process.exited_ms = Some(now_ms);
        if let Some(token) = process.hook_token.take() {
            // Nothing may report as this node any more.
            self.hooks.unregister(&token);
        }
        if let Some(pump) = self.pumps.remove(node) {
            pump.abort();
        }

        self.record_exit(&session_id, node, info, now_ms);
        self.refresh_checkout_lock_owner(&session_id);
    }

    /// Lets go of the ptys of processes that have ended and nobody is watching.
    ///
    /// Without this the daemon's memory is a function of everything that has ever run
    /// in it: every exited pane, and every start-up command — which has no pane at all,
    /// so no close and no relaunch would ever have reclaimed it. Thirty sessions over
    /// several days is precisely the shape the product promises to survive.
    ///
    /// A process being watched is never reclaimed, however long ago it ended: somebody
    /// has that terminal on screen.
    ///
    /// A death nobody reported is recorded here first. Letting the pty go while the node
    /// still claims to be running would leave the daemon asserting something it has no
    /// evidence for and no handle behind — the one thing the restore path is built to
    /// avoid — and the pty knows how the process ended, so there is nothing to guess.
    pub(crate) fn reap_finished_processes(&mut self, now_ms: i64) {
        // Ended, and never written down: the exit watcher lost its channel, or its task
        // was gone before the status arrived. Collected first because recording an exit
        // takes `&mut self`.
        let unreported: Vec<(NodeId, ExitInfo)> = self
            .processes
            .iter()
            .filter(|(_, process)| process.exited_ms.is_none())
            .filter_map(|(node, process)| {
                // `None` while it runs; `Some` from the moment it does not, which is the
                // same fact `is_running` reports and the status we need with it.
                process.pty.exit_info().map(|info| (node.clone(), info))
            })
            .collect();
        for (node, info) in unreported {
            tracing::debug!(%node, "a process ended without its exit being reported");
            // The ordinary path: it stamps `exited_ms`, revokes the node's hook token and
            // writes the lifecycle, the exit code and the event.
            self.node_exited(&node, info, now_ms);
        }

        let mut finished: Vec<NodeId> = Vec::new();
        for (node, process) in self.processes.iter_mut() {
            if process.pty.is_running() {
                continue;
            }
            // Every ended process was stamped above — a pty that is not running always
            // has its status — so this is a fallback that keeps the retention clock
            // starting now rather than never, without an unwrap.
            let ended = *process.exited_ms.get_or_insert(now_ms);
            if now_ms.saturating_sub(ended) >= FINISHED_PTY_RETENTION_MS {
                finished.push(node.clone());
            }
        }

        for node in finished {
            if self.is_watched(&node) {
                continue;
            }
            self.discard_process(&node);
            tracing::debug!(%node, "let go of a finished process's terminal");
        }
    }

    /// Writes an exit into the tree and the event log.
    ///
    /// Separate from [`Self::node_exited`] because a pane being closed takes the pty with
    /// it: there is then no handle left to look the session up through, and the exit still
    /// has to be recorded rather than leaving a node that claims to be running.
    pub(crate) fn record_exit(
        &mut self,
        session_id: &SessionId,
        node: &NodeId,
        info: ExitInfo,
        now_ms: i64,
    ) {
        if !self.failed_ingest_checkpoints.is_empty() {
            self.deferred_runtime_inputs
                .push_back(DeferredRuntimeInput::Exit {
                    session_id: session_id.clone(),
                    node_id: node.clone(),
                    info,
                    now_ms,
                });
            return;
        }
        let session_id = session_id.clone();
        // Spent whether or not it still applies — the exit it was waiting for has now
        // happened, one way or another — but it only *excuses* the exit while the stop
        // request can plausibly explain it. A `SIGTERM` the process caught and ignored
        // an hour ago explains nothing about the crash that finally killed it, and
        // filing that crash as a deliberate stop would raise no failure at all.
        let expected = self
            .expected_exits
            .remove(node)
            .is_some_and(|applies_until| now_ms <= applies_until);
        let lifecycle = exit_lifecycle(&info, expected);

        if let Some(session) = self.sessions.get_mut(&session_id) {
            if let Some(node) = session.tree.get_mut(node) {
                node.lifecycle = lifecycle.clone();
                node.ended_ms = Some(now_ms);
                node.exit_code = Some(info.code);
            }
        }

        tracing::info!(
            %node, session = %session_id, code = info.code, signal = ?info.signal,
            expected, "a process ended"
        );

        // A process the user asked to stop has not failed, so it does not raise the
        // failure trigger and does not notify them about something they did on purpose.
        let kind = if expected || (info.code == 0 && info.signal.is_none()) {
            EventKind::ProcessExited { code: info.code }
        } else {
            EventKind::ProcessFailed {
                // `None` for a signal death: `portable-pty` reports one with a
                // meaningless exit code of 1 (ADR-010), and passing that on would
                // record a status the process never returned.
                code: info.signal.is_none().then_some(info.code),
                // The event vocabulary types this as a signal *number*, and there is no
                // number to put in it — see `signal_note` for where the name goes.
                signal: None,
            }
        };
        let mut event = TurnEvent::new(
            session_id.clone(),
            kind,
            EventSource::Supervisor,
            Confidence::Explicit,
            now_ms,
        )
        .with_node(node.clone());
        if let Some(note) = signal_note(&info) {
            event = event.with_raw(note);
        }
        self.ingest(event, now_ms);
        self.request_sweep(now_ms);
    }

    /// Records that an agent Turn started inside a terminal went with that terminal.
    ///
    /// [`Lifecycle::Lost`] because that is what happened: Turn closed the terminal the
    /// agent was reading from and never saw the agent exit, so there is no exit code and
    /// none is invented. `Lost` is terminal without being a failure, which is the right
    /// reading for a pane the user closed.
    pub(crate) fn record_hosted_loss(
        &mut self,
        session_id: &SessionId,
        hosted: &NodeId,
        now_ms: i64,
    ) -> bool {
        let retired = match self.sessions.get_mut(session_id) {
            Some(session) => match session
                .tree
                .get_mut(hosted)
                .filter(|node| node.is_running())
            {
                Some(node) => {
                    node.lifecycle = Lifecycle::Lost;
                    node.ended_ms = Some(now_ms);
                    super::clear_interaction_state(node);
                    (
                        node.id.clone(),
                        node.parent.clone(),
                        node.agent.as_ref().and_then(|agent| {
                            agent
                                .external_id
                                .clone()
                                .or_else(|| agent.agent.external_id.clone())
                        }),
                    )
                }
                None => return false,
            },
            None => return false,
        };
        let returned_to_runtime = retired.1.as_ref().is_some_and(|runtime| {
            self.release_terminal_subject(session_id, hosted, runtime, true)
        });
        // Whatever the agent itself had reported — subagents, a pending permission — went
        // with it, and the demands it raised stop being answerable.
        let mut gone = vec![retired];
        gone.extend(self.mark_runtime_dependents(session_id, hosted, now_ms));
        self.resolve_lifecycle_attention(session_id, &gone, now_ms);
        returned_to_runtime
    }

    /// Makes an Agent observed in a Shell-owned foreground process group the semantic
    /// subject of that terminal.
    ///
    /// This records presentation and input routing only. In particular it never writes
    /// `Process::hosted`: foreground ownership grants no stop or relaunch authority,
    /// whether the Agent was launched by Turn or merely found in the process table.
    pub(crate) fn record_observed_terminal_subject(
        &mut self,
        session_id: &SessionId,
        runtime: &NodeId,
        subject: &NodeId,
        adapter_id: &str,
    ) -> bool {
        let subject_is_live_agent = self
            .sessions
            .get(session_id)
            .and_then(|session| session.tree.get(subject))
            .is_some_and(|node| node.kind == NodeKind::Agent && node.is_running());
        if !subject_is_live_agent {
            return false;
        }

        let (previous, foreground_pane) =
            {
                let Some(process) = self.processes.get_mut(runtime).filter(|process| {
                    process.session_id == *session_id && process.pty.is_running()
                }) else {
                    return false;
                };
                if process.observed_subject.as_ref() == Some(subject) {
                    return false;
                }
                let previous = process.observed_subject.replace(subject.clone());
                // These fields describe low-latency observation of the foreground
                // terminal subject. `hosted` and `hook_token` retain the independent
                // lifecycle authority for an Agent Turn launched even while it is in the
                // background.
                if process.hosted.as_ref() == Some(subject) {
                    process.adapter_id = process
                        .hosted_adapter_id
                        .clone()
                        .unwrap_or_else(|| adapter_id.to_string());
                    process.level = process.hosted_level.unwrap_or(IntegrationLevel::Heuristic);
                    process.heuristic = (process.level == IntegrationLevel::Heuristic)
                        .then(turn_agents::OutputHeuristic::new);
                } else {
                    process.adapter_id = adapter_id.to_string();
                    process.level = IntegrationLevel::Heuristic;
                    process.heuristic = Some(turn_agents::OutputHeuristic::new());
                }
                (previous, process.foreground_pane.clone())
            };

        let mut stale_exact_panes = previous
            .as_ref()
            .map(|previous| self.exact_temporary_panes(session_id, previous))
            .unwrap_or_default();
        {
            let Some(session) = self.sessions.get_mut(session_id) else {
                return false;
            };
            let pane_ids = session
                .layout
                .panes()
                .into_iter()
                .map(|pane| pane.id.clone())
                .collect::<Vec<_>>();
            for pane_id in pane_ids {
                let Some(pane) = session.layout.get_mut(&pane_id) else {
                    continue;
                };
                let follows_foreground = foreground_pane.as_ref() == Some(&pane_id)
                    && (pane.node_id.as_ref() == Some(runtime)
                        || previous
                            .as_ref()
                            .is_some_and(|previous| pane.node_id.as_ref() == Some(previous)));
                if follows_foreground {
                    pane.node_id = Some(subject.clone());
                } else if previous
                    .as_ref()
                    .is_some_and(|previous| pane.node_id.as_ref() == Some(previous))
                {
                    // An exact view keeps A's identity when its Shell moves to B. It
                    // must stop consuming that Shell's feed before B writes a byte;
                    // Automatic falls back to the truthful semantic details surface.
                    pane.detect_kind(PaneKind::ProcessDetails);
                    stale_exact_panes.insert(pane_id.clone());
                }
                if pane.node_id.as_ref() == Some(subject) {
                    // A durable exact view may already exist as ProcessDetails because
                    // this Agent was in the background. Foreground ownership gives it
                    // an attachable terminal again; manual display overrides remain.
                    pane.detect_kind(PaneKind::Agent);
                }
            }
        }
        for pane_id in stale_exact_panes {
            self.detach_everyone(session_id, &pane_id);
        }
        true
    }

    /// Gives a live Shell-owned terminal back after its semantic foreground subject
    /// ends. This changes presentation/binding only; it grants no lifecycle authority.
    pub(crate) fn release_terminal_subject(
        &mut self,
        session_id: &SessionId,
        subject: &NodeId,
        runtime: &NodeId,
        release_hosted: bool,
    ) -> bool {
        let (foreground_pane, was_foreground, retired_token) = {
            let Some(process) = self.processes.get_mut(runtime).filter(|process| {
                process.session_id == *session_id
                    && process.pty.is_running()
                    && (process.observed_subject.as_ref() == Some(subject)
                        || (release_hosted && process.hosted.as_ref() == Some(subject)))
            }) else {
                return false;
            };
            let retired_token = if release_hosted && process.hosted.as_ref() == Some(subject) {
                process.hosted = None;
                process.hosted_adapter_id = None;
                process.hosted_level = None;
                process.hook_token.take()
            } else {
                None
            };
            let was_foreground = process.observed_subject.as_ref() == Some(subject);
            if was_foreground {
                process.observed_subject = None;
            }
            if process.observed_subject.is_none() {
                process.adapter_id = "generic-terminal".into();
                process.level = IntegrationLevel::GenericTerminal;
                process.heuristic = None;
                process.fallback_agent_name = None;
            }
            (
                process.foreground_pane.clone(),
                was_foreground,
                retired_token,
            )
        };
        self.revoke(retired_token.as_deref());
        if !was_foreground {
            // Lifecycle authority can end while the Agent is already in the
            // background. Its exact views were fenced when foreground changed.
            return false;
        }

        let mut stale_exact_panes = self.exact_temporary_panes(session_id, subject);
        let mut changed = !stale_exact_panes.is_empty();
        {
            let Some(session) = self.sessions.get_mut(session_id) else {
                return false;
            };
            let pane_ids = session
                .layout
                .panes()
                .into_iter()
                .map(|pane| pane.id.clone())
                .collect::<Vec<_>>();
            for pane_id in pane_ids {
                let Some(pane) = session.layout.get_mut(&pane_id) else {
                    continue;
                };
                if foreground_pane.as_ref() == Some(&pane_id)
                    && pane.node_id.as_ref() == Some(subject)
                {
                    pane.node_id = Some(runtime.clone());
                    pane.detect_kind(PaneKind::Shell);
                    changed = true;
                } else if pane.node_id.as_ref() == Some(subject) {
                    // This Pane is an exact view of the Agent, not another follower of
                    // the Shell. Keep that binding, retire the shared PTY feed and let
                    // Automatic render the Agent's durable details.
                    pane.detect_kind(PaneKind::ProcessDetails);
                    stale_exact_panes.insert(pane_id.clone());
                    changed = true;
                }
            }
        }
        for pane_id in stale_exact_panes {
            self.detach_everyone(session_id, &pane_id);
        }
        changed
    }

    /// Every temporary exact view of one semantic subject.
    ///
    /// Temporary Panes do not live in `Layout`, but their attachments are just as
    /// capable of consuming a shared Shell feed. A foreground hand-off must therefore
    /// fence them alongside durable exact views. If the binding index is unavailable,
    /// fail closed by detaching every non-Layout attachment in this Session; briefly
    /// refreshing an unrelated preview is safer than ever showing Agent B as Agent A.
    fn exact_temporary_panes(&self, session_id: &SessionId, subject: &NodeId) -> HashSet<PaneId> {
        match self.store.hierarchy().bindings_for_session(session_id) {
            Ok(bindings) => bindings
                .into_iter()
                .filter(|binding| binding.temporary && binding.node_id == *subject)
                .map(|binding| binding.pane_id)
                .collect(),
            Err(error) => {
                tracing::warn!(
                    %error,
                    session = %session_id,
                    node = %subject,
                    "could not read temporary Pane bindings during terminal hand-off; detaching all ephemeral feeds"
                );
                let durable = self
                    .sessions
                    .get(session_id)
                    .map(|session| {
                        session
                            .layout
                            .panes()
                            .into_iter()
                            .map(|pane| pane.id.clone())
                            .collect::<HashSet<_>>()
                    })
                    .unwrap_or_default();
                self.clients
                    .values()
                    .flat_map(|client| client.attachments.keys())
                    .filter(|(owner, pane)| owner == session_id && !durable.contains(pane))
                    .map(|(_, pane)| pane.clone())
                    .collect()
            }
        }
    }

    /// Applies only the in-memory part of dependent retirement. Event ingestion
    /// uses this form so its normal session/queue checkpoint and push remain the
    /// single publication boundary for a SubagentStop.
    pub(in crate::core) fn mark_runtime_dependents(
        &mut self,
        session_id: &SessionId,
        parent: &NodeId,
        now_ms: i64,
    ) -> Vec<(NodeId, Option<NodeId>, Option<String>)> {
        self.supervisor.refresh();
        let Some(session) = self.sessions.get_mut(session_id) else {
            return Vec::new();
        };
        let mut children = Vec::new();
        let mut stack: Vec<NodeId> = session
            .tree
            .children(parent)
            .into_iter()
            .map(|node| node.id.clone())
            .rev()
            .collect();
        while let Some(node_id) = stack.pop() {
            let Some(node) = session.tree.get(&node_id) else {
                continue;
            };

            // A living PID is an independent observation channel. Neither it
            // nor its descendants depend on the runtime that just died.
            if node.is_running() && node.pid.is_some_and(|pid| self.supervisor.is_alive(pid)) {
                continue;
            }

            let child_ids: Vec<NodeId> = session
                .tree
                .children(&node_id)
                .into_iter()
                .map(|child| child.id.clone())
                .rev()
                .collect();
            stack.extend(child_ids);

            // A PID the supervisor confirms is gone is lost regardless of how
            // confidently its parent edge was learned. Virtual subagents and
            // inferred PID-less nodes also relied on the dead owner's channel.
            let depends_on_owner = node.pid.is_some()
                || node.kind == NodeKind::Subagent
                || node.relation == Relation::Inferred
                // An agent Turn started inside the owner's shell, before the process
                // table was swept and its own pid became known: the terminal that just
                // died is the only thing it was ever running in. Without this it would
                // claim to be running for as long as its session lived, and no sweep
                // would ever correct it because there is no pid to look for.
                || (node.relation == Relation::Confirmed
                    && node.pid.is_none()
                    && !self.processes.contains_key(&node.id));
            if node.is_running() && depends_on_owner {
                children.push((
                    node.id.clone(),
                    node.parent.clone(),
                    node.agent.as_ref().and_then(|agent| {
                        agent
                            .external_id
                            .clone()
                            .or_else(|| agent.agent.external_id.clone())
                    }),
                ));
            }
        }
        if children.is_empty() {
            return children;
        }
        for (child, _, _) in &children {
            if let Some(node) = session.tree.get_mut(child) {
                // `Lost`, not `Exited`: we never held this process and did not see it
                // end. Claiming a clean exit would be inventing an exit code.
                node.lifecycle = Lifecycle::Lost;
                node.ended_ms = Some(now_ms);
                super::clear_interaction_state(node);
            }
        }
        children
    }

    /// Notices when an agent running in a pane's shell has ended.
    ///
    /// Turn holds no handle on it — the shell forked it — so there is no exit watcher and
    /// nothing pushes its death. The alternative to noticing is a tree that shows an
    /// agent as running for as long as its shell lives, which is precisely the pane the
    /// user is looking at after typing `/exit`.
    ///
    /// This is not the process-table sweep and must not become it: it asks the kernel
    /// about one pid Turn already knows, which is a single syscall per hosted agent, not
    /// a scan of every process on the machine. That is what makes it affordable on every
    /// tick where a full refresh would not be.
    pub(crate) fn observe_hosted_agents(&mut self, now_ms: i64) {
        let ended: Vec<(SessionId, NodeId)> = self
            .processes
            .values()
            .filter(|process| process.pty.is_running())
            .filter_map(|process| {
                let hosted = process.hosted.as_ref()?;
                let node = self.sessions.get(&process.session_id)?.tree.get(hosted)?;
                // No pid yet means the sweep has not identified it, which says nothing
                // about whether it is alive. Only a pid Turn knows can be asked about.
                let pid = node.pid.filter(|_| node.is_running())?;
                (!pid_exists(pid)).then(|| (process.session_id.clone(), hosted.clone()))
            })
            .collect();
        for (session_id, node_id) in ended {
            tracing::debug!(
                %session_id, %node_id,
                "an agent running in a pane's shell has ended; the shell is still there"
            );
            let layout_changed = self.record_hosted_loss(&session_id, &node_id, now_ms);
            self.persist_session_quietly(&session_id);
            if layout_changed {
                self.bump_hierarchy();
                self.push_layout(&session_id, None);
            }
            self.push_tree(&session_id, now_ms);
            self.push_node_state(&session_id, &node_id, None, now_ms);
            self.push_session_state(&session_id, now_ms);
        }
    }

    /// Runs output inference for the panes that have it, and feeds anything it
    /// concludes through the same pipeline as a hook callback.
    pub(crate) fn observe_heuristics(&mut self, now_ms: i64) {
        let mut inferred = Vec::new();
        for (node, process) in self.processes.iter_mut() {
            let Some(heuristic) = process.heuristic.as_mut() else {
                continue;
            };
            if !process.pty.is_running() {
                continue;
            }
            let Some(snapshot) = process.pty.snapshot() else {
                continue;
            };
            let ctx = turn_agents::EventContext {
                session_id: process.session_id.clone(),
                // The screen belongs to the pty, but what is inferred from it belongs to
                // the agent drawing on it. Attributing a hosted agent's inferred state
                // to the shell around it would give the shell a turn axis and leave the
                // agent's own permanently unknown.
                node_id: process
                    .observed_subject
                    .clone()
                    .or_else(|| process.hosted.clone())
                    .unwrap_or_else(|| node.clone()),
                timestamp_ms: now_ms,
            };
            inferred.extend(heuristic.observe(&snapshot, now_ms, &ctx));
        }
        for event in inferred {
            self.ingest(event, now_ms);
        }
    }
}

/// Whether a pid is still a process, asked of the kernel rather than of a snapshot.
///
/// Signal zero performs every permission check and delivers nothing, which is the
/// portable way to ask this question. `EPERM` means the process exists and belongs to
/// somebody else — still a process, so still alive.
#[cfg(unix)]
fn pid_exists(pid: u32) -> bool {
    // Safe: `kill` takes two integers and touches no memory we own.
    if unsafe { libc::kill(pid as libc::pid_t, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Without a way to ask, nothing is claimed: a node keeps the state it was last told
/// about rather than being retired on a guess.
#[cfg(not(unix))]
fn pid_exists(_pid: u32) -> bool {
    true
}

/// The lifecycle to record for an exit, given whether the user asked for it.
///
/// A signal death Turn *requested* is recorded as a clean end, and that is the whole
/// point: [`turn_core::state::DisplayState`] derives `Failed` from any signal, so
/// leaving it as [`Lifecycle::Signaled`] paints a red "failed" row for something the
/// user did deliberately — and a badge that fires when nothing is wrong teaches people
/// to ignore the one that matters. Nothing is lost by it. The exit code the platform
/// reported stays on the node, and the signal's own name is in the event log.
///
/// Only signal deaths are neutralised. A process the user asked to stop that chose to
/// exit non-zero on its way out keeps that status, because that is the process's own
/// word about itself rather than an artefact of how it was stopped — `portable-pty`
/// reports a signal death with a meaningless code of 1 (ADR-010), and
/// [`Core::stop_and_release`](crate::core::Core::stop_and_release) synthesises 137 or
/// 143 for a pty it closed, so in the signalled case there is no real status to keep.
fn exit_lifecycle(info: &ExitInfo, expected: bool) -> Lifecycle {
    // A stop the user asked for is recorded as `Stopped`, which is not a failure,
    // rather than rewritten into a clean exit. Fabricating `Exited { code: 0 }`
    // would have made a deliberate stop indistinguishable from a process that
    // really did exit successfully, and would have discarded the signal name —
    // exactly the invented information ADR-010 refuses.
    match (&info.signal, expected) {
        (Some(signal), true) => Lifecycle::Stopped {
            signal: signal.clone(),
        },
        _ => info.lifecycle(),
    }
}

/// How a signalled process died, for the event log.
///
/// [`EventKind::ProcessFailed`] types its `signal` as a number and the platform gives a
/// name ("Killed", "Terminated") — ADR-010 is explicit that converting one to the other
/// would invent information — so the name travels in the event's `raw` field, which
/// exists for exactly this: the source's own account, kept verbatim. Without it the log
/// cannot say whether a process was killed or merely exited, which is the difference
/// between two very different mornings.
fn signal_note(info: &ExitInfo) -> Option<String> {
    let signal = info.signal.as_deref()?;
    Some(format!("signal={signal} code={}", info.code))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::clients::Attachment;
    use crate::core::testing::Harness;
    use crate::core::{Command, FailedIngestCheckpoint};
    use turn_core::attention::AttentionDemandKind;
    use turn_core::ids::{PaneId, SessionId};
    use turn_core::model::ProcessNode;

    const NOW: i64 = 1_775_000_000_000;

    #[tokio::test]
    async fn hosted_lifecycle_authority_survives_foreground_agent_handoffs() {
        let mut harness = Harness::new().await;
        let session_id = SessionId::from_stored("sess_hosted_foreground_handoff");
        let pane_id = PaneId::from_stored("pane_hosted_foreground_handoff");
        harness.add_session(session_id.clone(), pane_id.clone(), NOW);
        let runtime = harness.spawn_process(&session_id, &pane_id, NOW).await;

        let mut hosted = ProcessNode::agent(session_id.clone(), "claude", "/tmp", NOW);
        hosted.lifecycle = Lifecycle::Alive;
        hosted.link_to(runtime.clone(), Relation::Confirmed);
        let hosted_id = hosted.id.clone();
        let mut foreground = ProcessNode::agent(session_id.clone(), "codex", "/tmp", NOW);
        foreground.lifecycle = Lifecycle::Alive;
        foreground.link_to(runtime.clone(), Relation::Inferred);
        let foreground_id = foreground.id.clone();
        {
            let session = harness.core.sessions.get_mut(&session_id).unwrap();
            session.tree.get_mut(&runtime).unwrap().kind = NodeKind::Shell;
            session.tree.insert(hosted);
            session.tree.insert(foreground);
            session.layout.get_mut(&pane_id).unwrap().node_id = Some(hosted_id.clone());
        }
        {
            let process = harness.core.processes.get_mut(&runtime).unwrap();
            process.hosted = Some(hosted_id.clone());
            process.hosted_adapter_id = Some("claude-code".into());
            process.hosted_level = Some(IntegrationLevel::Heuristic);
            process.observed_subject = Some(hosted_id.clone());
            process.adapter_id = "claude-code".into();
            process.level = IntegrationLevel::Heuristic;
            process.heuristic = Some(turn_agents::OutputHeuristic::new());
            process.hook_token = Some("hook-a".into());
        }

        assert!(harness
            .core
            .release_terminal_subject(&session_id, &hosted_id, &runtime, false,));
        let process = &harness.core.processes[&runtime];
        assert_eq!(process.hosted.as_ref(), Some(&hosted_id));
        assert_eq!(process.hook_token.as_deref(), Some("hook-a"));
        assert!(process.observed_subject.is_none());
        assert_eq!(process.level, IntegrationLevel::GenericTerminal);
        assert!(process.heuristic.is_none());
        assert!(harness.core.terminal_node(&hosted_id).is_none());

        assert!(harness.core.record_observed_terminal_subject(
            &session_id,
            &runtime,
            &foreground_id,
            "codex",
        ));
        let process = &harness.core.processes[&runtime];
        assert_eq!(process.hosted.as_ref(), Some(&hosted_id));
        assert_eq!(process.observed_subject.as_ref(), Some(&foreground_id));
        assert_eq!(process.adapter_id, "codex");
        assert_eq!(process.level, IntegrationLevel::Heuristic);
        assert!(process.heuristic.is_some());
        assert_eq!(
            harness.core.terminal_node(&foreground_id),
            Some(runtime.clone())
        );
        assert!(harness.core.terminal_node(&hosted_id).is_none());

        let hook = TurnEvent::new(
            session_id.clone(),
            EventKind::AgentIdle,
            EventSource::Hook {
                tool: "claude-code".into(),
                event_name: "Stop".into(),
            },
            Confidence::Explicit,
            NOW + 1,
        )
        .with_node(hosted_id.clone());
        harness.core.promote_authenticated_integration(&hook);
        let process = &harness.core.processes[&runtime];
        assert_eq!(process.hosted_level, Some(IntegrationLevel::Structured));
        assert_eq!(process.adapter_id, "codex");
        assert_eq!(process.level, IntegrationLevel::Heuristic);
        assert!(process.heuristic.is_some());

        assert!(harness.core.release_terminal_subject(
            &session_id,
            &foreground_id,
            &runtime,
            false,
        ));
        assert!(harness.core.record_observed_terminal_subject(
            &session_id,
            &runtime,
            &hosted_id,
            "claude-code",
        ));
        let process = &harness.core.processes[&runtime];
        assert_eq!(process.hosted.as_ref(), Some(&hosted_id));
        assert_eq!(process.observed_subject.as_ref(), Some(&hosted_id));
        assert_eq!(process.adapter_id, "claude-code");
        assert_eq!(process.level, IntegrationLevel::Structured);
        assert!(process.heuristic.is_none());
        assert_eq!(process.hook_token.as_deref(), Some("hook-a"));
        assert_eq!(harness.core.terminal_node(&hosted_id), Some(runtime));
    }

    #[tokio::test]
    async fn a_confirmed_child_with_a_dead_pid_is_lost_with_its_runtime_owner() {
        let mut harness = Harness::new().await;
        let session = SessionId::from_stored("sess_dead_confirmed_child");
        harness.add_session(session.clone(), PaneId::new(), NOW);
        let mut parent = ProcessNode::agent(session.clone(), "claude", "/tmp", NOW);
        parent.lifecycle = Lifecycle::Alive;
        let parent_id = parent.id.clone();
        let mut child = ProcessNode::agent(session.clone(), "worker", "/tmp", NOW);
        child.kind = NodeKind::Subagent;
        child.lifecycle = Lifecycle::Alive;
        child.pid = Some(u32::MAX);
        child.link_to(parent_id.clone(), Relation::Confirmed);
        let child_id = child.id.clone();
        let tree = &mut harness.core.sessions.get_mut(&session).unwrap().tree;
        tree.insert(parent);
        tree.insert(child);

        let retired = harness
            .core
            .mark_runtime_dependents(&session, &parent_id, NOW + 1);
        assert_eq!(retired.len(), 1);
        assert_eq!(retired[0].0, child_id);
        assert_eq!(
            harness.core.sessions[&session]
                .tree
                .get(&child_id)
                .unwrap()
                .lifecycle,
            Lifecycle::Lost
        );
    }

    #[tokio::test]
    async fn a_process_exit_checkpoints_virtual_descendants_with_the_owner() {
        let mut harness = Harness::new().await;
        let session = SessionId::from_stored("sess_owner_exit_checkpoint");
        harness.add_session(session.clone(), PaneId::new(), NOW);
        let mut parent = ProcessNode::agent(session.clone(), "claude", "/tmp", NOW);
        parent.lifecycle = Lifecycle::Alive;
        let parent_id = parent.id.clone();
        let mut child = ProcessNode::agent(session.clone(), "reviewer", "/tmp", NOW);
        child.kind = NodeKind::Subagent;
        child.lifecycle = Lifecycle::Alive;
        child.link_to(parent_id.clone(), Relation::Confirmed);
        let child_id = child.id.clone();
        {
            let tree = &mut harness.core.sessions.get_mut(&session).unwrap().tree;
            tree.insert(parent);
            tree.insert(child);
        }
        harness.core.persist_session(&session).unwrap();

        harness.core.record_exit(
            &session,
            &parent_id,
            ExitInfo {
                code: 0,
                signal: None,
            },
            NOW + 1,
        );

        assert_eq!(
            harness.core.sessions[&session]
                .tree
                .get(&child_id)
                .unwrap()
                .lifecycle,
            Lifecycle::Lost
        );
        let durable = harness
            .core
            .store
            .sessions()
            .get(&session)
            .unwrap()
            .unwrap();
        assert_eq!(
            durable.tree.get(&child_id).unwrap().lifecycle,
            Lifecycle::Lost
        );
    }

    #[tokio::test]
    async fn a_live_pid_is_a_boundary_for_its_virtual_descendants() {
        let mut harness = Harness::new().await;
        let session = SessionId::from_stored("sess_live_runtime_boundary");
        harness.add_session(session.clone(), PaneId::new(), NOW);
        let mut parent = ProcessNode::agent(session.clone(), "claude", "/tmp", NOW);
        parent.lifecycle = Lifecycle::Alive;
        let parent_id = parent.id.clone();
        let mut independent = ProcessNode::agent(session.clone(), "worker", "/tmp", NOW);
        independent.kind = NodeKind::Subagent;
        independent.lifecycle = Lifecycle::Alive;
        independent.pid = Some(std::process::id());
        independent.link_to(parent_id.clone(), Relation::Confirmed);
        let independent_id = independent.id.clone();
        let mut nested = ProcessNode::agent(session.clone(), "reviewer", "/tmp", NOW);
        nested.kind = NodeKind::Subagent;
        nested.lifecycle = Lifecycle::Alive;
        nested.link_to(independent_id.clone(), Relation::Confirmed);
        let nested_id = nested.id.clone();
        let tree = &mut harness.core.sessions.get_mut(&session).unwrap().tree;
        tree.insert(parent);
        tree.insert(independent);
        tree.insert(nested);

        let retired = harness
            .core
            .mark_runtime_dependents(&session, &parent_id, NOW + 1);
        assert!(retired.is_empty());
        assert!(harness.core.sessions[&session]
            .tree
            .get(&independent_id)
            .unwrap()
            .is_running());
        assert!(harness.core.sessions[&session]
            .tree
            .get(&nested_id)
            .unwrap()
            .is_running());
    }

    #[tokio::test]
    async fn a_later_exit_waits_unapplied_behind_an_older_failed_checkpoint() {
        let mut harness = Harness::new().await;
        let session = SessionId::from_stored("sess_exit_checkpoint_barrier");
        harness.add_session(session.clone(), PaneId::new(), NOW);

        let mut earlier = ProcessNode::agent(session.clone(), "claude", "/tmp", NOW);
        earlier.lifecycle = Lifecycle::Alive;
        earlier.turn = Some(turn_core::state::Turn::Idle);
        let earlier_id = earlier.id.clone();
        let mut later = ProcessNode::agent(session.clone(), "reviewer", "/tmp", NOW);
        later.lifecycle = Lifecycle::Alive;
        let later_id = later.id.clone();
        {
            let tree = &mut harness.core.sessions.get_mut(&session).unwrap().tree;
            tree.insert(earlier);
            tree.insert(later);
        }
        harness.core.persist_session(&session).unwrap();

        let pending_event = TurnEvent::new(
            session.clone(),
            EventKind::AgentIdle,
            EventSource::Supervisor,
            Confidence::Explicit,
            NOW + 1,
        )
        .with_node(earlier_id);
        harness
            .core
            .failed_ingest_checkpoints
            .push_back(FailedIngestCheckpoint {
                event: pending_event,
                effects: Vec::new(),
            });

        harness.core.record_exit(
            &session,
            &later_id,
            ExitInfo {
                code: 0,
                signal: None,
            },
            NOW + 2,
        );

        assert!(harness.core.sessions[&session]
            .tree
            .get(&later_id)
            .unwrap()
            .is_running());
        assert_eq!(harness.core.deferred_runtime_inputs.len(), 1);
        assert!(matches!(
            harness.core.deferred_runtime_inputs.front(),
            Some(DeferredRuntimeInput::Exit { node_id, .. }) if node_id == &later_id
        ));

        harness.core.retry_failed_ingest_checkpoints(NOW + 3);

        assert!(harness.core.failed_ingest_checkpoints.is_empty());
        assert!(harness.core.deferred_runtime_inputs.is_empty());
        assert_eq!(
            harness.core.sessions[&session]
                .tree
                .get(&later_id)
                .unwrap()
                .lifecycle,
            Lifecycle::Exited { code: 0 }
        );
        let durable = harness
            .core
            .store
            .sessions()
            .get(&session)
            .unwrap()
            .unwrap();
        assert_eq!(
            durable.tree.get(&later_id).unwrap().lifecycle,
            Lifecycle::Exited { code: 0 }
        );
    }

    /// Waits for the exit watcher to report, then applies it the way the loop does.
    async fn run_until_it_exits(harness: &mut Harness, node: &NodeId, now_ms: i64) {
        let info = loop {
            match harness.commands.recv().await {
                Some(Command::Exited { node: exited, info }) if &exited == node => break info,
                Some(_) => continue,
                None => panic!("the exit watcher went away"),
            }
        };
        harness.core.node_exited(node, info, now_ms);
    }

    /// Takes the exit report off the channel and throws it away.
    ///
    /// This is what a lost watcher looks like from the state owner's side: the process
    /// really has ended, and nothing ever told the core about it. Taking the report is how
    /// the test knows the process is finished without waiting on a clock.
    async fn discard_the_exit_report(harness: &mut Harness, node: &NodeId) -> ExitInfo {
        loop {
            match harness.commands.recv().await {
                Some(Command::Exited { node: exited, info }) if &exited == node => return info,
                Some(_) => continue,
                None => panic!("the exit watcher went away"),
            }
        }
    }

    /// Counts the endings the log holds for a session.
    fn endings(harness: &Harness, session: &SessionId) -> usize {
        harness
            .core
            .store
            .events()
            .list_for_session(session, 50)
            .expect("the log must be readable")
            .iter()
            .filter(|event| {
                matches!(
                    event.kind,
                    EventKind::ProcessExited { .. } | EventKind::ProcessFailed { .. }
                )
            })
            .count()
    }

    /// The reaper lets go of a pty, and the node it belonged to has to be able to answer
    /// for itself afterwards. If the exit was never reported, the node still carries the
    /// lifecycle it was launched with — so reaping alone would leave the daemon presenting
    /// a running process with no process and no terminal behind it. Honesty about what we
    /// know is the rule the restore path is built around, and it does not stop applying
    /// because the news arrived by an unusual route.
    #[tokio::test]
    async fn a_process_reaped_without_its_exit_being_reported_still_says_what_happened() {
        let mut harness = Harness::new().await;
        let session = SessionId::from_stored("sess_unreported");
        harness.add_session(session.clone(), PaneId::from_stored("pane_unreported"), NOW);
        harness.allow_test_processes(&session);

        let node = harness
            .core
            .spawn_init_command(&session, "exit 7", NOW)
            .expect("the start-up command must run");
        let report = discard_the_exit_report(&mut harness, &node).await;
        assert_eq!(
            report.code, 7,
            "the process really did end with that status"
        );
        assert!(
            harness
                .core
                .sessions
                .get(&session)
                .expect("the session")
                .tree
                .get(&node)
                .expect("the node")
                .is_running(),
            "nothing has told the core about this exit yet"
        );

        harness.core.reap_finished_processes(NOW + 1_000);

        {
            let recorded = harness
                .core
                .sessions
                .get(&session)
                .expect("the session")
                .tree
                .get(&node)
                .expect("the node")
                .clone();
            assert_eq!(
                recorded.lifecycle,
                Lifecycle::Exited { code: 7 },
                "a node whose pty is being let go must not claim to be running"
            );
            assert_eq!(recorded.exit_code, Some(7));
            assert_eq!(recorded.ended_ms, Some(NOW + 1_000));
        }
        assert_eq!(endings(&harness, &session), 1);

        // A report that finds its way in afterwards changes nothing: one process ends once,
        // and a second ending in the log would be an event the user never lived through.
        harness.core.node_exited(&node, report, NOW + 2_000);
        assert_eq!(
            endings(&harness, &session),
            1,
            "one process must not end twice"
        );
        assert_eq!(
            harness
                .core
                .sessions
                .get(&session)
                .expect("the session")
                .tree
                .get(&node)
                .expect("the node")
                .ended_ms,
            Some(NOW + 1_000),
            "the moment it ended must not be rewritten by a late report"
        );

        // And the terminal is still reclaimed on the ordinary schedule.
        harness
            .core
            .reap_finished_processes(NOW + 1_000 + FINISHED_PTY_RETENTION_MS);
        assert!(
            !harness.core.processes.contains_key(&node),
            "a finished process nobody is watching must not be held for the daemon's lifetime"
        );
    }

    /// The daemon is meant to hold thirty sessions for days. A pty handle kept for every
    /// process that ever ran is a memory leak with a schedule, and a start-up command is
    /// the worst case: it has no pane, so no close and no relaunch would ever reach it.
    #[tokio::test]
    async fn a_finished_process_nobody_is_watching_gives_its_terminal_back() {
        let mut harness = Harness::new().await;
        let session = SessionId::from_stored("sess_reaped");
        let pane = PaneId::from_stored("pane_reaped");
        harness.add_session(session.clone(), pane.clone(), NOW);
        harness.allow_test_processes(&session);

        let node = harness
            .core
            .spawn_init_command(&session, "exit 0", NOW)
            .expect("the start-up command must run");
        run_until_it_exits(&mut harness, &node, NOW).await;

        // Kept at first, on purpose: the buffer still holds why it failed.
        assert!(harness.core.processes.contains_key(&node));
        harness.core.reap_finished_processes(NOW + 1_000);
        assert!(
            harness.core.processes.contains_key(&node),
            "a terminal must not be taken away while the user is still reading it"
        );

        // Somebody watching keeps it, however long ago it ended.
        let (client, _frames) = harness.add_client(8);
        harness
            .core
            .clients
            .get_mut(&client)
            .expect("the client")
            .attachments
            .insert(
                (session.clone(), pane.clone()),
                Attachment {
                    attachment_id: 7,
                    node_id: Some(node.clone()),
                    stream: turn_proto::PaneStream::Cells,
                    next_seq: 0,
                    owed_gap: 0,
                    owes_full_screen: false,
                },
            );
        harness
            .core
            .reap_finished_processes(NOW + FINISHED_PTY_RETENTION_MS * 10);
        assert!(
            harness.core.processes.contains_key(&node),
            "somebody has that terminal on screen"
        );

        harness.core.client_closed(client);
        harness
            .core
            .reap_finished_processes(NOW + FINISHED_PTY_RETENTION_MS);
        assert!(
            !harness.core.processes.contains_key(&node),
            "a finished process nobody is watching must not be held for the daemon's lifetime"
        );

        // What happened to it is still on the record: only the scrollback went.
        let node_view = harness
            .core
            .sessions
            .get(&session)
            .expect("the session")
            .tree
            .get(&node)
            .expect("the node stays in the tree");
        assert!(node_view.lifecycle.is_terminal());
        assert_eq!(node_view.exit_code, Some(0));
    }

    /// A signal death is the one exit where the code is meaningless, so the name is the
    /// only account of it there is. Losing it means the log cannot answer "was it killed,
    /// or did it fail?" — and applying the event must not wipe what the platform said.
    #[tokio::test]
    async fn the_log_says_how_a_signalled_process_died_and_the_node_keeps_its_status() {
        let mut harness = Harness::new().await;
        let session = SessionId::from_stored("sess_signalled");
        harness.add_session(session.clone(), PaneId::new(), NOW);
        harness.allow_test_processes(&session);

        let node = harness
            .core
            .spawn_init_command(&session, "sleep 30", NOW)
            .expect("the start-up command must run");
        // A death nobody asked for: an out-of-memory kill, not a stop request.
        harness.core.record_exit(
            &session,
            &node,
            turn_pty::ExitInfo {
                code: 1,
                signal: Some("Killed".to_string()),
            },
            NOW + 5_000,
        );

        let recorded = harness
            .core
            .sessions
            .get(&session)
            .expect("the session")
            .tree
            .get(&node)
            .expect("the node");
        assert_eq!(
            recorded.lifecycle,
            Lifecycle::Signaled {
                signal: "Killed".to_string()
            }
        );
        assert_eq!(
            recorded.exit_code,
            Some(1),
            "applying the event must not erase what the platform reported"
        );

        let logged = harness
            .core
            .store
            .events()
            .list_for_session(&session, 20)
            .expect("the log must be readable");
        let failure = logged
            .iter()
            .find(|event| matches!(event.kind, EventKind::ProcessFailed { .. }))
            .expect("a signal death nobody asked for is a failure");
        assert!(
            failure
                .raw
                .as_deref()
                .is_some_and(|raw| raw.contains("Killed")),
            "the log cannot say how it died: {:?}",
            failure.raw
        );

        let durable = harness
            .core
            .store
            .sessions()
            .get(&session)
            .expect("the Session projection")
            .expect("the durable Session");
        assert_eq!(
            durable.tree.get(&node).unwrap().lifecycle,
            Lifecycle::Signaled {
                signal: "Killed".to_string()
            },
            "the failure state and event commit in the same checkpoint"
        );
        let queue = harness
            .core
            .store
            .attention()
            .load_queue()
            .expect("the durable attention queue");
        let demand = queue
            .iter()
            .find(|entry| entry.node_id.as_ref() == Some(&node))
            .expect("an unexpected process death remains actionable after restart");
        assert_eq!(demand.demand_kind, AttentionDemandKind::ProcessFailed);
        assert!(demand.survives_owner_exit);
    }

    #[test]
    fn a_process_the_user_asked_to_stop_is_not_recorded_as_a_failure() {
        // The failure this prevents: the user kills a build on purpose and the sidebar
        // shows it in red, alongside the crashes that actually need them.
        let killed = ExitInfo {
            code: 137,
            signal: Some("Killed".to_string()),
        };
        let lifecycle = exit_lifecycle(&killed, true);
        assert!(!lifecycle.is_failure(), "{lifecycle:?}");
        assert!(lifecycle.is_terminal());
        assert_eq!(
            turn_core::state::DisplayState::derive(&lifecycle, None),
            turn_core::state::DisplayState::Stopped
        );

        // The same death nobody asked for is still a failure, and still says how it died.
        let unexpected = exit_lifecycle(&killed, false);
        assert_eq!(
            unexpected,
            Lifecycle::Signaled {
                signal: "Killed".to_string()
            }
        );
        assert!(unexpected.is_failure());
    }

    #[test]
    fn a_process_that_exits_on_its_own_keeps_its_status_whoever_asked() {
        // Stopping something on purpose does not rewrite what it said on the way out.
        let failed = ExitInfo {
            code: 3,
            signal: None,
        };
        let kept = Lifecycle::Exited { code: 3 };
        assert_eq!(exit_lifecycle(&failed, true), kept);
        assert_eq!(exit_lifecycle(&failed, false), kept);

        let clean = ExitInfo {
            code: 0,
            signal: None,
        };
        assert_eq!(exit_lifecycle(&clean, true), Lifecycle::Exited { code: 0 });
    }

    #[test]
    fn the_log_records_the_platforms_own_name_for_the_signal() {
        let terminated = ExitInfo {
            code: 143,
            signal: Some("Terminated".to_string()),
        };
        let note = signal_note(&terminated).expect("a signal death has a note");
        assert!(note.contains("Terminated"), "{note}");
        assert!(note.contains("143"), "{note}");
        // An ordinary exit has no signal to describe, and inventing a note for one
        // would put "signal=" in the log for every process that ever ended.
        assert!(signal_note(&ExitInfo {
            code: 1,
            signal: None
        })
        .is_none());
    }
}
