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
//! **Nothing is relaunched.** Panes that could be started again are marked
//! `can_relaunch` with the command shown verbatim, and that is an offer. The user
//! answers it with [`turn_proto::Request::RelaunchNode`] or does not.

use super::Core;
use crate::error::Result;
use crate::paths;
use std::collections::HashSet;
use turn_core::attention::AttentionManager;
use turn_core::ids::SessionId;
use turn_core::model::{PaneKind, RestoreBehaviour, RestoreState, Session};
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
                if migrate_obsolete_navigation_panes(&mut session) {
                    // The old central AgentTree cannot coexist with the unified
                    // sidebar. Persist the structural migration immediately, but
                    // never materialise Fang during restore.
                    self.store.sessions().save(&session)?;
                }
                self.sessions.insert(session.id.clone(), session);
            }
        }

        // One refresh for every decision below. Deciding a process is gone requires
        // looking at the process table, which the store rightly refuses to guess at.
        if !self.sessions.is_empty() {
            self.supervisor.refresh();
            self.last_sweep_ms = now_ms;
        }
        let ids: Vec<SessionId> = self.sessions.keys().cloned().collect();
        for id in &ids {
            let report = self.decide_session(id, now_ms);
            self.restore_reports.push(report);
        }

        // After the verdicts, so a demand raised by a process that turned out to be gone
        // is not put back in front of the user.
        self.restore_queue()?;
        // Written straight back, so the demands this start-up decided against do not sit
        // on disk waiting to be reconsidered by the next one.
        self.persist_attention();

        let known: HashSet<String> = self.sessions.keys().map(|id| id.to_string()).collect();
        let pruned = paths::prune_scratch(&self.data_dir, &known);

        tracing::info!(
            workspaces = self.workspaces.len(),
            sessions = self.sessions.len(),
            templates = self.templates.len(),
            built_ins_installed = installed,
            attention = self.attention.queue().len(),
            leases_requiring_recovery,
            temporary_bindings_pruned,
            scratch_pruned = pruned,
            "restored"
        );
        Ok(())
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
            let relaunchable = pane.restore != RestoreBehaviour::Skip
                && !node.lifecycle.is_running()
                && (pane.command.is_some() || pane.kind == turn_core::model::PaneKind::Shell);
            panes.push(PaneRestoreOutcome {
                pane_id: pane.id.clone(),
                node_id: Some(node_id),
                lifecycle: node.lifecycle.clone(),
                can_relaunch: relaunchable,
                // Shown verbatim so accepting the offer is an informed choice.
                command: pane.command.clone(),
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
            "restored a session; nothing was relaunched"
        );
        self.persist_session_quietly(id);

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
    fn matches_command(&self, pid: u32, command: &str) -> bool {
        let Some(observed) = self.supervisor.observe(pid) else {
            return false;
        };
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
        pane.kind = PaneKind::Tui;
        pane.title = Some("fang (files)".into());
        pane.command = Some("fang".into());
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
    use turn_core::event::Risk;
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
}
