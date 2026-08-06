//! Checkout write authority across the two ends of a daemon's life.
//!
//! A Main Checkout Session owns an exclusive write lease on its checkout, and there are
//! exactly two honest ways for that ownership to end:
//!
//! * **This daemon gives it up.** [`Core::release_write_authority`] runs on the ordered
//!   shutdown, so the row on disk says `released` and the next start has nothing to ask
//!   about. Without this half, *every* clean quit looked identical to a crash and the
//!   user was asked to confirm write access they had never given up — which is the bug
//!   this module exists to fix.
//! * **The daemon dies.** Nothing releases anything, and the next start finds an
//!   unreleased lease. [`super::restore`] fences it as `recovery_required` before loading
//!   a single Session, and then [`Core::restore_write_authority`] asks the one question
//!   that actually matters: *is anything from that dead generation still writing?*
//!
//! The answer is evidence, not a guess. Two facts are available and both are used:
//!
//! * The daemon holds a non-blocking exclusive `flock` on the canonical data directory
//!   ([`crate::instance::DataDirLock`]), acquired before SQLite is even opened. The
//!   kernel releases it when the owning process dies, so holding it proves that no other
//!   Turn daemon is using this data directory. That is a structural precondition here:
//!   `Core` cannot exist without the lock.
//! * The processes the previous generation recorded are in the OS process table or they
//!   are not. Turn persists their pids and commands, so each one can be looked up.
//!
//! When the flock is held and every recorded process is provably gone, there is no second
//! writer and authority is taken back silently. When something is still running — or when
//! Turn cannot prove what a live pid belongs to — the lease stays `recovery_required` and
//! the user is asked, because a false "safe" here means two processes writing one Git
//! checkout.

use super::Core;
use turn_core::ids::{CheckoutId, NodeId, SessionId, WorkspaceId};
use turn_core::model::{LeaseState, ProcessNode, SessionMode};
use turn_pty::ObservedProcess;

/// How far a node's recorded launch and the OS's own start time may disagree before the
/// difference becomes evidence rather than noise.
///
/// Two sources of slack, both small and both real: the platform reports whole seconds, and
/// Turn writes the node down around the fork rather than exactly at it. Past this window a
/// process that began *later* than the node cannot be that node's process, because only a
/// recycled pid can start after the moment it was written down.
const START_TIME_SKEW_MS: i64 = 2_000;

/// What one process the previous daemon recorded says about a second writer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WriterSurvival {
    /// Provably not the process Turn recorded: either its pid is absent from the table,
    /// or the process now at that pid cannot be the one Turn launched.
    Gone,
    /// Still running — or alive and impossible to corroborate, which is treated the same
    /// way. A checkout is not something to guess about.
    Running,
}

/// Decides what the process table says about one process the previous daemon recorded.
///
/// [`WriterSurvival::Running`] is the answer for any pid still in the table; deciding
/// otherwise needs positive evidence. The costs are not symmetric: a wrong `Running`
/// asks the user a question they did not need, while a wrong `Gone` hands a second
/// writer the checkout — so the doubt is spent on the side that only annoys.
///
/// `observed` is the same refresh `alive` came from. `None` for a live pid means the
/// platform would not describe it, which corroborates nothing and therefore rules
/// nothing out.
pub(crate) fn writer_survival(
    node: &ProcessNode,
    alive: bool,
    observed: Option<&ObservedProcess>,
) -> WriterSurvival {
    if !alive {
        return WriterSurvival::Gone;
    }
    let Some(observed) = observed else {
        return WriterSurvival::Running;
    };
    let Some(started_ms) = observed.start_time_ms else {
        return WriterSurvival::Running;
    };
    // A process that began after Turn wrote this node down is a stranger wearing a
    // recycled pid. This is the one direction pid reuse can be proved in.
    if started_ms > node.started_ms + START_TIME_SKEW_MS {
        return WriterSurvival::Gone;
    }
    // Older than the launch *and* running something else. Age alone proves nothing: a
    // whole-second start time can round to before the millisecond Turn recorded, and a
    // shell keeps running the agent typed into it under its own command line.
    if started_ms + START_TIME_SKEW_MS < node.started_ms
        && !runs_the_recorded_command(&node.command, observed)
    {
        return WriterSurvival::Gone;
    }
    WriterSurvival::Running
}

/// Whether the observed process still looks like the command Turn recorded.
///
/// Deliberately generous — the executable name appearing in the name or the command line
/// is enough — because a stricter rule would start declaring live processes gone, and
/// that is the failure that puts two writers in one checkout. Restore's own verdicts read
/// the same fact more harshly on purpose: a process it cannot recognise is reported as
/// `Lost` because Turn genuinely cannot show it, while authority keeps treating it as a
/// possible writer.
pub(crate) fn runs_the_recorded_command(command: &str, observed: &ObservedProcess) -> bool {
    let Some(expected) = command
        .split_whitespace()
        .next()
        .and_then(|first| first.rsplit('/').next())
        .filter(|name| !name.is_empty())
    else {
        return false;
    };
    observed.name.contains(expected) || observed.command_line.contains(expected)
}

/// One process from a dead daemon generation that is still running.
#[derive(Debug, Clone)]
pub(crate) struct SurvivingWriter {
    pub session_id: SessionId,
    pub node_id: NodeId,
    pub pid: u32,
}

/// Whether a checkout still resolves to the filesystem identity that was fenced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CheckoutIdentity {
    /// The registered path canonicalises to exactly the stored identity.
    Intact,
    /// The path could not be resolved at all, with the reason.
    Unverifiable(String),
    /// It resolves somewhere else now, so the fence protects a different directory.
    Moved,
}

/// What one start-up did about the checkout authority it inherited, for the log.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RestoredAuthority {
    /// Fenced leases adopted again because every recorded process was gone.
    pub recovered: usize,
    /// Leases taken fresh after a clean release.
    pub reacquired: usize,
    /// Checkouts left needing the user's confirmation.
    pub withheld: usize,
}

impl Core {
    /// Gives up this daemon's checkout authority. The last durable act of a clean stop.
    ///
    /// Every route out of the run loop reaches this: the explicit shutdown request the
    /// GUI's companion daemon sends, `SIGINT`/`SIGTERM`, and the command channel closing
    /// because the last sender went away.
    pub(crate) fn release_write_authority(&mut self, now_ms: i64) {
        match self.store.hierarchy().release_active_write_leases(now_ms) {
            Ok(0) => {}
            Ok(released) => tracing::info!(released, "released checkout write authority"),
            // Worth being loud about: the leases stay `active` on disk, so the next
            // start will treat this clean stop as a crash and ask the user to confirm.
            Err(error) => tracing::error!(
                %error,
                "could not release checkout write authority; the next start will ask to confirm it"
            ),
        }
    }

    /// Decides, once per start, which Sessions get their checkout back without asking.
    ///
    /// Runs after the process table has been refreshed and before any Session's node
    /// lifecycles are rewritten by [`super::restore`]'s verdicts: those verdicts collapse
    /// "alive but unrecognisable" into `Lost`, which is the right thing to *show* a user
    /// and the wrong thing to hand a checkout on.
    pub(crate) fn restore_write_authority(&mut self, now_ms: i64) -> RestoredAuthority {
        let mut summary = RestoredAuthority::default();
        // Sorted, so a machine with several Workspaces produces the same log every time.
        let mut writers: Vec<(WorkspaceId, SessionId, CheckoutId)> = self
            .sessions
            .values()
            .filter(|session| session.mode == SessionMode::MainCheckout && !session.is_archived())
            .map(|session| {
                (
                    session.workspace_id.clone(),
                    session.id.clone(),
                    session.checkout_id.clone(),
                )
            })
            .collect();
        writers.sort_by(|left, right| left.1.as_str().cmp(right.1.as_str()));

        for (workspace_id, session_id, checkout_id) in writers {
            let Some(workspace) = self.workspaces.get(&workspace_id) else {
                continue;
            };
            if workspace.archived {
                continue;
            }
            if workspace.lease_reconciliation_required {
                // A migrated Workspace has never proved which Session was its writer.
                // That gate is explicit by design and is not this daemon's to clear.
                tracing::debug!(
                    workspace = %workspace_id,
                    "left checkout authority alone: the Workspace awaits explicit reconciliation"
                );
                continue;
            }

            let current = match self.store.hierarchy().active_lease(&workspace_id) {
                Ok(current) => current,
                Err(error) => {
                    tracing::warn!(%error, workspace = %workspace_id, "could not read the write lease");
                    continue;
                }
            };
            if let Some(lease) = &current {
                if lease.session_id != session_id || lease.checkout_id != checkout_id {
                    // Another Session holds or is owed this Workspace's checkout. That is
                    // a conflict for the user to resolve, not a restart to reinterpret.
                    continue;
                }
                if lease.state != LeaseState::RecoveryRequired {
                    continue;
                }
            }

            let survivors = self.surviving_checkout_writers(&checkout_id);
            if let Some(survivor) = survivors.first() {
                tracing::info!(
                    session = %session_id,
                    checkout = %checkout_id,
                    surviving = survivors.len(),
                    pid = survivor.pid,
                    node = %survivor.node_id,
                    owner = %survivor.session_id,
                    "withholding checkout write access: a process from the previous daemon is still running"
                );
                if current.is_none() {
                    self.withhold_checkout_authority(
                        &workspace_id,
                        &session_id,
                        &checkout_id,
                        now_ms,
                    );
                }
                summary.withheld += 1;
                continue;
            }

            match self.primary_checkout_identity(&workspace_id, &checkout_id) {
                CheckoutIdentity::Intact => {}
                identity => {
                    tracing::warn!(
                        session = %session_id,
                        checkout = %checkout_id,
                        ?identity,
                        "withholding checkout write access: the checkout identity is not the one that was fenced"
                    );
                    if current.is_none() {
                        self.withhold_checkout_authority(
                            &workspace_id,
                            &session_id,
                            &checkout_id,
                            now_ms,
                        );
                    }
                    summary.withheld += 1;
                    continue;
                }
            }

            match &current {
                // A crash: the fenced row keeps its identity and is adopted exactly.
                Some(lease) => {
                    match self.store.hierarchy().reclaim_write_lease(
                        &workspace_id,
                        &session_id,
                        &checkout_id,
                        &lease.id,
                        lease.generation,
                        now_ms,
                    ) {
                        Ok(Some(adopted)) => {
                            tracing::info!(
                                session = %session_id,
                                lease = %adopted.id,
                                generation = adopted.generation,
                                "recovered checkout write access: nothing from the previous daemon survived"
                            );
                            summary.recovered += 1;
                        }
                        Ok(None) => tracing::warn!(
                            session = %session_id,
                            lease = %lease.id,
                            "the fenced lease changed while restoring; it still needs confirmation"
                        ),
                        Err(error) => tracing::warn!(
                            %error, session = %session_id,
                            "could not recover checkout write access; it still needs confirmation"
                        ),
                    }
                }
                // A clean stop: nothing was fenced, so this is an ordinary acquisition
                // for the Session that was already the writer.
                None => match self.store.hierarchy().acquire_write_lease(
                    &workspace_id,
                    &session_id,
                    &checkout_id,
                    now_ms,
                ) {
                    Ok(lease) => {
                        tracing::info!(
                            session = %session_id,
                            lease = %lease.id,
                            generation = lease.generation,
                            "took checkout write access back after a clean stop"
                        );
                        summary.reacquired += 1;
                    }
                    Err(error) => tracing::warn!(
                        %error, session = %session_id,
                        "could not take checkout write access back; the Session must ask for it"
                    ),
                },
            }
        }
        summary
    }

    /// Every process a previous daemon recorded against this checkout that could still
    /// be writing to it.
    ///
    /// Only Main Checkout Sessions assigned to this exact checkout are considered: they
    /// are the only ones that ever had authority over it. A read-only Session launches
    /// nothing without a technical guard, and an isolated worktree writes somewhere else.
    pub(crate) fn surviving_checkout_writers(&self, checkout: &CheckoutId) -> Vec<SurvivingWriter> {
        let mut survivors = Vec::new();
        for session in self.sessions.values().filter(|session| {
            &session.checkout_id == checkout && session.mode == SessionMode::MainCheckout
        }) {
            for node in session.tree.iter() {
                let Some(pid) = node.pid else {
                    continue;
                };
                if !node.lifecycle.is_running() {
                    continue;
                }
                let survival = writer_survival(
                    node,
                    self.supervisor.is_alive(pid),
                    self.supervisor.observe(pid).as_ref(),
                );
                if survival == WriterSurvival::Running {
                    survivors.push(SurvivingWriter {
                        session_id: session.id.clone(),
                        node_id: node.id.clone(),
                        pid,
                    });
                }
            }
        }
        survivors.sort_by(|left, right| left.node_id.as_str().cmp(right.node_id.as_str()));
        survivors
    }

    /// Whether a Workspace's checkout still resolves to the identity that was fenced.
    ///
    /// The unattended path has to be at least as careful as the one with a human in it,
    /// so this is the same check the explicit confirmation performs.
    pub(crate) fn primary_checkout_identity(
        &self,
        workspace_id: &WorkspaceId,
        checkout_id: &CheckoutId,
    ) -> CheckoutIdentity {
        let checkout = match self.store.hierarchy().checkout(workspace_id, checkout_id) {
            Ok(Some(checkout)) => checkout,
            Ok(None) => {
                return CheckoutIdentity::Unverifiable(format!(
                    "workspace {workspace_id} has no registered checkout {checkout_id}"
                ))
            }
            Err(error) => return CheckoutIdentity::Unverifiable(error.to_string()),
        };
        match std::fs::canonicalize(&checkout.path) {
            Ok(resolved) if resolved.to_string_lossy() == checkout.canonical_path => {
                CheckoutIdentity::Intact
            }
            Ok(_) => CheckoutIdentity::Moved,
            Err(error) => CheckoutIdentity::Unverifiable(error.to_string()),
        }
    }

    /// Records that authority is withheld when a clean release left nothing to fence.
    fn withhold_checkout_authority(
        &self,
        workspace_id: &WorkspaceId,
        session_id: &SessionId,
        checkout_id: &CheckoutId,
        now_ms: i64,
    ) {
        if let Err(error) = self.store.hierarchy().withhold_write_lease(
            workspace_id,
            session_id,
            checkout_id,
            now_ms,
        ) {
            // The Session then has no lease at all, which still refuses every
            // write-capable launch. It is the recovery *offer* that is lost, so this is
            // an error rather than a note.
            tracing::error!(
                %error, session = %session_id,
                "could not record withheld checkout write access; the Session cannot be offered it"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use turn_core::model::NodeKind;
    use turn_core::state::Lifecycle;

    const NOW: i64 = 1_775_000_000_000;

    fn recorded(command: &str, started_ms: i64) -> ProcessNode {
        let mut node = ProcessNode::process(
            SessionId::from_stored("sess_authority"),
            NodeKind::Shell,
            command,
            "/repo",
            started_ms,
        );
        node.pid = Some(4_242);
        node.lifecycle = Lifecycle::Orphaned;
        node
    }

    fn observed(command: &str, start_time_ms: Option<i64>) -> ObservedProcess {
        ObservedProcess {
            pid: 4_242,
            ppid: Some(1),
            name: command.split_whitespace().next().unwrap_or("").to_string(),
            command_line: command.to_string(),
            args: command.split_whitespace().map(str::to_string).collect(),
            cwd: Some("/repo".to_string()),
            start_time_ms,
            kind: NodeKind::Shell,
        }
    }

    #[test]
    fn a_pid_that_left_the_process_table_is_the_only_cheap_proof_a_writer_is_gone() {
        let node = recorded("claude", NOW);
        assert_eq!(
            writer_survival(&node, false, None),
            WriterSurvival::Gone,
            "the kernel not knowing the pid is proof enough"
        );
    }

    #[test]
    fn a_live_process_from_the_same_moment_as_its_node_is_treated_as_still_running() {
        let node = recorded("claude", NOW);
        assert_eq!(
            writer_survival(&node, true, Some(&observed("claude --resume", Some(NOW)))),
            WriterSurvival::Running
        );
    }

    /// The pid-reuse direction that can be proved: nothing Turn wrote down at `NOW` can
    /// have started a minute later.
    #[test]
    fn a_process_that_started_after_its_node_was_recorded_is_a_recycled_pid() {
        let node = recorded("claude", NOW);
        assert_eq!(
            writer_survival(&node, true, Some(&observed("claude", Some(NOW + 60_000)))),
            WriterSurvival::Gone
        );
    }

    /// The false-safe this whole function is shaped around. A recorded start time can
    /// legitimately be later than the process's own — a whole-second start time rounds
    /// down, and a node may be written after the fork — so "older than its node" must
    /// never be enough on its own to declare a live agent gone.
    #[test]
    fn a_live_process_whose_start_time_predates_its_node_is_not_declared_gone() {
        let node = recorded("claude", NOW);
        assert_eq!(
            writer_survival(&node, true, Some(&observed("claude", Some(NOW - 60_000)))),
            WriterSurvival::Running
        );
        assert_eq!(
            writer_survival(&node, true, Some(&observed("claude", Some(NOW - 1_000)))),
            WriterSurvival::Running,
            "a second of disagreement is rounding, not evidence"
        );
    }

    #[test]
    fn an_older_process_running_something_else_is_a_stranger_at_that_pid() {
        let node = recorded("claude", NOW);
        assert_eq!(
            writer_survival(
                &node,
                true,
                Some(&observed("/usr/sbin/cupsd -l", Some(NOW - 600_000)))
            ),
            WriterSurvival::Gone
        );
    }

    /// "Err toward asking" as an executable rule: with nothing to corroborate, a live
    /// pid keeps its checkout.
    #[test]
    fn a_live_pid_the_platform_will_not_describe_keeps_its_checkout() {
        let node = recorded("claude", NOW);
        assert_eq!(
            writer_survival(&node, true, None),
            WriterSurvival::Running,
            "an unreadable process is not a dead one"
        );
        assert_eq!(
            writer_survival(&node, true, Some(&observed("claude", None))),
            WriterSurvival::Running,
            "no start time means no corroboration, so nothing is ruled out"
        );
    }
}
