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
use turn_core::model::{
    AgentIdentitySource, NodeKind, PaneKind, PreviewVisibility, ProcessNode, RestoreBehaviour,
    RestoreState, Session, SessionMode,
};
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
        let protected_attention_nodes: HashSet<NodeId> = self
            .store
            .attention()
            .load_queue()?
            .iter()
            .flat_map(|entry| [entry.node_id.clone(), entry.parent_node_id.clone()])
            .flatten()
            .collect();
        let mut subagent_alias_repairs = 0usize;
        for id in &stored {
            // `load_for_restore` downgrades anything stored as running to `Orphaned`,
            // because a stored "alive" only ever meant "alive when we last wrote".
            if let Some(mut session) = self.store.sessions().load_for_restore(id)? {
                let mut protected_nodes = protected_attention_nodes.clone();
                protected_nodes.extend(session.tree.iter().filter_map(|node| {
                    std::fs::symlink_metadata(paths::node_terminal_history(
                        &self.data_dir,
                        &session.id,
                        &node.id,
                    ))
                    .is_ok()
                    .then_some(node.id.clone())
                }));
                let repairs = repair_legacy_claude_subagent_aliases(&mut session, &protected_nodes);
                let repaired = repairs.len();
                subagent_alias_repairs += repaired;
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
                if navigation_migrated || guard_downgraded || repaired > 0 {
                    // The old central AgentTree cannot coexist with the unified
                    // sidebar. Persist structural/guard truth immediately, but never
                    // materialise the replacement Shell during restore. False is not
                    // upgraded here: legacy/orphaned processes may never have been
                    // sandboxed even when this platform can guard a future launch.
                    if repairs.is_empty() {
                        self.store.sessions().save(&session)?;
                    } else {
                        self.store
                            .sessions()
                            .save_after_node_remaps(&session, &repairs)?;
                    }
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
            subagent_alias_repairs,
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

#[derive(Default)]
struct LegacyAliasPair {
    lifecycle: Vec<NodeId>,
    parent_spawn: Vec<NodeId>,
}

/// Repairs the exact duplicate shape written before structured Agent aliases existed.
///
/// The projection is intentionally narrow: both rows must be leaf siblings under a
/// Claude parent, one must have the live lifecycle id shape (`a<name>-*`), the other
/// the parent declaration shape (`<name>@session-*`), and each side must be unique for
/// that name. If two homonymous workers exist, or two durable references require both
/// node ids to survive, restoration leaves the rows untouched rather than guessing.
fn repair_legacy_claude_subagent_aliases(
    session: &mut Session,
    attention_nodes: &HashSet<NodeId>,
) -> Vec<(NodeId, NodeId)> {
    let mut protected = attention_nodes.clone();
    protected.extend(
        session
            .layout
            .panes()
            .iter()
            .filter_map(|pane| pane.node_id.clone()),
    );

    let mut pairs: HashMap<(NodeId, String), LegacyAliasPair> = HashMap::new();
    for node in session.tree.iter() {
        let Some(parent) = node.parent.clone() else {
            continue;
        };
        if node.kind != NodeKind::Subagent
            || !session.tree.children(&node.id).is_empty()
            || !is_claude_parent(&session.tree, &parent)
        {
            continue;
        }
        let Some((source, alias)) = legacy_claude_identity(node) else {
            continue;
        };
        let pair = pairs.entry((parent, alias)).or_default();
        match source {
            AgentIdentitySource::Lifecycle => pair.lifecycle.push(node.id.clone()),
            AgentIdentitySource::ParentSpawn => pair.parent_spawn.push(node.id.clone()),
        }
    }

    let mut repairs = Vec::new();
    for ((_parent, alias), pair) in pairs {
        let ([lifecycle_id], [team_id]) = (pair.lifecycle.as_slice(), pair.parent_spawn.as_slice())
        else {
            continue;
        };
        let lifecycle_id = lifecycle_id.clone();
        let team_id = team_id.clone();
        if protected.contains(&lifecycle_id) && protected.contains(&team_id) {
            continue;
        }
        let Some(lifecycle) = session.tree.get(&lifecycle_id).cloned() else {
            continue;
        };
        let Some(team) = session.tree.get(&team_id).cloned() else {
            continue;
        };
        let Some(lifecycle_external) = single_legacy_external_id(&lifecycle).map(str::to_string)
        else {
            continue;
        };
        let Some(team_external) = single_legacy_external_id(&team).map(str::to_string) else {
            continue;
        };

        // Preserve whichever id is referenced durably. With no such constraint,
        // keep the parent-spawn row because it owns the semantic task preview/history.
        let survivor_id = if protected.contains(&lifecycle_id) {
            lifecycle_id.clone()
        } else {
            team_id.clone()
        };
        let removed_id = if survivor_id == lifecycle_id {
            team_id
        } else {
            lifecycle_id
        };
        let Some(survivor) = session.tree.get_mut(&survivor_id) else {
            continue;
        };
        merge_legacy_claude_alias_pair(
            survivor,
            &lifecycle,
            &team,
            &lifecycle_external,
            &team_external,
        );
        session.tree.remove(&removed_id);
        tracing::info!(
            session = %session.id,
            survivor = %survivor_id,
            removed = %removed_id,
            alias,
            "repaired duplicate Claude subagent identities"
        );
        repairs.push((removed_id, survivor_id));
    }
    repairs
}

fn is_claude_parent(tree: &turn_core::model::SessionTree, parent: &NodeId) -> bool {
    let Some(parent) = tree.get(parent) else {
        return false;
    };
    if parent.agent.as_ref().is_some_and(|agent| {
        agent.agent.tool.as_deref() == Some("claude-code")
            || agent.agent.provider.as_deref() == Some("anthropic")
    }) {
        return true;
    }
    parent
        .command
        .split_whitespace()
        .next()
        .and_then(|command| command.rsplit('/').next())
        == Some("claude")
}

fn single_legacy_external_id(node: &ProcessNode) -> Option<&str> {
    let agent = node.agent.as_ref()?;
    if !agent.identity_aliases.is_empty() {
        return None;
    }
    match (
        agent.external_id.as_deref(),
        agent.agent.external_id.as_deref(),
    ) {
        (Some(left), Some(right)) if left != right => None,
        (Some(id), _) | (_, Some(id)) => Some(id),
        (None, None) => None,
    }
}

fn legacy_claude_identity(node: &ProcessNode) -> Option<(AgentIdentitySource, String)> {
    let agent = node.agent.as_ref()?;
    let external_id = single_legacy_external_id(node)?;
    if let Some((name, session)) = external_id.split_once('@') {
        if !name.is_empty()
            && session.starts_with("session-")
            && agent.name.declared_name.as_deref() == Some(name)
            // A user rename changes only the display name and deliberately keeps
            // the parent's declaration. Treat that stronger user-authored state as
            // compatible with this otherwise exact legacy identity shape so repair
            // can retain it instead of leaving a known duplicate forever.
            && (agent.name.user_renamed || node.resolved_title().0 == name)
        {
            return Some((AgentIdentitySource::ParentSpawn, name.to_string()));
        }
    }

    let alias = agent.agent_type.as_deref()?;
    let suffix = external_id.strip_prefix('a')?.strip_prefix(alias)?;
    if agent.name.declared_name.is_none()
        && (agent.name.user_renamed || node.resolved_title().0 == alias)
        && suffix.starts_with('-')
        && suffix.len() > 1
    {
        return Some((AgentIdentitySource::Lifecycle, alias.to_string()));
    }
    None
}

fn merge_legacy_claude_alias_pair(
    survivor: &mut ProcessNode,
    lifecycle: &ProcessNode,
    team: &ProcessNode,
    lifecycle_external: &str,
    team_external: &str,
) {
    // Do not invent a process merge here. Both legacy rows were virtual subagents
    // created by `insert_subagent_from`: each copied the same parent's command/cwd,
    // carried empty args and no pid/ppid, and used the same confirmed SpawnedBy
    // relationship. The lifecycle row is an Agent lifecycle identity, not a second
    // OS runtime. NodeId-sensitive process history is handled by the protected-id
    // choice above and by the store's transactional remap.
    survivor.started_ms = survivor
        .started_ms
        .min(lifecycle.started_ms.min(team.started_ms));
    survivor.lifecycle = lifecycle.lifecycle.clone();
    survivor.turn = lifecycle.turn.clone();
    survivor.ended_ms = lifecycle.ended_ms;
    survivor.exit_code = lifecycle.exit_code;
    // Both historical rows could receive semantic events. Keep the newest compact
    // projection rather than blindly preferring the spawn declaration, then bind
    // the nested typed identity to the surviving row as well as remapping the
    // standalone preview-history table in the store.
    survivor.activity_preview = match (&team.activity_preview, &lifecycle.activity_preview) {
        (Some(team), Some(lifecycle)) if lifecycle.updated_ms > team.updated_ms => {
            Some(lifecycle.clone())
        }
        (Some(team), _) => Some(team.clone()),
        (None, Some(lifecycle)) => Some(lifecycle.clone()),
        (None, None) => None,
    };
    if let Some(preview) = survivor.activity_preview.as_mut() {
        preview.node_id = survivor.id.clone();
    }
    // Visibility is a user/privacy choice. Either explicit choice beats Inherit,
    // and Hide wins an otherwise unknowable conflict so repair cannot expose text
    // the operator had hidden on one of the duplicate rows.
    survivor.preview_visibility = match (team.preview_visibility, lifecycle.preview_visibility) {
        (PreviewVisibility::Hide, _) | (_, PreviewVisibility::Hide) => PreviewVisibility::Hide,
        (PreviewVisibility::Show, _) | (_, PreviewVisibility::Show) => PreviewVisibility::Show,
        (PreviewVisibility::Inherit, PreviewVisibility::Inherit) => PreviewVisibility::Inherit,
    };
    if survivor.process_title.is_none() {
        survivor.process_title = team
            .process_title
            .clone()
            .or_else(|| lifecycle.process_title.clone());
    }
    for (key, value) in lifecycle
        .env_highlights
        .iter()
        .chain(team.env_highlights.iter())
    {
        survivor
            .env_highlights
            .entry(key.clone())
            .or_insert_with(|| value.clone());
    }
    if survivor.user_title.is_none() {
        survivor.user_title = team
            .user_title
            .clone()
            .or_else(|| lifecycle.user_title.clone());
    }

    let lifecycle_info = lifecycle.agent.as_ref();
    let team_info = team.agent.as_ref();
    if let Some(agent) = survivor.agent.as_mut() {
        // Parent-spawn owns declarations; lifecycle owns observations made while
        // the worker ran. The old merge accidentally used `survivor` as the
        // fallback, which meant the normal team-row survivor could never inherit
        // lifecycle-only metadata. Refer to both source rows explicitly so the
        // result is independent of which NodeId durable references force us to keep.
        let renamed = [team_info, lifecycle_info]
            .into_iter()
            .flatten()
            .find(|candidate| candidate.name.user_renamed);
        if let Some(renamed) = renamed {
            agent.name = renamed.name.clone();
        } else if let Some(team_agent) = team_info {
            agent.name = team_agent.name.clone();
        } else if let Some(lifecycle_agent) = lifecycle_info {
            agent.name = lifecycle_agent.name.clone();
        }

        agent.agent_type = team_info
            .and_then(|candidate| candidate.agent_type.clone())
            .or_else(|| lifecycle_info.and_then(|candidate| candidate.agent_type.clone()));
        agent.current_task = team_info
            .and_then(|candidate| candidate.current_task.clone())
            .or_else(|| lifecycle_info.and_then(|candidate| candidate.current_task.clone()));

        agent.agent.provider = lifecycle_info
            .and_then(|candidate| candidate.agent.provider.clone())
            .or_else(|| team_info.and_then(|candidate| candidate.agent.provider.clone()));
        agent.agent.tool = lifecycle_info
            .and_then(|candidate| candidate.agent.tool.clone())
            .or_else(|| team_info.and_then(|candidate| candidate.agent.tool.clone()));
        agent.agent.model = lifecycle_info
            .and_then(|candidate| candidate.agent.model.clone())
            .or_else(|| team_info.and_then(|candidate| candidate.agent.model.clone()));
        agent.last_message = lifecycle_info
            .and_then(|candidate| candidate.last_message.clone())
            .or_else(|| team_info.and_then(|candidate| candidate.last_message.clone()));
        agent.tokens_used = lifecycle_info
            .and_then(|candidate| candidate.tokens_used)
            .or_else(|| team_info.and_then(|candidate| candidate.tokens_used));
        agent.cost_usd = lifecycle_info
            .and_then(|candidate| candidate.cost_usd)
            .or_else(|| team_info.and_then(|candidate| candidate.cost_usd));
        agent.permission_mode = lifecycle_info
            .and_then(|candidate| candidate.permission_mode.clone())
            .or_else(|| team_info.and_then(|candidate| candidate.permission_mode.clone()));
        agent.runtime = team_info
            .map(|candidate| candidate.runtime.clone())
            .unwrap_or_default()
            .prefer_newer(
                lifecycle_info
                    .map(|candidate| candidate.runtime.clone())
                    .unwrap_or_default(),
            );
        agent.git_branch = lifecycle_info
            .and_then(|candidate| candidate.git_branch.clone())
            .or_else(|| team_info.and_then(|candidate| candidate.git_branch.clone()));
        agent.resumable = team_info.is_some_and(|candidate| candidate.resumable)
            || lifecycle_info.is_some_and(|candidate| candidate.resumable);

        agent.external_id = Some(lifecycle_external.to_string());
        agent.agent.external_id = Some(lifecycle_external.to_string());
        agent.record_identity_alias(
            AgentIdentitySource::Lifecycle,
            lifecycle_external.to_string(),
        );
        agent.record_identity_alias(AgentIdentitySource::ParentSpawn, team_external.to_string());
        if survivor.lifecycle.is_terminal() {
            agent.pending_permission = None;
            agent.pending_question = None;
        } else {
            agent.pending_permission = match (
                lifecycle_info.and_then(|candidate| candidate.pending_permission.as_ref()),
                team_info.and_then(|candidate| candidate.pending_permission.as_ref()),
            ) {
                (Some(lifecycle), Some(team)) if team.requested_ms > lifecycle.requested_ms => {
                    Some(team.clone())
                }
                (Some(lifecycle), _) => Some(lifecycle.clone()),
                (None, Some(team)) => Some(team.clone()),
                (None, None) => None,
            };
            agent.pending_question = lifecycle_info
                .and_then(|candidate| candidate.pending_question.clone())
                .or_else(|| team_info.and_then(|candidate| candidate.pending_question.clone()));
        }
        if !agent.name.user_renamed {
            survivor.title = agent.name.display_name.clone();
        }
    }
    if survivor.lifecycle.is_terminal() {
        survivor.interaction_pending = false;
    } else {
        survivor.interaction_pending = lifecycle.interaction_pending || team.interaction_pending;
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
        pane.launch_profile = None;
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
    use turn_core::ids::{HandoffId, PaneId, WorkspaceId};
    use turn_core::model::{
        ActivityPreview, AgentName, ContextHandoffMode, ContextHandoffOutcome, Direction, Layout,
        NameSource, Pane, PendingPermission, PreviewSource, ProcessNode, Relation,
    };
    use turn_core::state::{AwaitingReason, Turn};

    const NOW: i64 = 1_775_000_000_000;

    #[tokio::test]
    async fn restore_repairs_one_unambiguous_legacy_claude_alias_pair_and_persists_it() {
        let mut harness = Harness::new().await;
        let session_id = SessionId::from_stored("sess_restore_alias_pair");
        harness.add_session(
            session_id.clone(),
            PaneId::from_stored("pane_restore_alias_pair"),
            NOW,
        );
        let mut parent = ProcessNode::agent(session_id.clone(), "claude", "/tmp", NOW);
        parent.lifecycle = Lifecycle::Alive;
        let parent_id = parent.id.clone();

        let mut team = ProcessNode::agent(session_id.clone(), "claude", "/tmp", NOW + 1);
        team.kind = NodeKind::Subagent;
        team.title = "frase-1".into();
        team.lifecycle = Lifecycle::Alive;
        team.link_to(parent_id.clone(), Relation::Confirmed);
        let team_id = team.id.clone();
        {
            let info = team.agent.as_mut().unwrap();
            info.external_id = Some("frase-1@session-legacy".into());
            info.agent.external_id = info.external_id.clone();
            info.name = AgentName::declared("frase-1");
            info.agent_type = Some("general-purpose".into());
            info.current_task = Some("Combine three phrases".into());
        }

        let mut lifecycle = ProcessNode::agent(session_id.clone(), "claude", "/tmp", NOW + 2);
        lifecycle.kind = NodeKind::Subagent;
        lifecycle.title = "frase-1".into();
        lifecycle.lifecycle = Lifecycle::Exited { code: 0 };
        lifecycle.turn = Some(Turn::Done);
        lifecycle.ended_ms = Some(NOW + 3);
        lifecycle.link_to(parent_id.clone(), Relation::Confirmed);
        let lifecycle_id = lifecycle.id.clone();
        {
            let info = lifecycle.agent.as_mut().unwrap();
            info.external_id = Some("afrase-1-ea36227f2c953321".into());
            info.agent.external_id = info.external_id.clone();
            info.name = AgentName {
                declared_name: None,
                display_name: "frase-1".into(),
                source: NameSource::Integration,
                confidence: Confidence::Integrated,
                user_renamed: false,
            };
            info.agent_type = Some("frase-1".into());
        }

        let session = harness.core.sessions.get_mut(&session_id).unwrap();
        session.tree.insert(parent);
        session.tree.insert(team);
        session.tree.insert(lifecycle);
        harness.core.persist_session(&session_id).unwrap();

        let sent = TurnEvent::new(
            session_id.clone(),
            EventKind::ContextHandoffFinished {
                handoff_id: HandoffId::from_stored("handoff_legacy_sent"),
                target_node_id: parent_id.clone(),
                mode: ContextHandoffMode::ReviewHandoff,
                outcome: ContextHandoffOutcome::Submitted,
            },
            EventSource::UserAction,
            Confidence::Explicit,
            NOW + 4,
        )
        .with_node(lifecycle_id.clone());
        let received = TurnEvent::new(
            session_id.clone(),
            EventKind::ContextHandoffFinished {
                handoff_id: HandoffId::from_stored("handoff_legacy_received"),
                target_node_id: lifecycle_id.clone(),
                mode: ContextHandoffMode::SecondOpinion,
                outcome: ContextHandoffOutcome::Submitted,
            },
            EventSource::UserAction,
            Confidence::Explicit,
            NOW + 5,
        )
        .with_node(parent_id);
        harness
            .core
            .store
            .events()
            .append_all(&[sent, received])
            .unwrap();

        let mut restored = harness
            .core
            .store
            .sessions()
            .load_for_restore(&session_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            restored.tree.subagent_count(),
            2,
            "legacy duplicate fixture"
        );
        let repairs = repair_legacy_claude_subagent_aliases(&mut restored, &HashSet::new());
        assert_eq!(repairs.len(), 1);
        assert_eq!(restored.tree.subagent_count(), 1);
        assert!(restored.tree.get(&lifecycle_id).is_none());
        let worker = restored.tree.get(&team_id).unwrap();
        assert!(worker.lifecycle.is_terminal());
        assert_eq!(worker.resolved_title().0, "frase-1");
        assert_eq!(
            worker.agent.as_ref().unwrap().current_task.as_deref(),
            Some("Combine three phrases")
        );
        assert_eq!(
            restored
                .tree
                .find_by_external_id("frase-1@session-legacy")
                .unwrap()
                .id,
            team_id
        );
        assert_eq!(
            restored
                .tree
                .find_by_external_id("afrase-1-ea36227f2c953321")
                .unwrap()
                .id,
            team_id
        );

        harness
            .core
            .store
            .sessions()
            .save_after_node_remaps(&restored, &repairs)
            .unwrap();
        let round_trip = harness
            .core
            .store
            .sessions()
            .get(&session_id)
            .unwrap()
            .unwrap();
        assert_eq!(round_trip.tree.subagent_count(), 1);
        assert_eq!(
            round_trip
                .tree
                .find_by_external_id("frase-1@session-legacy")
                .unwrap()
                .id,
            round_trip
                .tree
                .find_by_external_id("afrase-1-ea36227f2c953321")
                .unwrap()
                .id
        );
        let handoffs = harness
            .core
            .store
            .events()
            .list_of_kind(&session_id, "context_handoff.finished", 10)
            .unwrap();
        assert_eq!(handoffs.len(), 2);
        assert!(handoffs.iter().all(|event| {
            event.node_id.as_ref() != Some(&lifecycle_id)
                && matches!(
                    &event.kind,
                    EventKind::ContextHandoffFinished { target_node_id, .. }
                        if target_node_id != &lifecycle_id
                )
        }));
        assert!(handoffs
            .iter()
            .any(|event| event.node_id.as_ref() == Some(&team_id)));
        assert!(handoffs.iter().any(|event| matches!(
            &event.kind,
            EventKind::ContextHandoffFinished { target_node_id, .. }
                if target_node_id == &team_id
        )));
    }

    #[tokio::test]
    async fn restore_team_survivor_keeps_lifecycle_runtime_metadata_and_user_choices() {
        let mut harness = Harness::new().await;
        let session_id = SessionId::from_stored("sess_restore_team_survivor_metadata");
        harness.add_session(
            session_id.clone(),
            PaneId::from_stored("pane_restore_team_survivor_metadata"),
            NOW,
        );
        let mut parent = ProcessNode::agent(session_id.clone(), "claude", "/tmp", NOW);
        parent.lifecycle = Lifecycle::Alive;
        let parent_id = parent.id.clone();

        let mut team = ProcessNode::agent(session_id.clone(), "claude", "/tmp", NOW + 1);
        team.kind = NodeKind::Subagent;
        team.title = "reviewer".into();
        team.lifecycle = Lifecycle::Alive;
        team.link_to(parent_id.clone(), Relation::Confirmed);
        let team_id = team.id.clone();
        {
            let info = team.agent.as_mut().unwrap();
            info.external_id = Some("reviewer@session-legacy".into());
            info.agent.external_id = info.external_id.clone();
            info.name = AgentName::declared("reviewer");
            info.agent_type = Some("code-reviewer".into());
            info.current_task = Some("Review the durable migration".into());
        }
        team.activity_preview = Some(ActivityPreview {
            node_id: team_id.clone(),
            raw_source_sequence: Some(11),
            normalized_text: "starting the durable migration review".into(),
            source: PreviewSource::SemanticEvent,
            confidence: Confidence::Explicit,
            stable: true,
            contains_sensitive_data: false,
            redacted: false,
            updated_ms: NOW + 1,
        });

        let mut lifecycle = ProcessNode::agent(session_id.clone(), "claude", "/tmp", NOW + 2);
        lifecycle.kind = NodeKind::Subagent;
        lifecycle.title = "reviewer".into();
        lifecycle.lifecycle = Lifecycle::Alive;
        lifecycle.turn = Some(Turn::AwaitingUser {
            reason: AwaitingReason::Permission,
        });
        lifecycle.interaction_pending = true;
        lifecycle.preview_visibility = PreviewVisibility::Hide;
        lifecycle.link_to(parent_id, Relation::Confirmed);
        let lifecycle_id = lifecycle.id.clone();
        lifecycle.activity_preview = Some(ActivityPreview {
            node_id: lifecycle_id.clone(),
            raw_source_sequence: Some(23),
            normalized_text: "waiting to run cargo test".into(),
            source: PreviewSource::SemanticEvent,
            confidence: Confidence::Explicit,
            stable: true,
            contains_sensitive_data: false,
            redacted: false,
            updated_ms: NOW + 3,
        });
        {
            let info = lifecycle.agent.as_mut().unwrap();
            info.external_id = Some("areviewer-runtime".into());
            info.agent.external_id = info.external_id.clone();
            info.agent.provider = Some("anthropic".into());
            info.agent.tool = Some("claude-code".into());
            info.agent.model = Some("claude-opus-4-1".into());
            info.name = AgentName {
                declared_name: None,
                display_name: "reviewer".into(),
                source: NameSource::Integration,
                confidence: Confidence::Integrated,
                user_renamed: false,
            };
            info.name.rename("My reviewer");
            info.agent_type = Some("reviewer".into());
            info.last_message = Some("I need permission before the test".into());
            info.pending_permission = Some(PendingPermission {
                summary: "Run the focused test".into(),
                command: Some("cargo test -p turnd".into()),
                tool_name: Some("Bash".into()),
                risk: Risk::Low,
                requested_ms: NOW + 3,
                cwd: Some("/tmp".into()),
            });
            info.pending_question = Some("Should I include ignored tests?".into());
            info.tokens_used = Some(12_345);
            info.cost_usd = Some(0.42);
            info.permission_mode = Some("default".into());
            info.git_branch = Some("feature/durable-repair".into());
            info.resumable = true;
        }

        let session = harness.core.sessions.get_mut(&session_id).unwrap();
        session.tree.insert(parent);
        session.tree.insert(team);
        session.tree.insert(lifecycle);
        harness.core.persist_session(&session_id).unwrap();

        let mut restored = harness
            .core
            .store
            .sessions()
            .load_for_restore(&session_id)
            .unwrap()
            .unwrap();
        let repairs = repair_legacy_claude_subagent_aliases(&mut restored, &HashSet::new());
        assert_eq!(repairs, [(lifecycle_id.clone(), team_id.clone())]);
        harness
            .core
            .store
            .sessions()
            .save_after_node_remaps(&restored, &repairs)
            .unwrap();

        let round_trip = harness
            .core
            .store
            .sessions()
            .get(&session_id)
            .unwrap()
            .unwrap();
        let worker = round_trip.tree.get(&team_id).unwrap();
        assert_eq!(worker.lifecycle, Lifecycle::Orphaned);
        assert_eq!(
            worker.turn,
            Some(Turn::AwaitingUser {
                reason: AwaitingReason::Permission
            })
        );
        assert!(worker.interaction_pending);
        assert_eq!(worker.preview_visibility, PreviewVisibility::Hide);
        let preview = worker.activity_preview.as_ref().unwrap();
        assert_eq!(preview.node_id, team_id);
        assert_eq!(preview.normalized_text, "waiting to run cargo test");

        let info = worker.agent.as_ref().unwrap();
        assert!(info.name.user_renamed);
        assert_eq!(info.name.display_name, "My reviewer");
        assert_eq!(info.agent_type.as_deref(), Some("code-reviewer"));
        assert_eq!(
            info.current_task.as_deref(),
            Some("Review the durable migration")
        );
        assert_eq!(info.agent.provider.as_deref(), Some("anthropic"));
        assert_eq!(info.agent.tool.as_deref(), Some("claude-code"));
        assert_eq!(info.agent.model.as_deref(), Some("claude-opus-4-1"));
        assert_eq!(
            info.last_message.as_deref(),
            Some("I need permission before the test")
        );
        assert_eq!(
            info.pending_permission
                .as_ref()
                .map(|pending| pending.summary.as_str()),
            Some("Run the focused test")
        );
        assert_eq!(
            info.pending_question.as_deref(),
            Some("Should I include ignored tests?")
        );
        assert_eq!(info.tokens_used, Some(12_345));
        assert_eq!(info.cost_usd, Some(0.42));
        assert_eq!(info.permission_mode.as_deref(), Some("default"));
        assert_eq!(info.git_branch.as_deref(), Some("feature/durable-repair"));
        assert!(info.resumable);
        assert!(round_trip.tree.get(&lifecycle_id).is_none());
    }

    #[tokio::test]
    async fn restore_lifecycle_survivor_rekeys_team_preview_and_metadata() {
        let mut harness = Harness::new().await;
        let session_id = SessionId::from_stored("sess_restore_protected_lifecycle_preview");
        harness.add_session(
            session_id.clone(),
            PaneId::from_stored("pane_restore_protected_lifecycle_preview"),
            NOW,
        );
        let mut parent = ProcessNode::agent(session_id.clone(), "claude", "/tmp", NOW);
        let parent_id = parent.id.clone();
        parent.lifecycle = Lifecycle::Alive;

        let mut team = ProcessNode::agent(session_id.clone(), "claude", "/tmp", NOW + 1);
        team.kind = NodeKind::Subagent;
        team.title = "reviewer".into();
        team.lifecycle = Lifecycle::Alive;
        team.interaction_pending = true;
        team.preview_visibility = PreviewVisibility::Hide;
        team.link_to(parent_id.clone(), Relation::Confirmed);
        let team_id = team.id.clone();
        {
            let info = team.agent.as_mut().unwrap();
            info.external_id = Some("reviewer@session-legacy".into());
            info.agent.external_id = info.external_id.clone();
            info.agent.provider = Some("anthropic".into());
            info.agent.tool = Some("claude-code".into());
            info.agent.model = Some("claude-sonnet-4".into());
            info.name = AgentName::declared("reviewer");
            info.name.rename("Trusted reviewer");
            info.agent_type = Some("code-reviewer".into());
            info.current_task = Some("Inspect the persisted state".into());
            info.pending_permission = Some(PendingPermission {
                summary: "Read the crash artifact".into(),
                command: Some("open crash.ips".into()),
                tool_name: Some("Bash".into()),
                risk: Risk::Low,
                requested_ms: NOW + 2,
                cwd: Some("/tmp".into()),
            });
            info.pending_question = Some("Keep the diagnostic artifact?".into());
            info.resumable = true;
        }
        team.activity_preview = Some(ActivityPreview {
            node_id: team_id.clone(),
            raw_source_sequence: Some(17),
            normalized_text: "reviewed the durable history".into(),
            source: PreviewSource::SemanticEvent,
            confidence: Confidence::Explicit,
            stable: true,
            contains_sensitive_data: false,
            redacted: false,
            updated_ms: NOW + 2,
        });

        let mut lifecycle = ProcessNode::agent(session_id.clone(), "claude", "/tmp", NOW + 2);
        lifecycle.kind = NodeKind::Subagent;
        lifecycle.title = "reviewer".into();
        lifecycle.lifecycle = Lifecycle::Alive;
        lifecycle.turn = Some(Turn::AwaitingUser {
            reason: AwaitingReason::Question,
        });
        lifecycle.link_to(parent_id, Relation::Confirmed);
        let lifecycle_id = lifecycle.id.clone();
        {
            let info = lifecycle.agent.as_mut().unwrap();
            info.external_id = Some("areviewer-deadbeef".into());
            info.agent.external_id = info.external_id.clone();
            info.name = AgentName {
                declared_name: None,
                display_name: "reviewer".into(),
                source: NameSource::Integration,
                confidence: Confidence::Integrated,
                user_renamed: false,
            };
            info.agent_type = Some("reviewer".into());
        }

        let session = harness.core.sessions.get_mut(&session_id).unwrap();
        session.tree.insert(parent);
        session.tree.insert(team);
        session.tree.insert(lifecycle);
        harness.core.persist_session(&session_id).unwrap();

        let mut restored = harness
            .core
            .store
            .sessions()
            .load_for_restore(&session_id)
            .unwrap()
            .unwrap();
        let protected = HashSet::from([lifecycle_id.clone()]);
        let repairs = repair_legacy_claude_subagent_aliases(&mut restored, &protected);
        assert_eq!(repairs, [(team_id.clone(), lifecycle_id.clone())]);
        assert!(restored.tree.get(&team_id).is_none());
        assert_eq!(
            restored
                .tree
                .get(&lifecycle_id)
                .and_then(|node| node.activity_preview.as_ref())
                .map(|preview| &preview.node_id),
            Some(&lifecycle_id)
        );

        harness
            .core
            .store
            .sessions()
            .save_after_node_remaps(&restored, &repairs)
            .unwrap();
        let round_trip = harness
            .core
            .store
            .sessions()
            .get(&session_id)
            .unwrap()
            .unwrap();
        let preview = round_trip
            .tree
            .get(&lifecycle_id)
            .and_then(|node| node.activity_preview.as_ref())
            .expect("the protected lifecycle identity keeps the semantic preview");
        assert_eq!(preview.node_id, lifecycle_id);
        let worker = round_trip.tree.get(&lifecycle_id).unwrap();
        assert_eq!(worker.lifecycle, Lifecycle::Orphaned);
        assert_eq!(
            worker.turn,
            Some(Turn::AwaitingUser {
                reason: AwaitingReason::Question
            })
        );
        assert!(worker.interaction_pending);
        assert_eq!(worker.preview_visibility, PreviewVisibility::Hide);
        let info = worker.agent.as_ref().unwrap();
        assert!(info.name.user_renamed);
        assert_eq!(info.name.display_name, "Trusted reviewer");
        assert_eq!(info.agent_type.as_deref(), Some("code-reviewer"));
        assert_eq!(
            info.current_task.as_deref(),
            Some("Inspect the persisted state")
        );
        assert_eq!(info.agent.provider.as_deref(), Some("anthropic"));
        assert_eq!(info.agent.tool.as_deref(), Some("claude-code"));
        assert_eq!(info.agent.model.as_deref(), Some("claude-sonnet-4"));
        assert_eq!(
            info.pending_permission
                .as_ref()
                .map(|pending| pending.summary.as_str()),
            Some("Read the crash artifact")
        );
        assert_eq!(
            info.pending_question.as_deref(),
            Some("Keep the diagnostic artifact?")
        );
        assert!(info.resumable);
        assert!(round_trip.tree.get(&team_id).is_none());
        assert_eq!(
            harness
                .core
                .store
                .hierarchy()
                .preview_history(&preview.node_id, 20)
                .unwrap()
                .len(),
            1
        );
        assert!(harness
            .core
            .store
            .hierarchy()
            .preview_history(&team_id, 20)
            .unwrap()
            .is_empty());
    }

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
