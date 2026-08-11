//! Coming back after a restart, honestly.
//!
//! A fresh daemon can load the desk — workspaces, sessions, layouts, policies, the
//! attention queue — but it cannot inherit a pty. The master end lived in the previous
//! process's file table and went with it. So for every process the store says was
//! running, there are exactly two truthful answers:
//!
//! * **Orphaned** — it is in the process table and still running, but out of reach. The
//!   work is alive; Turn cannot show you its terminal.
//! * **Lost** — it was running when we last wrote and it is not there now.
//!
//! [`turn_core::state::Lifecycle::Reconnected`] is deliberately never produced here. It
//! describes a UI being rebuilt over a daemon that kept the ptys all along, which needs
//! no restore at all — the session was never gone. Reporting it after a daemon restart
//! would be the most convenient lie available.
//!
//! **Nothing is relaunched unattended.** The daemon reports what can be started again; a
//! connected window immediately restores panes marked `Relaunch` and every commandless terminal
//! as the user's shell. Booting a daemon with no window still runs no user process.
//!
//! Checkout write authority is decided in the same pass, by [`super::authority`]: every
//! unreleased lease is fenced before a single Session is loaded, and only evidence — the
//! data-directory lock plus the process table — can give it back without asking.

use super::Core;
use crate::error::Result;
use crate::paths;
use std::collections::{HashMap, HashSet};
use turn_core::attention::AttentionManager;
use turn_core::ids::{NodeId, PaneId, SessionId};
use turn_core::model::{PaneKind, RestoreBehaviour, RestoreState, Session, SessionMode};
use turn_core::state::Lifecycle;
use turn_proto::{PaneRestoreOutcome, ServerEvent};

impl Core {
    /// Loads everything from the store and decides what became of each process.
    pub(crate) fn restore(&mut self, now_ms: i64) -> Result<()> {
        // A temporary Pane belongs to a concrete UI surface. No such surface
        // survives this daemon generation, so carrying its binding forward would
        // create a phantom "TEMP PANE" marker that cannot be focused.
        let temporary_bindings_pruned = self.store.hierarchy().clear_all_temporary_bindings()?;
        // This must be the first state transition of a new daemon generation.
        // Loading a Session is not proof that its previous daemon relinquished
        // checkout authority, and no heartbeat or launch may auto-adopt it.
        let leases_requiring_recovery = self
            .store
            .hierarchy()
            .require_recovery_after_daemon_restart()?;

        let installed = self.store.templates().install_built_ins(now_ms)?;
        for template in self.store.templates().list()? {
            self.templates.insert(template.id.clone(), template);
        }
        for workspace in self.store.workspaces().list()? {
            self.workspaces.insert(workspace.id.clone(), workspace);
        }

        let stored: Vec<SessionId> = self
            .store
            .sessions()
            .list_all()?
            .into_iter()
            .map(|session| session.id)
            .collect();
        for id in &stored {
            // `load_for_restore` downgrades anything stored as running to `Orphaned`,
            // because a stored "alive" only ever meant "alive when we last wrote".
            if let Some(mut session) = self.store.sessions().load_for_restore(id)? {
                let navigation_migrated = migrate_obsolete_navigation_panes(&mut session);
                let guard_downgraded =
                    if session.mode == SessionMode::ReadOnly && session.read_only_enforced {
                        match self.read_only_sandbox(&session) {
                            Ok(Some(_)) => false,
                            Ok(None) => {
                                session.read_only_enforced = false;
                                true
                            }
                            Err(error) => {
                                tracing::warn!(
                                    session_id = %session.id,
                                    %error,
                                    "restored read-only Session lost its process guard"
                                );
                                session.read_only_enforced = false;
                                true
                            }
                        }
                    } else {
                        false
                    };
                if navigation_migrated || guard_downgraded {
                    // The old central AgentTree cannot coexist with the unified
                    // sidebar. Persist structural/guard truth immediately, but never
                    // materialise the replacement Shell during restore. False is not
                    // upgraded here: legacy/orphaned processes may never have been
                    // sandboxed even when this platform can guard a future launch.
                    self.store.sessions().save(&session)?;
                }
                self.sessions.insert(session.id.clone(), session);
            }
        }

        let (terminal_histories_restored, terminal_histories_pruned) =
            self.restore_terminal_histories();

        // One refresh for every decision below. Deciding a process is gone requires
        // looking at the process table, which the store rightly refuses to guess at.
        if !self.sessions.is_empty() {
            self.supervisor.refresh();
            self.last_sweep_ms = now_ms;
        }
        // Before the per-node verdicts, which collapse "alive but unrecognisable" into
        // `Lost`. That is the right thing to show a user and the wrong thing to hand a
        // checkout on, so write authority is decided from the raw process table.
        let authority = self.restore_write_authority(now_ms);
        let ids: Vec<SessionId> = self.sessions.keys().cloned().collect();
        for id in &ids {
            let report = self.decide_session(id, now_ms);
            self.persist_session_quietly(id);
            self.restore_reports.push(report);
        }

        // After the verdicts, so a demand raised by a process that turned out to be gone
        // is not put back in front of the user.
        self.restore_queue()?;
        self.restore_attention_mutes(now_ms)?;
        // Written straight back, so the demands this start-up decided against do not sit
        // on disk waiting to be reconsidered by the next one.
        let _ = self.persist_attention();

        let known: HashSet<String> = self.sessions.keys().map(|id| id.to_string()).collect();
        let pruned = paths::prune_scratch(&self.data_dir, &known);

        tracing::info!(
            workspaces = self.workspaces.len(),
            sessions = self.sessions.len(),
            templates = self.templates.len(),
            built_ins_installed = installed,
            attention = self.attention.queue().len(),
            leases_requiring_recovery,
            leases_recovered = authority.recovered,
            leases_reacquired = authority.reacquired,
            leases_withheld = authority.withheld,
            temporary_bindings_pruned,
            scratch_pruned = pruned,
            terminal_histories_restored,
            terminal_histories_pruned,
            "restored"
        );
        Ok(())
    }

    pub(crate) fn restore_attention_mutes(&mut self, now_ms: i64) -> Result<()> {
        for session_id in self.sessions.keys() {
            let key = crate::core::attention::mute_setting_key(session_id);
            if let Some(until_ms) = self.store.settings().get::<i64>(&key)? {
                if until_ms > now_ms {
                    self.attention.mute_session(session_id, until_ms);
                } else {
                    self.store.settings().remove(&key)?;
                }
            }
        }
        Ok(())
    }

    /// Loads display-only terminal models without changing any process lifecycle.
    fn restore_terminal_histories(&mut self) -> (usize, usize) {
        let sessions: Vec<(SessionId, bool, Vec<NodeId>)> = self
            .sessions
            .iter()
            .map(|(session_id, session)| {
                (
                    session_id.clone(),
                    self.terminal_history_enabled(session_id),
                    session.tree.iter().map(|node| node.id.clone()).collect(),
                )
            })
            .collect();
        let mut known: HashMap<String, HashSet<String>> = HashMap::new();
        let mut restored = 0usize;

        for (session_id, enabled, nodes) in sessions {
            if !enabled {
                paths::remove_session_terminal_history(&self.data_dir, &session_id);
                continue;
            }
            known.insert(
                session_id.to_string(),
                nodes.iter().map(ToString::to_string).collect(),
            );
            for node_id in nodes {
                let dir = paths::node_terminal_history(&self.data_dir, &session_id, &node_id);
                match turn_pty::TerminalJournal::recover(&dir, self.journal_config()) {
                    Ok(Some(recovered)) => {
                        self.recovered_terminals
                            .insert(node_id.clone(), recovered.buffer);
                        restored += 1;
                    }
                    Ok(None) => {}
                    Err(error) => {
                        tracing::warn!(
                            session = %session_id,
                            node = %node_id,
                            path = %dir.display(),
                            %error,
                            "could not recover terminal history"
                        );
                    }
                }
            }
        }
        let pruned = paths::prune_terminal_history(&self.data_dir, &known);
        (restored, pruned)
    }

    /// Retires one recovery offer after its pane process was explicitly relaunched.
    /// The returned event is published by the caller only after the updated Session is
    /// durably stored, so a reconnect never resurrects an offer that already succeeded.
    pub(crate) fn resolve_restore_node(
        &mut self,
        session_id: &SessionId,
        node_id: &NodeId,
    ) -> Option<ServerEvent> {
        self.resolve_restore_where(session_id, |outcome| &outcome.node_id == node_id)
    }

    /// Retires the offer for a pane the user explicitly removed from the Layout.
    pub(crate) fn resolve_restore_pane(
        &mut self,
        session_id: &SessionId,
        pane_id: &PaneId,
    ) -> Option<ServerEvent> {
        self.resolve_restore_where(session_id, |outcome| &outcome.pane_id == pane_id)
    }

    /// Ends every unresolved recovery offer when the user explicitly terminates a
    /// Session. Detaching with `KeepProcesses` deliberately does not call this.
    pub(crate) fn resolve_restore_session(
        &mut self,
        session_id: &SessionId,
    ) -> Option<ServerEvent> {
        self.resolve_restore_where(session_id, |_| true)
    }

    /// Rechecks runtimes that survived a daemon restart before write authority is
    /// recovered.
    ///
    /// An orphan is an observation, not a permanent state. The user may stop that
    /// process outside Turn exactly as the recovery UI asks them to. Without this
    /// refresh the in-memory tree would continue to claim it is alive until another
    /// daemon restart, permanently blocking both the lease and an explicit relaunch.
    pub(crate) fn reconcile_orphaned_recovery(
        &mut self,
        session_id: &SessionId,
        now_ms: i64,
    ) -> std::result::Result<(), turn_proto::ProtoError> {
        let orphaned: Vec<(NodeId, Option<NodeId>, Option<String>)> = self
            .sessions
            .get(session_id)
            .map(|session| {
                session
                    .tree
                    .iter()
                    .filter(|node| node.lifecycle == Lifecycle::Orphaned)
                    .map(|node| {
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
                    })
                    .collect()
            })
            .unwrap_or_default();
        if orphaned.is_empty() {
            return Ok(());
        }

        let previous = self.session(session_id)?.clone();
        self.supervisor.refresh();
        let report = self.decide_session(session_id, now_ms);
        let lost: Vec<_> = orphaned
            .into_iter()
            .filter(|(node_id, _, _)| {
                self.sessions
                    .get(session_id)
                    .and_then(|session| session.tree.get(node_id))
                    .is_some_and(|node| node.lifecycle == Lifecycle::Lost)
            })
            .collect();

        if lost.is_empty() {
            return Ok(());
        }

        if let Err(error) = self.persist_session(session_id) {
            // A retry must not reclaim authority from a transition that never became
            // durable. Restore the Orphaned truth and leave the lease fenced.
            self.sessions.insert(session_id.clone(), previous);
            return Err(error);
        }

        if let Some(existing) = self.restore_reports.iter_mut().find(|existing| {
            matches!(
                existing,
                ServerEvent::RestoreResult {
                    session_id: reported,
                    ..
                } if reported == session_id
            )
        }) {
            *existing = report.clone();
        } else {
            self.restore_reports.push(report.clone());
        }
        self.resolve_lifecycle_attention(session_id, &lost, now_ms);
        self.push_tree(session_id, now_ms);
        self.push_session_state(session_id, now_ms);
        self.push_all(report);
        Ok(())
    }

    fn resolve_restore_where(
        &mut self,
        session_id: &SessionId,
        resolved: impl Fn(&PaneRestoreOutcome) -> bool,
    ) -> Option<ServerEvent> {
        let report = self.restore_reports.iter_mut().find(|report| {
            matches!(
                report,
                ServerEvent::RestoreResult { session_id: reported, .. } if reported == session_id
            )
        })?;
        let ServerEvent::RestoreResult {
            state,
            needs_explanation,
            panes,
            ..
        } = report
        else {
            unreachable!("the search above selected a restore result")
        };
        panes.retain(|outcome| !resolved(outcome));
        *state = if panes.is_empty() {
            RestoreState::Live
        } else if panes.iter().any(|pane| pane.lifecycle.is_running()) {
            RestoreState::PartiallyRestored
        } else {
            RestoreState::LayoutOnly
        };
        *needs_explanation = state.needs_explanation();
        if let Some(session) = self.sessions.get_mut(session_id) {
            session.restore_state = *state;
        }
        Some(report.clone())
    }

    /// Rebuilds the attention manager around its exact durable queue.
    ///
    /// Entries are not replayed as events: replay would mint identities, reset
    /// age, wake snoozes, discard acknowledgements and potentially emit effects.
    /// Reconciliation may only remove an entry whose owning runtime invariant no
    /// longer holds.
    fn restore_queue(&mut self) -> Result<()> {
        let mut queue = self.store.attention().load_queue()?;
        queue.retain(|entry| {
            let Some(session) = self.sessions.get(&entry.session_id) else {
                return false;
            };
            // A demand belongs to a process. If that process turned out to be gone, the
            // demand went with it: the agent that was waiting for an answer is not there
            // to receive one, and putting it back would have the user open Turn to a
            // question nobody is asking any more.
            if let Some(node) = &entry.node_id {
                let Some(subject) = session.tree.get(node) else {
                    tracing::debug!(
                        session = %entry.session_id, %node,
                        "dropped a stored demand: its subject no longer exists"
                    );
                    return false;
                };
                if entry.survives_owner_exit {
                    return true;
                }
                if !subject.lifecycle.is_running() {
                    tracing::debug!(
                        session = %entry.session_id, %node,
                        "dropped a stored demand: its process did not survive"
                    );
                    return false;
                }
            } else if let Some(parent) = &entry.parent_node_id {
                let Some(owner) = session.tree.get(parent) else {
                    tracing::debug!(
                        session = %entry.session_id, %parent,
                        "dropped a stored unresolved demand: its hook parent is missing"
                    );
                    return false;
                };
                if entry.survives_owner_exit {
                    return true;
                }
                if !owner.lifecycle.is_running() {
                    tracing::debug!(
                        session = %entry.session_id, %parent,
                        "dropped a stored unresolved demand: its hook parent did not survive"
                    );
                    return false;
                }
            } else if entry.survives_owner_exit {
                // Session-level post-mortem evidence has no process identity to
                // validate; the existing Session remains its durable boundary.
                return true;
            }
            if entry.node_id.is_none() {
                if let Some(external_id) = entry.subject_external_id.as_deref() {
                    if let Some(subject) = session.tree.find_by_external_id(external_id) {
                        let belongs_to_scope = entry.parent_node_id.as_ref().is_none_or(|parent| {
                            subject.id == *parent
                                || session
                                    .tree
                                    .descendants(parent)
                                    .into_iter()
                                    .any(|descendant| descendant.id == subject.id)
                        });
                        if belongs_to_scope && !subject.lifecycle.is_running() {
                            tracing::debug!(
                                session = %entry.session_id,
                                node = %subject.id,
                                external_id,
                                "dropped a stored unresolved demand: its declared subject is terminal"
                            );
                            return false;
                        }
                    }
                }
            }
            true
        });
        self.attention = AttentionManager::from_persisted_queue(queue);
        Ok(())
    }

    /// Works out what happened to one session's processes and reports it.
    fn decide_session(&mut self, id: &SessionId, now_ms: i64) -> ServerEvent {
        // Verdicts first, with only an immutable borrow, so the process table can be
        // consulted without the tree being mutably borrowed at the same time.
        let mut verdicts: Vec<(turn_core::ids::NodeId, Lifecycle)> = Vec::new();
        match self.sessions.get(id) {
            Some(session) => {
                for node in session.tree.iter() {
                    if node.lifecycle != Lifecycle::Orphaned {
                        continue;
                    }
                    let alive = match node.pid {
                        Some(pid) => {
                            self.supervisor.is_alive(pid)
                                && self.matches_command(pid, &node.command)
                        }
                        None => false,
                    };
                    verdicts.push((
                        node.id.clone(),
                        if alive {
                            Lifecycle::Orphaned
                        } else {
                            Lifecycle::Lost
                        },
                    ));
                }
            }
            None => {
                return ServerEvent::RestoreResult {
                    session_id: id.clone(),
                    state: RestoreState::Live,
                    needs_explanation: false,
                    panes: Vec::new(),
                }
            }
        }

        // Also while the borrow is immutable: what each pane's offer would need from the
        // checkout. A pane that only opens the user's shell is not gated on write access,
        // and the UI has to be told which is which rather than blocking the lot.
        let needs_write: HashMap<PaneId, bool> = match self.sessions.get(id) {
            Some(session) => session
                .layout
                .panes()
                .iter()
                .map(|pane| {
                    (
                        pane.id.clone(),
                        self.pane_launch_authority(id, &pane.id)
                            == crate::core::spawn::LaunchAuthority::CheckoutWrite,
                    )
                })
                .collect(),
            None => HashMap::new(),
        };

        let Some(session) = self.sessions.get_mut(id) else {
            return ServerEvent::RestoreResult {
                session_id: id.clone(),
                state: RestoreState::Live,
                needs_explanation: false,
                panes: Vec::new(),
            };
        };

        let mut orphaned = 0usize;
        let mut lost = 0usize;
        for (node_id, verdict) in verdicts {
            match verdict {
                Lifecycle::Orphaned => orphaned += 1,
                _ => lost += 1,
            }
            if let Some(node) = session.tree.get_mut(&node_id) {
                if verdict == Lifecycle::Lost {
                    node.ended_ms.get_or_insert(now_ms);
                    super::events::clear_interaction_state(node);
                }
                node.lifecycle = verdict;
            }
        }

        let mut panes = Vec::new();
        for pane in session.layout.panes() {
            let Some(node_id) = pane.node_id.clone() else {
                continue;
            };
            let Some(node) = session.tree.get(&node_id) else {
                continue;
            };
            let shell_fallback = pane.kind.is_terminal()
                && pane
                    .command
                    .as_deref()
                    .is_none_or(|command| command.trim().is_empty());
            let relaunchable = pane.restore != RestoreBehaviour::Skip
                && !node.lifecycle.is_running()
                && (pane.command.is_some() || shell_fallback);
            panes.push(PaneRestoreOutcome {
                pane_id: pane.id.clone(),
                node_id,
                lifecycle: node.lifecycle.clone(),
                can_relaunch: relaunchable,
                // `Relaunch` means running this again is harmless. A commandless terminal is
                // harmless too, including legacy layouts that predate that metadata: it becomes
                // the configured shell rather than a dead panel requiring a click.
                auto_start: relaunchable
                    && (pane.restore == RestoreBehaviour::Relaunch || shell_fallback),
                // Descriptive only; relaunch authority is the durable node id.
                command: pane.command.clone(),
                // Absent means "assume it does", which is also the honest answer for a
                // pane whose launch shape could not be resolved at all.
                needs_checkout_write: needs_write.get(&pane.id).copied().unwrap_or(true),
            });
        }

        // `Live` when there was nothing to recover, because there is then nothing to
        // explain. `RestoreState::needs_explanation` is the flag that tells the UI to
        // say something went wrong, and a flag that fires for every session — the ones
        // that were merely idle, the ones that never ran anything — says nothing at all,
        // which is the same as being silent about the sessions that really did lose work.
        let state = if orphaned == 0 && lost == 0 {
            RestoreState::Live
        } else if orphaned > 0 {
            RestoreState::PartiallyRestored
        } else {
            RestoreState::LayoutOnly
        };
        session.restore_state = state;
        let eager: usize = session
            .layout
            .panes()
            .iter()
            .filter(|pane| pane.restore == RestoreBehaviour::Relaunch)
            .count();

        tracing::info!(
            session = %id, orphaned, lost, eager_offers = eager, state = ?state,
            "restored a session; waiting for a connected window to start safe panes"
        );
        ServerEvent::RestoreResult {
            session_id: id.clone(),
            state,
            needs_explanation: state.needs_explanation(),
            panes,
        }
    }

    /// Whether the process at `pid` still looks like the one we recorded.
    ///
    /// Pids are reused. Without this check a session could report a stranger's process
    /// as its own surviving agent, which is worse than reporting it lost: the user
    /// would be told their work is still running when it is not.
    ///
    /// This decides what to *show*, and so it is stricter than the identical question
    /// asked about write authority in [`super::authority`]: a process Turn cannot
    /// recognise is one it cannot display, but it may still be writing to the checkout.
    fn matches_command(&self, pid: u32, command: &str) -> bool {
        let Some(observed) = self.supervisor.observe(pid) else {
            return false;
        };
        super::authority::runs_the_recorded_command(command, &observed)
    }
}

fn migrate_obsolete_navigation_panes(session: &mut Session) -> bool {
    let obsolete: Vec<_> = session
        .layout
        .panes()
        .iter()
        .filter(|pane| pane.kind == PaneKind::AgentTree)
        .map(|pane| pane.id.clone())
        .collect();
    for pane_id in &obsolete {
        let pane = session
            .layout
            .get_mut(pane_id)
            .expect("the Pane id came from this Layout");
        pane.kind = PaneKind::Shell;
        pane.title = Some("shell".into());
        pane.command = None;
        pane.args.clear();
        pane.cwd = None;
        pane.env.clear();
        pane.node_id = None;
        pane.restore = RestoreBehaviour::Relaunch;
    }
    !obsolete.is_empty()
}

#[cfg(test)]
mod migration_tests {
    use super::*;
    use crate::core::testing::Harness;
    use crate::core::FailedIngestCheckpoint;
    use turn_core::event::{Confidence, EventKind, EventSource, Risk, TurnEvent};
    use turn_core::ids::{PaneId, WorkspaceId};
    use turn_core::model::{Direction, Layout, Pane, PendingPermission, ProcessNode};
    use turn_core::state::{AwaitingReason, Turn};

    const NOW: i64 = 1_775_000_000_000;

    #[test]
    fn a_legacy_session_keeps_its_geometry_but_not_a_second_navigator() {
        let agent = Pane::new(PaneKind::Agent).with_command("claude");
        let mut layout = Layout::single(agent);
        let agent_id = layout.active.clone().unwrap();
        layout.split(
            &agent_id,
            Direction::Horizontal,
            Pane::new(PaneKind::AgentTree),
        );
        let mut session = Session::new(
            WorkspaceId::from_stored("ws_restore"),
            "Legacy",
            "/repo",
            layout,
            1,
        );
        let pane_ids: Vec<_> = session
            .layout
            .panes()
            .iter()
            .map(|pane| pane.id.clone())
            .collect();

        assert!(migrate_obsolete_navigation_panes(&mut session));
        assert_eq!(
            session
                .layout
                .panes()
                .iter()
                .map(|pane| pane.id.clone())
                .collect::<Vec<_>>(),
            pane_ids
        );
        assert!(session
            .layout
            .panes()
            .iter()
            .all(|pane| pane.kind != PaneKind::AgentTree));
        assert!(session
            .layout
            .panes()
            .iter()
            .any(|pane| pane.kind == PaneKind::Shell && pane.command.is_none()));
        assert!(!migrate_obsolete_navigation_panes(&mut session));
    }

    #[tokio::test]
    async fn a_runtime_lost_during_restore_cannot_keep_interaction_metadata() {
        let mut harness = Harness::new().await;
        let session_id = SessionId::from_stored("sess_restore_lost_waiting");
        harness.add_session(session_id.clone(), PaneId::new(), NOW);
        let mut agent = ProcessNode::agent(session_id.clone(), "claude", "/tmp", NOW);
        agent.lifecycle = Lifecycle::Orphaned;
        agent.pid = Some(u32::MAX);
        agent.turn = Some(Turn::AwaitingUser {
            reason: AwaitingReason::Permission,
        });
        agent.interaction_pending = true;
        if let Some(info) = agent.agent.as_mut() {
            info.pending_permission = Some(PendingPermission {
                summary: "stale permission".into(),
                command: Some("cargo test".into()),
                tool_name: Some("Bash".into()),
                risk: Risk::Low,
                requested_ms: NOW,
                cwd: Some("/tmp".into()),
            });
            info.pending_question = Some("stale question".into());
        }
        let node_id = agent.id.clone();
        harness
            .core
            .sessions
            .get_mut(&session_id)
            .unwrap()
            .tree
            .insert(agent);

        harness.core.decide_session(&session_id, NOW + 1);
        harness.core.persist_session(&session_id).unwrap();

        let node = harness.core.sessions[&session_id]
            .tree
            .get(&node_id)
            .unwrap();
        assert_eq!(node.lifecycle, Lifecycle::Lost);
        assert!(!node.interaction_pending);
        assert!(!node.turn.as_ref().is_some_and(Turn::needs_user));
        assert!(node.agent.as_ref().is_some_and(|agent| {
            agent.pending_permission.is_none() && agent.pending_question.is_none()
        }));
        let persisted = harness
            .core
            .store
            .sessions()
            .get(&session_id)
            .unwrap()
            .unwrap();
        let node = persisted.tree.get(&node_id).unwrap();
        assert_eq!(node.lifecycle, Lifecycle::Lost);
        assert!(!node.interaction_pending);
        assert!(node.agent.as_ref().is_some_and(|agent| {
            agent.pending_permission.is_none() && agent.pending_question.is_none()
        }));
    }

    #[tokio::test]
    async fn failed_recovery_persistence_rolls_back_orphan_reconciliation() {
        let mut harness = Harness::new().await;
        let session_id = SessionId::from_stored("sess_restore_persist_failure");
        let pane_id = PaneId::from_stored("pane_restore_persist_failure");
        harness.add_session(session_id.clone(), pane_id.clone(), NOW);
        let mut process = ProcessNode::process(
            session_id.clone(),
            turn_core::model::NodeKind::Shell,
            "sh",
            "/tmp",
            NOW,
        );
        process.lifecycle = Lifecycle::Orphaned;
        process.pid = Some(u32::MAX);
        let node_id = process.id.clone();
        {
            let session = harness.core.sessions.get_mut(&session_id).unwrap();
            session.tree.insert(process);
            let pane = session.layout.get_mut(&pane_id).unwrap();
            pane.node_id = Some(node_id.clone());
            pane.command = Some("sh".into());
            session.restore_state = RestoreState::PartiallyRestored;
        }
        harness.core.persist_session(&session_id).unwrap();
        harness
            .core
            .restore_reports
            .push(ServerEvent::RestoreResult {
                session_id: session_id.clone(),
                state: RestoreState::PartiallyRestored,
                needs_explanation: true,
                panes: vec![PaneRestoreOutcome {
                    pane_id,
                    node_id: node_id.clone(),
                    lifecycle: Lifecycle::Orphaned,
                    can_relaunch: false,
                    command: Some("sh".into()),
                    auto_start: false,
                    needs_checkout_write: false,
                }],
            });
        harness
            .core
            .failed_ingest_checkpoints
            .push_back(FailedIngestCheckpoint {
                event: TurnEvent::new(
                    session_id.clone(),
                    EventKind::AgentIdle,
                    EventSource::Supervisor,
                    Confidence::Explicit,
                    NOW + 1,
                )
                .with_node(node_id.clone()),
                effects: Vec::new(),
            });

        let error = harness
            .core
            .reconcile_orphaned_recovery(&session_id, NOW + 2)
            .expect_err("write authority cannot advance past an undurable Session");
        assert_eq!(error.code, turn_proto::ErrorCode::Unavailable);
        assert_eq!(
            harness.core.sessions[&session_id]
                .tree
                .get(&node_id)
                .unwrap()
                .lifecycle,
            Lifecycle::Orphaned
        );
        assert_eq!(
            harness
                .core
                .store
                .sessions()
                .get(&session_id)
                .unwrap()
                .unwrap()
                .tree
                .get(&node_id)
                .unwrap()
                .lifecycle,
            Lifecycle::Orphaned
        );
        assert!(matches!(
            harness.core.restore_reports.as_slice(),
            [ServerEvent::RestoreResult { panes, .. }]
                if panes[0].lifecycle == Lifecycle::Orphaned && !panes[0].can_relaunch
        ));
    }

    #[tokio::test]
    async fn resolving_the_last_offer_prevents_a_new_client_from_seeing_stale_recovery() {
        let mut harness = Harness::new().await;
        let session_id = SessionId::from_stored("sess_resolved_restore");
        let pane_id = PaneId::from_stored("pane_resolved_restore");
        let node_id = turn_core::ids::NodeId::from_stored("proc_resolved_restore");
        harness.add_session(session_id.clone(), pane_id.clone(), NOW);
        harness
            .core
            .sessions
            .get_mut(&session_id)
            .unwrap()
            .restore_state = RestoreState::LayoutOnly;
        harness
            .core
            .restore_reports
            .push(ServerEvent::RestoreResult {
                session_id: session_id.clone(),
                state: RestoreState::LayoutOnly,
                needs_explanation: true,
                panes: vec![PaneRestoreOutcome {
                    pane_id,
                    node_id: node_id.clone(),
                    lifecycle: Lifecycle::Lost,
                    can_relaunch: true,
                    command: Some("sh".into()),
                    auto_start: false,
                    needs_checkout_write: false,
                }],
            });

        let update = harness
            .core
            .resolve_restore_node(&session_id, &node_id)
            .expect("the old offer existed");
        assert!(matches!(
            update,
            ServerEvent::RestoreResult {
                state: RestoreState::Live,
                needs_explanation: false,
                ref panes,
                ..
            } if panes.is_empty()
        ));
        assert_eq!(
            harness.core.sessions[&session_id].restore_state,
            RestoreState::Live
        );
        assert!(matches!(
            harness.core.restore_reports.as_slice(),
            [ServerEvent::RestoreResult {
                state: RestoreState::Live,
                needs_explanation: false,
                panes,
                ..
            }] if panes.is_empty()
        ));
    }
}
