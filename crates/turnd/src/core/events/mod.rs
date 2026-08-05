//! Turning events into state.
//!
//! Every signal Turn receives — a Claude Code hook, a Codex `notify`, an exit status,
//! an output guess, a user correction — arrives here as a [`TurnEvent`], and this is
//! the only place that changes what a node claims to be doing. Three rules hold
//! throughout:
//!
//! * **The two axes stay separate.** A completed turn never touches the lifecycle, and
//!   an exit never leaves a stale turn behind claiming the agent is still waiting.
//! * **A guess cannot overwrite a fact.** Each node remembers how trustworthy the
//!   source of its current turn state was; a provisional event that would contradict a
//!   hook, or the user, is recorded in the log and refused as a state change.
//! * **Nothing is invented.** A subagent link is `Confirmed` because a tool reported
//!   it; a node we cannot place stays where it is rather than being attached to a
//!   plausible parent.
//!
//! This module holds the events that *change* a node. The two that do something else
//! have their own files: [`tree`] for the ones that add a node, and [`exit`] for a
//! process ending and for what that implies about children we never held.

mod exit;
mod tree;

use super::Core;
use turn_core::event::{Confidence, EventKind, TurnEvent};
use turn_core::ids::NodeId;
use turn_core::model::{NodeKind, PendingPermission};
use turn_core::state::{AwaitingReason, Lifecycle, Turn};
use turn_proto::ServerEvent;

/// What applying an event changed.
#[derive(Debug, Default)]
pub(super) struct Changed {
    /// The node whose state moved, if any.
    pub(super) node: Option<NodeId>,
    /// Whether the shape of the tree changed, so the whole tree must be re-sent.
    pub(super) structure: bool,
    /// Whether the state change was refused for want of authority.
    pub(super) refused: bool,
}

impl Core {
    /// Applies one event: state, store, attention, pushes.
    pub(crate) fn ingest(&mut self, mut event: TurnEvent, now_ms: i64) {
        let session_id = event.session_id.clone();
        let Some(session) = self.sessions.get(&session_id) else {
            // A hook from a session that has since gone. Recording it against nothing
            // would violate the store's foreign keys, and there is no state to change.
            tracing::debug!(
                session = %session_id,
                kind = turn_core::event::event_name(&event.kind),
                "dropped an event for an unknown session"
            );
            return;
        };
        if event.workspace_id.is_none() {
            event.workspace_id = Some(session.workspace_id.clone());
        }
        if let Some(external_id) = event.agent.external_id.as_deref() {
            if let Some(subject) = session.tree.find_by_external_id(external_id) {
                let belongs_to_hook_parent = event.parent_node_id.as_ref().is_none_or(|parent| {
                    subject.id == *parent
                        || session
                            .tree
                            .descendants(parent)
                            .into_iter()
                            .any(|descendant| descendant.id == subject.id)
                });
                // Hook connections belong to the main runtime process, but a
                // worker-aware payload names its own Agent. Resolve that identity
                // before state, preview and Attention handling so Reviewer can be
                // YOUR TURN without pretending it owns a separate PTY.
                if belongs_to_hook_parent {
                    event.node_id = Some(subject.id.clone());
                    event.dedup_key = format!("{}|subject:{}", event.dedup_key, subject.id);
                }
            }
        }
        self.correlate_unbound_agent_event(&mut event);
        let policy = session.attention.clone();

        let changed = self.apply(&event, now_ms);
        let preview_changed = self.update_preview_from_event(&event, changed.node.as_ref(), now_ms);
        if let Some(session) = self.sessions.get_mut(&session_id) {
            session.touch(now_ms);
        }
        self.persist_event(&event);
        self.persist_session_quietly(&session_id);

        // An event too weak to change the state is also too weak to demand attention.
        // Letting a refused guess into the queue would produce the worst of both: the
        // node saying one thing and the sidebar asking about another. It is still
        // recorded and still pushed, so the user can see what the heuristic thought.
        let effects = if changed.refused {
            Vec::new()
        } else {
            self.attention
                .ingest(&event, &policy, &self.user.clone(), now_ms)
        };

        self.push_all(ServerEvent::TurnEventEmitted {
            turn_event: event.clone(),
        });
        if changed.structure {
            self.push_tree(&session_id, now_ms);
        } else if preview_changed {
            if let Some(node) = &changed.node {
                self.push_activity_preview(&session_id, node, now_ms);
            }
        }
        if let Some(node) = &changed.node {
            self.push_node_state(&session_id, node, Some(event.clone()), now_ms);
        }
        self.push_session_state(&session_id, now_ms);
        self.emit_effects(effects, now_ms);

        if changed.refused {
            tracing::debug!(
                session = %session_id,
                kind = turn_core::event::event_name(&event.kind),
                confidence = event.confidence.label(),
                "kept the existing state: the event was less trustworthy than what set it"
            );
        }
    }

    /// Resolves callbacks delivered through a parent's hook endpoint but lacking
    /// a worker id.
    ///
    /// Claude explicitly reported the event, but not its subject. A single live
    /// child is a high-confidence correlation; several children remain node-less
    /// and provisional. That distinction prevents both failure modes: silently
    /// marking the parent `YOUR TURN`, and confidently blaming an arbitrary
    /// sibling.
    fn correlate_unbound_agent_event(&self, event: &mut TurnEvent) {
        if event.node_id.is_some() {
            return;
        }
        let Some(parent) = event.parent_node_id.clone() else {
            return;
        };
        let Some(session) = self.sessions.get(&event.session_id) else {
            return;
        };

        // An explicit tool-owned identity is stronger evidence than the shape of
        // today's tree, even when that identity has not been declared yet. In
        // particular, an out-of-order callback must not be assigned to the one
        // *different* child that happens to exist. Keep the authenticated parent
        // and external id as a durable correlation scope instead.
        if let Some(external_id) = event.agent.external_id.clone() {
            preserve_unresolved_external_subject(event, &parent, &external_id);
            return;
        }

        match &event.kind {
            EventKind::AgentPermissionRequired { .. } | EventKind::AgentWaitingForUser { .. } => {
                let candidates: Vec<_> = session
                    .tree
                    .descendants(&parent)
                    .into_iter()
                    .filter(|node| node.kind == NodeKind::Subagent && node.is_running())
                    .map(|node| node.id.clone())
                    .collect();
                match candidates.as_slice() {
                    [subject] => bind_inferred_subject(event, subject),
                    _ => preserve_unresolved_subject(event, &parent),
                }
            }
            EventKind::AgentTurnStarted { .. } => {
                // An earlier ambiguous worker demand is itself the best available
                // correlation target. Resolve that provisional flow, not a known
                // sibling which may still be waiting for an unrelated answer.
                let has_unassigned =
                    self.attention
                        .queue()
                        .has_unresolved_scope(&event.session_id, &parent, None);
                if has_unassigned {
                    preserve_unresolved_subject(event, &parent);
                    return;
                }

                let waiting: Vec<_> = std::iter::once(session.tree.get(&parent))
                    .flatten()
                    .chain(session.tree.descendants(&parent))
                    .filter(|node| {
                        node.kind.is_agentic()
                            && node.is_running()
                            && (node.interaction_pending
                                || node.turn.as_ref().is_some_and(|turn| turn.needs_user()))
                    })
                    .map(|node| node.id.clone())
                    .collect();

                match waiting.as_slice() {
                    [subject] if subject == &parent => bind_explicit_subject(event, subject),
                    [subject] => bind_inferred_subject(event, subject),
                    [] => {
                        // With no outstanding child flow, UserPromptSubmit belongs
                        // to the runtime whose authenticated hook endpoint received
                        // it: the parent node itself.
                        bind_explicit_subject(event, &parent);
                    }
                    _ => preserve_unresolved_subject(event, &parent),
                }
            }
            _ => {}
        }
    }

    /// Applies an event to the session's process tree.
    fn apply(&mut self, event: &TurnEvent, now_ms: i64) -> Changed {
        let session_id = event.session_id.clone();
        let Some(node_id) = event.node_id.clone() else {
            return Changed::default();
        };

        // Subagents are the one case that creates a node rather than changing one.
        if let EventKind::AgentSpawned {
            declared_name,
            agent_type,
            agent_id,
            task,
        } = &event.kind
        {
            return self.insert_subagent(
                &session_id,
                &node_id,
                declared_name.clone(),
                agent_type.clone(),
                agent_id.clone(),
                task.clone(),
                now_ms,
            );
        }
        if let EventKind::ProcessSpawnedChild {
            child,
            pid,
            ppid,
            command,
            cwd,
            confirmed_parent,
        } = &event.kind
        {
            return self.insert_child(
                &session_id,
                &node_id,
                child.clone(),
                *pid,
                *ppid,
                command.clone(),
                cwd.clone(),
                *confirmed_parent,
                now_ms,
            );
        }
        if let EventKind::AgentSubagentStopped { agent_id } = &event.kind {
            return self.stop_subagent(&session_id, &node_id, agent_id.as_deref(), now_ms);
        }

        let touches_turn = turn_axis_change(&event.kind).is_some();
        if touches_turn && !self.may_set_turn_for_event(event, &node_id) {
            return Changed {
                node: None,
                structure: false,
                refused: true,
            };
        }

        let Some(session) = self.sessions.get_mut(&session_id) else {
            return Changed::default();
        };
        let Some(node) = session.tree.get_mut(&node_id) else {
            // The event outlived its node. The log keeps it; there is nothing to move.
            return Changed::default();
        };

        match &event.kind {
            EventKind::ProcessStarted { pid, command } => {
                node.pid = Some(*pid);
                node.command = command.clone();
                if !node.lifecycle.is_terminal() {
                    node.lifecycle = Lifecycle::Alive;
                }
            }
            EventKind::ProcessExited { code } => {
                // Left alone when it is already terminal: `node_exited` records what
                // `turn-pty` observed, including the name of the signal that killed
                // it, and an exit code of 1 would lose that.
                if !node.lifecycle.is_terminal() {
                    node.lifecycle = Lifecycle::Exited { code: *code };
                }
                node.exit_code = Some(*code);
                node.ended_ms = Some(now_ms);
                node.interaction_pending = false;
                // A dead process is not waiting for anybody. Clearing the turn axis
                // here is what stops a crashed agent sitting in the sidebar for the
                // rest of the day claiming it is your turn.
                if let Some(turn) = node.turn.as_mut() {
                    if turn.needs_user() {
                        *turn = Turn::Unknown;
                    }
                }
                if let Some(agent) = node.agent.as_mut() {
                    agent.pending_permission = None;
                    agent.pending_question = None;
                }
            }
            EventKind::ProcessFailed { code, .. } => {
                if !node.lifecycle.is_terminal() {
                    node.lifecycle = match code {
                        Some(code) => Lifecycle::Exited { code: *code },
                        None => Lifecycle::Signaled {
                            signal: "unknown".to_string(),
                        },
                    };
                }
                // Only when the event carries one. A signal death has no meaningful
                // status to report, so the event carries `None` — and assigning that
                // would erase the code `node_exited` recorded from what the platform
                // actually said.
                if let Some(code) = code {
                    node.exit_code = Some(*code);
                }
                node.ended_ms = Some(now_ms);
                node.interaction_pending = false;
                if let Some(turn) = node.turn.as_mut() {
                    if turn.needs_user() {
                        *turn = Turn::Unknown;
                    }
                }
            }

            EventKind::AgentStarted {
                tool,
                model,
                external_id,
            } => {
                node.turn = Some(Turn::Idle);
                let agent = node.agent.get_or_insert_with(Default::default);
                agent.agent.tool = Some(tool.clone());
                if model.is_some() {
                    agent.agent.model = model.clone();
                }
                // The tool's own session id, which is what a resume needs and what a
                // later hook callback identifies itself by.
                if external_id.is_some() {
                    agent.external_id = external_id.clone();
                }
            }
            EventKind::AgentTurnStarted { prompt_excerpt } => {
                node.turn = Some(Turn::Active);
                node.interaction_pending = false;
                if let Some(agent) = node.agent.as_mut() {
                    agent.current_task = prompt_excerpt.clone();
                    agent.pending_permission = None;
                    agent.pending_question = None;
                }
            }
            EventKind::AgentWaitingForUser { reason, summary } => {
                node.turn = Some(Turn::AwaitingUser { reason: *reason });
                node.interaction_pending = true;
                if let Some(agent) = node.agent.as_mut() {
                    if summary.is_some() {
                        agent.last_message = summary.clone();
                    }
                }
            }
            EventKind::AgentQuestionAsked { question } => {
                node.turn = Some(Turn::AwaitingUser {
                    reason: AwaitingReason::Question,
                });
                node.interaction_pending = true;
                if let Some(agent) = node.agent.as_mut() {
                    agent.pending_question = Some(question.clone());
                }
            }
            EventKind::AgentPermissionRequired {
                summary,
                command,
                tool_name,
                risk,
            } => {
                node.turn = Some(Turn::AwaitingUser {
                    reason: AwaitingReason::Permission,
                });
                node.interaction_pending = true;
                let cwd = node.cwd.clone();
                if let Some(agent) = node.agent.as_mut() {
                    // The directory travels with the request. Approving something in
                    // the wrong repository is the mistake this field prevents, and it
                    // can only be prevented if the user is shown it.
                    agent.pending_permission = Some(PendingPermission {
                        summary: summary.clone(),
                        command: command.clone(),
                        tool_name: tool_name.clone(),
                        risk: *risk,
                        requested_ms: now_ms,
                        cwd: Some(cwd),
                    });
                }
            }
            EventKind::AgentPermissionResolved { .. } => {
                node.turn = Some(Turn::Active);
                node.interaction_pending = false;
                if let Some(agent) = node.agent.as_mut() {
                    agent.pending_permission = None;
                }
            }
            EventKind::AgentTurnCompleted {
                last_message,
                background_tasks,
            } => {
                node.turn = Some(Turn::Done);
                node.interaction_pending = false;
                if let Some(agent) = node.agent.as_mut() {
                    if last_message.is_some() {
                        agent.last_message = last_message.clone();
                    }
                    agent.pending_permission = None;
                    agent.pending_question = None;
                }
                let tasks = *background_tasks;
                self.background_tasks.insert(node_id.clone(), tasks);
                if tasks > 0 {
                    // The turn is over and the work is not. Look for those children so
                    // the tree shows them, rather than letting the session read as
                    // finished while a test run carries on.
                    self.request_sweep(now_ms);
                }
                self.turn_authority
                    .insert(node_id.clone(), event.confidence);
                return Changed {
                    node: Some(node_id),
                    structure: false,
                    refused: false,
                };
            }
            EventKind::AgentTaskCompleted { summary } => {
                node.turn = Some(Turn::TaskDone);
                node.interaction_pending = false;
                if let Some(agent) = node.agent.as_mut() {
                    if summary.is_some() {
                        agent.last_message = summary.clone();
                    }
                }
            }
            EventKind::AgentFailed { reason } => {
                node.turn = Some(Turn::Failed {
                    reason: reason.clone(),
                });
                node.interaction_pending = false;
            }
            EventKind::AgentIdle => {
                node.turn = Some(Turn::Idle);
                node.interaction_pending = false;
            }

            // Session-level events say nothing about a particular node.
            EventKind::SessionNeedsAttention { .. } | EventKind::SessionAttentionResolved => {
                return Changed::default()
            }

            // Handled above, before the mutable borrow.
            EventKind::AgentSpawned { .. }
            | EventKind::AgentSubagentStopped { .. }
            | EventKind::ProcessSpawnedChild { .. } => return Changed::default(),
        }

        if touches_turn {
            self.turn_authority
                .insert(node_id.clone(), event.confidence);
        }
        Changed {
            node: Some(node_id),
            structure: false,
            refused: false,
        }
    }

    /// Whether an event is trustworthy enough to change a node's turn state.
    ///
    /// Once a hook, a side channel or the user has established what an agent is doing,
    /// output inference may not contradict it. The reverse is allowed: a tool that
    /// starts reporting properly outranks whatever was guessed before it.
    fn may_set_turn(&self, node: &NodeId, confidence: Confidence) -> bool {
        match self.turn_authority.get(node) {
            Some(existing) => confidence >= *existing,
            None => true,
        }
    }

    /// A uniquely correlated prompt submission may close the one child flow that
    /// was waiting even when the original demand carried an explicit worker id.
    /// The resulting state retains `inferred_high` authority: the callback was a
    /// fact, while choosing this child was still a correlation.
    fn may_set_turn_for_event(&self, event: &TurnEvent, node: &NodeId) -> bool {
        if self.may_set_turn(node, event.confidence) {
            return true;
        }
        matches!(&event.kind, EventKind::AgentTurnStarted { .. })
            && event.confidence == Confidence::InferredHigh
            && event.parent_node_id.is_some()
            && self
                .sessions
                .get(&event.session_id)
                .and_then(|session| session.tree.get(node))
                .is_some_and(|subject| subject.interaction_pending)
    }
}

fn bind_explicit_subject(event: &mut TurnEvent, subject: &NodeId) {
    event.node_id = Some(subject.clone());
    event.dedup_key = format!("{}|subject:{subject}", event.dedup_key);
}

fn bind_inferred_subject(event: &mut TurnEvent, subject: &NodeId) {
    event.node_id = Some(subject.clone());
    event.confidence = event.confidence.min(Confidence::InferredHigh);
    event.dedup_key = format!("{}|subject:{subject}", event.dedup_key);
}

fn preserve_unresolved_subject(event: &mut TurnEvent, parent: &NodeId) {
    event.confidence = Confidence::Unknown;
    event.dedup_key = format!("{}|subject:unresolved-under:{parent}", event.dedup_key);
}

fn preserve_unresolved_external_subject(event: &mut TurnEvent, parent: &NodeId, external_id: &str) {
    event.confidence = Confidence::Unknown;
    event.dedup_key = format!(
        "{}|subject:external:{external_id}-unresolved-under:{parent}",
        event.dedup_key
    );
}

/// Whether an event kind moves the agent turn axis, and to what.
///
/// Used to decide whether the authority check applies. Lifecycle-only events — a
/// process starting, a child appearing — are never subject to it: the process table
/// and an exit status are facts regardless of what any agent reports.
fn turn_axis_change(kind: &EventKind) -> Option<()> {
    matches!(
        kind,
        EventKind::AgentStarted { .. }
            | EventKind::AgentTurnStarted { .. }
            | EventKind::AgentWaitingForUser { .. }
            | EventKind::AgentQuestionAsked { .. }
            | EventKind::AgentPermissionRequired { .. }
            | EventKind::AgentPermissionResolved { .. }
            | EventKind::AgentTurnCompleted { .. }
            | EventKind::AgentTaskCompleted { .. }
            | EventKind::AgentFailed { .. }
            | EventKind::AgentIdle
    )
    .then_some(())
}
