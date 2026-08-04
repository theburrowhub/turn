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
use turn_core::model::Relation;
use turn_core::state::Lifecycle;

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

        for (session_id, node_id, pid) in roots {
            self.adopt_children(&session_id, &node_id, pid, now_ms);
        }
        self.retire_vanished_children(now_ms);
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

        let mut events = Vec::new();
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
            events.push(
                TurnEvent::new(
                    session_id.clone(),
                    EventKind::ProcessSpawnedChild {
                        child: NodeId::new(),
                        pid: process.pid,
                        command,
                        // Never confirmed here. Only a tool reporting what it started
                        // earns that, and the process table is not a tool.
                        confirmed_parent: false,
                    },
                    EventSource::Supervisor,
                    Confidence::Explicit,
                    now_ms,
                )
                .with_node(parent),
            );
            room -= 1;
        }

        for event in events {
            self.ingest(event, now_ms);
        }
    }

    /// Marks inferred children that are no longer in the process table.
    ///
    /// They become [`Lifecycle::Lost`] rather than exited: Turn never held these
    /// processes and did not see them end, so it has no exit code to report and will
    /// not make one up.
    fn retire_vanished_children(&mut self, now_ms: i64) {
        let mut gone: Vec<(SessionId, NodeId)> = Vec::new();
        for session in self.sessions.values() {
            for node in session.tree.iter() {
                if node.relation != Relation::Inferred || !node.is_running() {
                    continue;
                }
                let Some(pid) = node.pid else { continue };
                if !self.supervisor.is_alive(pid) {
                    gone.push((session.id.clone(), node.id.clone()));
                }
            }
        }

        let mut touched: Vec<SessionId> = Vec::new();
        for (session_id, node_id) in gone {
            if let Some(session) = self.sessions.get_mut(&session_id) {
                if let Some(node) = session.tree.get_mut(&node_id) {
                    node.lifecycle = Lifecycle::Lost;
                    node.ended_ms = Some(now_ms);
                }
            }
            self.attention.resolve_node(&node_id);
            if !touched.contains(&session_id) {
                touched.push(session_id);
            }
        }
        for session_id in touched {
            self.persist_session_quietly(&session_id);
            self.push_tree(&session_id, now_ms);
            self.push_session_state(&session_id, now_ms);
        }
    }
}
