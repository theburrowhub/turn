//! Noticing what the processes we started went on to start.
//!
//! This is the fallback tier for hierarchy, and it is deliberately lazy. Refreshing
//! the whole process table is not free, and doing it on a timer across thirty sessions
//! is the aggressive polling the product is meant to avoid. So a sweep happens when
//! something suggests the tree changed — a process started or ended, or an agent
//! finished a turn while leaving work running — with a floor on how often, and a slow
//! safety net for the case where a child appeared with nothing to announce it.
//!
//! Everything a sweep finds is [`Relation::Inferred`]. A pid whose ppid happens to
//! match is not the same claim as a tool reporting what it spawned, and the UI draws
//! the difference.

use super::Core;
use turn_core::event::{Confidence, EventKind, EventSource, TurnEvent};
use turn_core::ids::{NodeId, SessionId};
use turn_core::model::{ProcessNode, Relation};
use turn_core::state::Lifecycle;
use turn_pty::ObservedProcess;

/// Whether an observed process looks like the program a command names.
///
/// Both fields are consulted because an agent CLI is routinely a script: `claude` is
/// run by `node`, so the process name is the interpreter and the executable's own name
/// only appears in the command line. This is the same test the restore path applies to
/// a surviving pid, for the same reason.
fn names_command(observed: &ObservedProcess, executable: &str) -> bool {
    !executable.is_empty()
        && (observed.name.contains(executable) || observed.command_line.contains(executable))
}

/// Allowance around the launch timestamp when corroborating a hosted process.
///
/// Process tables report whole seconds and Turn records the node around the write to the
/// shell. A process outside this interval is either older than the command or a later
/// process wearing a recycled pid.
const HOSTED_START_SKEW_MS: i64 = 2_000;

/// How long a shell-hosted command may take to appear in the process table.
///
/// This is deliberately much longer than a normal shell fork because an interactive shell
/// can still be reading startup files before it consumes the command Turn queued. It is
/// still bounded: an agent that starts and exits before the first sweep must not remain
/// `Alive` forever merely because Turn never learned its pid.
pub const HOSTED_IDENTIFICATION_TIMEOUT_MS: i64 = 30_000;

/// Whether this observed process can still be the hosted node Turn launched.
pub(super) fn corroborates_hosted_process(node: &ProcessNode, observed: &ObservedProcess) -> bool {
    let wanted = crate::core::spawn::executable_name(&node.command);
    if !names_command(observed, wanted) {
        return false;
    }
    observed.start_time_ms.is_none_or(|started_ms| {
        started_ms + HOSTED_START_SKEW_MS >= node.started_ms
            && started_ms
                <= node
                    .started_ms
                    .saturating_add(HOSTED_IDENTIFICATION_TIMEOUT_MS)
                    .saturating_add(HOSTED_START_SKEW_MS)
    })
}

fn hosted_identification_expired(started_ms: i64, now_ms: i64) -> bool {
    now_ms.saturating_sub(started_ms) >= HOSTED_IDENTIFICATION_TIMEOUT_MS
}

/// Shortest gap between two sweeps, however much has happened.
pub const SWEEP_MIN_INTERVAL_MS: i64 = 2_000;

/// How long after something changed the sweep waits before looking.
///
/// A process that has just started has not started anything yet, so sweeping the instant
/// it spawns is guaranteed to find nothing — and would then clear the request that would
/// have found its children a moment later.
pub const SWEEP_DELAY_MS: i64 = 600;

/// How long the daemon will go without sweeping while it owns a running process.
///
/// The safety net for children nothing announced: a dev server an agent started
/// through a shell reports to nobody, and thirty seconds late in the tree is better
/// than a process table refresh every second forever.
pub const SWEEP_IDLE_INTERVAL_MS: i64 = 30_000;

/// Most inferred children Turn will attach to one node.
///
/// A `make -j` fans out to a process per core and a watcher restarts its child every
/// save. Past a couple of dozen the tree stops being something a person reads, so the
/// cap is a readability decision rather than a performance one.
pub const MAX_INFERRED_CHILDREN: usize = 24;

impl Core {
    /// Asks for a sweep shortly after something changed the tree.
    pub(crate) fn request_sweep(&mut self, now_ms: i64) {
        let at = now_ms + SWEEP_DELAY_MS;
        // The earliest request wins: two spawns in quick succession should not keep
        // pushing the look-back out.
        self.sweep_due_ms = Some(self.sweep_due_ms.map_or(at, |existing| existing.min(at)));
    }

    /// Sweeps the process table if it is worth doing.
    pub(crate) fn maybe_sweep(&mut self, now_ms: i64) {
        let owns_running = self
            .processes
            .values()
            .any(|process| process.pty.is_running());
        if !owns_running {
            // Nothing of ours is running, so nothing of ours can have children.
            self.sweep_due_ms = None;
            return;
        }
        let since = now_ms.saturating_sub(self.last_sweep_ms);
        let due = self.sweep_due_ms.is_some_and(|due| now_ms >= due);
        if !(due && since >= SWEEP_MIN_INTERVAL_MS) && since < SWEEP_IDLE_INTERVAL_MS {
            return;
        }
        // Cleared only once the moment it asked for has arrived. A sweep that happened to
        // run earlier — on the slow safety net, or for another node — is too early to
        // answer this request, and discarding it would leave the child it was for
        // unnoticed until the next thirty-second sweep.
        if due {
            self.sweep_due_ms = None;
        }
        self.sweep(now_ms);
    }

    /// Looks for children of the processes we own, and for inferred children that have
    /// gone away.
    pub(crate) fn sweep(&mut self, now_ms: i64) {
        self.last_sweep_ms = now_ms;
        self.supervisor.refresh();

        let roots: Vec<(SessionId, NodeId, u32)> = self
            .processes
            .iter()
            .filter(|(_, process)| process.pty.is_running())
            .map(|(node, process)| (process.session_id.clone(), node.clone(), process.pty.pid()))
            .collect();

        // Identification before adoption, and the order matters: the agent Turn started
        // in a pane's shell is already in the tree with a confirmed edge, and letting
        // the generic pass reach it first would both duplicate the node and file the
        // edge as a guess.
        for (session_id, node_id, pid) in &roots {
            self.identify_hosted_process(session_id, node_id, *pid, now_ms);
        }
        for (session_id, node_id, pid) in roots {
            self.adopt_children(&session_id, &node_id, pid, now_ms);
        }
        self.retire_vanished_children(now_ms);
    }

    /// Learns the pid of the command Turn started inside one of its shells.
    ///
    /// This is identification, not inference. The node, its parent and its adapter were
    /// all decided when Turn wrote the command line; the only thing missing is which
    /// row of the process table is that command, because the shell forked it and Turn
    /// never held it. Matching a direct child of the shell against the executable Turn
    /// asked for answers that — and if nothing matches, nothing is claimed: the node
    /// keeps no pid rather than borrowing one from whatever else is running there.
    fn identify_hosted_process(
        &mut self,
        session_id: &SessionId,
        shell: &NodeId,
        shell_pid: u32,
        now_ms: i64,
    ) {
        let Some(hosted) = self.processes.get(shell).and_then(|p| p.hosted.clone()) else {
            return;
        };
        let Some(session) = self.sessions.get(session_id) else {
            return;
        };
        let Some(node) = session.tree.get(&hosted) else {
            return;
        };
        if node.pid.is_some() || !node.is_running() {
            return;
        }
        let wanted = crate::core::spawn::executable_name(&node.command).to_string();
        let started_ms = node.started_ms;
        let Some(found) = self
            .supervisor
            .children(shell_pid)
            .into_iter()
            .filter(|observed| corroborates_hosted_process(node, observed))
            .min_by_key(|observed| {
                (
                    observed
                        .start_time_ms
                        .map_or(i64::MAX, |started| started.abs_diff(started_ms) as i64),
                    observed.pid,
                )
            })
        else {
            if hosted_identification_expired(started_ms, now_ms) {
                tracing::warn!(
                    %session_id, %hosted, executable = %wanted,
                    "the hosted command never appeared in the process table"
                );
                self.publish_hosted_loss(session_id, &hosted, now_ms);
            } else {
                // Keep looking until the bounded identification window closes. Without
                // this follow-up, one early sweep that raced a short-lived process was the
                // only sweep and its node remained Alive indefinitely.
                self.request_sweep(now_ms);
            }
            return;
        };

        if let Some(session) = self.sessions.get_mut(session_id) {
            if let Some(node) = session.tree.get_mut(&hosted) {
                node.pid = Some(found.pid);
                node.ppid = found.ppid;
            }
        }
        tracing::debug!(
            %session_id, %hosted, pid = found.pid, ppid = ?found.ppid, executable = %wanted,
            "identified the process Turn started in a pane's shell"
        );
        self.persist_session_quietly(session_id);
        self.push_tree(session_id, now_ms);
    }

    /// Publishes a hosted loss through all views that carry lifecycle state.
    pub(crate) fn publish_hosted_loss(
        &mut self,
        session_id: &SessionId,
        hosted: &NodeId,
        now_ms: i64,
    ) {
        self.record_hosted_loss(session_id, hosted, now_ms);
        self.persist_session_quietly(session_id);
        self.push_tree(session_id, now_ms);
        self.push_node_state(session_id, hosted, None, now_ms);
        self.push_session_state(session_id, now_ms);
    }

    /// Adds children of one node that are not in the tree yet.
    fn adopt_children(
        &mut self,
        session_id: &SessionId,
        root: &NodeId,
        root_pid: u32,
        now_ms: i64,
    ) {
        let observed = self.supervisor.descendants(root_pid);
        if observed.is_empty() {
            return;
        }

        let Some(session) = self.sessions.get(session_id) else {
            return;
        };
        let existing_children = session
            .tree
            .descendants(root)
            .into_iter()
            .filter(|node| node.relation == Relation::Inferred)
            .count();
        let mut room = MAX_INFERRED_CHILDREN.saturating_sub(existing_children);

        for process in observed {
            if room == 0 {
                break;
            }
            let Some(session) = self.sessions.get(session_id) else {
                break;
            };
            if session.tree.find_by_pid(process.pid).is_some() {
                continue;
            }
            // Attach to the tracked process that actually is its parent when we have
            // one; otherwise to the node the sweep started from. Either way the edge
            // is a guess and says so.
            let parent = session
                .tree
                .iter()
                .find(|node| process.ppid.is_some() && node.pid == process.ppid)
                .map(|node| node.id.clone())
                .unwrap_or_else(|| root.clone());

            let command = if process.command_line.is_empty() {
                process.name.clone()
            } else {
                process.command_line.clone()
            };
            let event = TurnEvent::new(
                session_id.clone(),
                EventKind::ProcessSpawnedChild {
                    child: NodeId::new(),
                    pid: process.pid,
                    ppid: process.ppid,
                    command,
                    args: process.args,
                    cwd: process.cwd,
                    // Never confirmed here. Only a tool reporting what it started
                    // earns that, and the process table is not a tool.
                    confirmed_parent: false,
                },
                EventSource::Supervisor,
                Confidence::Explicit,
                now_ms,
            )
            .with_node(parent);
            // The supervisor yields a parent before its descendants. Applying
            // immediately lets the next row attach to that newly inserted parent;
            // batching every event first flattened grandchildren under the root.
            self.ingest(event, now_ms);
            room -= 1;
        }
    }

    /// Marks children that are no longer in the process table.
    ///
    /// They become [`Lifecycle::Lost`] rather than exited: Turn never held these
    /// processes and did not see them end, so it has no exit code to report and will
    /// not make one up. `Lost` is terminal without being a failure, which is the right
    /// reading for an agent the user quit with `/exit` as much as for a dev server that
    /// vanished — Turn watched neither of them die.
    ///
    /// Both kinds of pid Turn does not hold are covered: the children a sweep inferred,
    /// and the agent Turn started in a pane's shell. Leaving the second out would make
    /// an agent that quit go on claiming to be running for as long as its shell lived,
    /// which is exactly the pane the user is looking at.
    fn retire_vanished_children(&mut self, now_ms: i64) {
        let hosted: Vec<NodeId> = self
            .processes
            .values()
            .filter_map(|process| process.hosted.clone())
            .collect();
        let mut gone: Vec<(SessionId, NodeId, Option<NodeId>, Option<String>)> = Vec::new();
        for session in self.sessions.values() {
            for node in session.tree.iter() {
                let ours = node.relation == Relation::Inferred || hosted.contains(&node.id);
                if !ours || !node.is_running() {
                    continue;
                }
                let Some(pid) = node.pid else { continue };
                if !self.supervisor.is_alive(pid) {
                    gone.push((
                        session.id.clone(),
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
        }

        let mut touched: Vec<SessionId> = Vec::new();
        let mut changed: Vec<(SessionId, NodeId)> = Vec::new();
        for (session_id, node_id, parent_id, external_id) in gone {
            let retired_root = if let Some(session) = self.sessions.get_mut(&session_id) {
                if let Some(node) = session
                    .tree
                    .get_mut(&node_id)
                    .filter(|node| node.is_running())
                {
                    node.lifecycle = Lifecycle::Lost;
                    node.ended_ms = Some(now_ms);
                    super::events::clear_interaction_state(node);
                    true
                } else {
                    false
                }
            } else {
                false
            };
            if !retired_root {
                continue;
            }
            let mut retired = vec![(node_id.clone(), parent_id, external_id)];
            retired.extend(self.mark_runtime_dependents(&session_id, &node_id, now_ms));
            self.resolve_lifecycle_attention(&session_id, &retired, now_ms);
            // Each retired node's own state, not only the tree it sits in. A node going
            // from alive to lost is the transition a client is waiting on to stop drawing
            // it as running, and the tree push does not carry which node changed.
            // `caused_by` stays empty: nothing here is a reason to move anybody.
            for (node, _, _) in &retired {
                changed.push((session_id.clone(), node.clone()));
            }
            if !touched.contains(&session_id) {
                touched.push(session_id);
            }
        }
        for session_id in touched {
            self.persist_session_quietly(&session_id);
            self.push_tree(&session_id, now_ms);
            self.push_session_state(&session_id, now_ms);
        }
        for (session_id, node_id) in changed {
            self.push_node_state(&session_id, &node_id, None, now_ms);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::testing::Harness;
    use turn_core::event::{Confidence, EventSource, Risk};
    use turn_core::ids::PaneId;
    use turn_core::model::{NodeKind, ProcessNode};
    use turn_core::state::Turn;

    const NOW: i64 = 1_775_000_000_000;

    fn observed(command: &str, started_ms: Option<i64>) -> ObservedProcess {
        ObservedProcess {
            pid: 42,
            ppid: Some(7),
            name: command.to_string(),
            command_line: command.to_string(),
            args: vec![command.to_string()],
            cwd: Some("/tmp".into()),
            start_time_ms: started_ms,
            kind: NodeKind::Agent,
        }
    }

    #[test]
    fn a_hosted_pid_is_corroborated_by_command_and_launch_time() {
        let node = ProcessNode::agent(SessionId::new(), "claude", "/tmp", NOW);
        assert!(corroborates_hosted_process(
            &node,
            &observed("node /usr/local/bin/claude", Some(NOW + 1_000))
        ));
        assert!(!corroborates_hosted_process(
            &node,
            &observed("unrelated", Some(NOW + 1_000))
        ));
        assert!(!corroborates_hosted_process(
            &node,
            &observed(
                "claude",
                Some(NOW + HOSTED_IDENTIFICATION_TIMEOUT_MS + 3_000)
            )
        ));
    }

    #[test]
    fn an_unidentified_hosted_process_gets_a_bounded_grace_period() {
        assert!(!hosted_identification_expired(
            NOW,
            NOW + HOSTED_IDENTIFICATION_TIMEOUT_MS - 1
        ));
        assert!(hosted_identification_expired(
            NOW,
            NOW + HOSTED_IDENTIFICATION_TIMEOUT_MS
        ));
    }

    #[tokio::test]
    async fn a_vanished_inferred_runtime_retires_virtual_descendants_and_their_attention() {
        let mut harness = Harness::new().await;
        let session_id = SessionId::from_stored("sess_vanished_runtime_tree");
        harness.add_session(session_id.clone(), PaneId::new(), NOW);

        let mut root = ProcessNode::agent(session_id.clone(), "claude", "/tmp", NOW);
        root.lifecycle = Lifecycle::Alive;
        let root_id = root.id.clone();
        let mut runtime = ProcessNode::process(
            session_id.clone(),
            NodeKind::Background,
            "worker-runtime",
            "/tmp",
            NOW,
        );
        runtime.lifecycle = Lifecycle::Alive;
        runtime.pid = Some(u32::MAX);
        runtime.link_to(root_id.clone(), Relation::Inferred);
        let runtime_id = runtime.id.clone();
        let mut reviewer = ProcessNode::agent(session_id.clone(), "reviewer", "/tmp", NOW);
        reviewer.kind = NodeKind::Subagent;
        reviewer.lifecycle = Lifecycle::Alive;
        reviewer.turn = Some(Turn::Active);
        reviewer.link_to(runtime_id.clone(), Relation::Confirmed);
        let reviewer_id = reviewer.id.clone();
        let tree = &mut harness.core.sessions.get_mut(&session_id).unwrap().tree;
        tree.insert(root);
        tree.insert(runtime);
        tree.insert(reviewer);

        let permission = TurnEvent::new(
            session_id.clone(),
            EventKind::AgentPermissionRequired {
                summary: "Reviewer needs permission".into(),
                command: Some("cargo test".into()),
                tool_name: Some("Bash".into()),
                risk: Risk::Low,
            },
            EventSource::Hook {
                tool: "claude-code".into(),
                event_name: "Notification".into(),
            },
            Confidence::Explicit,
            NOW + 1,
        )
        .with_node(reviewer_id.clone());
        harness.core.ingest(permission, NOW + 1);
        assert_eq!(harness.core.attention.queue().len(), 1);

        harness.core.retire_vanished_children(NOW + 2);

        let session = &harness.core.sessions[&session_id];
        assert_eq!(
            session.tree.get(&runtime_id).unwrap().lifecycle,
            Lifecycle::Lost
        );
        let reviewer = session.tree.get(&reviewer_id).unwrap();
        assert_eq!(reviewer.lifecycle, Lifecycle::Lost);
        assert!(!reviewer.interaction_pending);
        assert!(!reviewer.turn.as_ref().is_some_and(Turn::needs_user));
        assert!(reviewer.agent.as_ref().is_some_and(|agent| {
            agent.pending_permission.is_none() && agent.pending_question.is_none()
        }));
        assert!(harness.core.attention.queue().is_empty());
        assert!(harness.core.store.attention().list().unwrap().is_empty());

        let persisted = harness
            .core
            .store
            .sessions()
            .get(&session_id)
            .unwrap()
            .unwrap();
        let reviewer = persisted.tree.get(&reviewer_id).unwrap();
        assert_eq!(reviewer.lifecycle, Lifecycle::Lost);
        assert!(!reviewer.interaction_pending);
        assert!(reviewer.agent.as_ref().is_some_and(|agent| {
            agent.pending_permission.is_none() && agent.pending_question.is_none()
        }));
    }
}
