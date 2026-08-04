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
use turn_core::attention::{Action, AttentionPolicy, Trigger};
use turn_core::event::{Confidence, EventKind, EventSource, TurnEvent};
use turn_core::ids::SessionId;
use turn_core::model::{RestoreBehaviour, RestoreState};
use turn_core::state::Lifecycle;
use turn_proto::{PaneRestoreOutcome, ServerEvent};

impl Core {
    /// Loads everything from the store and decides what became of each process.
    pub(crate) fn restore(&mut self, now_ms: i64) -> Result<()> {
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
            if let Some(session) = self.store.sessions().load_for_restore(id)? {
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
        self.restore_queue(now_ms)?;
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
            scratch_pruned = pruned,
            "restored"
        );
        Ok(())
    }

    /// Rebuilds the attention queue from the store.
    ///
    /// Replayed through the manager rather than assigned, because the manager owns the
    /// queue and its deduplication rules. The replay uses a policy that does nothing
    /// but enqueue: a daemon starting must not fire the notifications and sounds for
    /// demands the user has already seen, and it must certainly not move their focus
    /// before a window even exists.
    ///
    /// Two things do not survive, and it is better to say so than to imply otherwise:
    /// an entry's original age (the ranking clock starts again now) and a snooze (a
    /// postponed demand comes back pending). Both would need the manager to accept a
    /// queue wholesale.
    fn restore_queue(&mut self, now_ms: i64) -> Result<()> {
        let policy = AttentionPolicy {
            on_waiting_for_user: vec![Action::Enqueue],
            ..AttentionPolicy::silent()
        };
        debug_assert!(
            policy
                .resolve(Trigger::WaitingForUser, Confidence::Explicit)
                .iter()
                .all(|action| !action.is_focus()),
            "restoring the queue must not be able to move the user"
        );

        let context = turn_core::UserContext::default();
        for entry in self.store.attention().list()? {
            let Some(session) = self.sessions.get(&entry.session_id) else {
                continue;
            };
            // A demand belongs to a process. If that process turned out to be gone, the
            // demand went with it: the agent that was waiting for an answer is not there
            // to receive one, and putting it back would have the user open Turn to a
            // question nobody is asking any more.
            if let Some(node) = &entry.node_id {
                let still_there = session
                    .tree
                    .get(node)
                    .is_some_and(|node| node.lifecycle.is_running());
                if !still_there {
                    tracing::debug!(
                        session = %entry.session_id, %node,
                        "dropped a stored demand: its process did not survive"
                    );
                    continue;
                }
            }
            let mut event = TurnEvent::new(
                entry.session_id.clone(),
                EventKind::AgentWaitingForUser {
                    reason: entry.reason,
                    summary: entry.summary.clone(),
                },
                // The confidence the demand was originally raised with is preserved, so
                // a demand a heuristic guessed at still ranks as provisional and still
                // cannot be promoted into a focus change by coming back from disk.
                EventSource::Hook {
                    tool: "restore".to_string(),
                    event_name: "attention".to_string(),
                },
                entry.confidence,
                now_ms,
            );
            if let Some(node) = &entry.node_id {
                event = event.with_node(node.clone());
            }
            let effects = self.attention.ingest(&event, &policy, &context, now_ms);
            debug_assert!(
                effects.iter().all(|effect| !matches!(
                    effect,
                    turn_core::Effect::Focus { .. } | turn_core::Effect::Notify { .. }
                )),
                "restoring a demand produced an effect the user would notice"
            );
        }
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
