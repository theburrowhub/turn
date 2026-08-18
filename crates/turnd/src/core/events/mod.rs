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

use super::{Core, DeferredRuntimeInput, FailedIngestCheckpoint};
use turn_agents::IntegrationLevel;
use turn_core::event::{Confidence, EventKind, EventSource, TurnEvent};
use turn_core::ids::NodeId;
use turn_core::model::{
    AgentIdentitySource, LaunchConfiguration, NodeKind, Observable, ObservationSource,
    ObservationSourceKind, PendingPermission, ProcessNode,
};
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

/// Durable/display bounds for facts discovered from agents or the OS process
/// table. The raw process-table snapshot remains available to the supervisor for
/// classification and PID traversal; only this safe projection crosses into an
/// event, Session tree, inspector, or SQLite.
pub(super) const MAX_DISCOVERED_COMMAND_CHARS: usize = turn_agents::text::MAX_COMMAND_CHARS;
pub(super) const MAX_DISCOVERED_CWD_CHARS: usize = 4_096;
pub(super) const MAX_DISCOVERED_ARGS: usize = 128;
pub(super) const MAX_DISCOVERED_ARG_CHARS: usize = 1_024;
pub(super) const MAX_DISCOVERED_ARGV_CHARS: usize = 4_096;
pub(super) const MAX_AGENT_TASK_CHARS: usize = 512;
pub(super) const UNPRINTABLE_LABEL: &str = "[unprintable]";

pub(super) fn safe_untrusted_label(raw: &str, max_chars: usize) -> Option<String> {
    turn_pty::sanitise_label(raw, max_chars)
}

pub(super) fn safe_untrusted_args(args: Vec<String>) -> Vec<String> {
    let mut remaining = MAX_DISCOVERED_ARGV_CHARS;
    let mut safe = Vec::with_capacity(args.len().min(MAX_DISCOVERED_ARGS));
    for argument in args.into_iter().take(MAX_DISCOVERED_ARGS) {
        if remaining == 0 {
            break;
        }
        if argument.is_empty() {
            safe.push(String::new());
            continue;
        }
        let limit = remaining.min(MAX_DISCOVERED_ARG_CHARS);
        let projected = match safe_untrusted_label(&argument, limit) {
            Some(projected) => projected,
            None if remaining >= UNPRINTABLE_LABEL.chars().count() => UNPRINTABLE_LABEL.into(),
            None => break,
        };
        remaining = remaining.saturating_sub(projected.chars().count());
        safe.push(projected);
    }
    safe
}

fn normalise_untrusted_tree_fields(event: &mut TurnEvent) {
    match &mut event.kind {
        EventKind::AgentSpawned {
            declared_name,
            agent_type,
            task,
            ..
        } => {
            *declared_name = declared_name
                .take()
                .and_then(|name| safe_untrusted_label(&name, turn_pty::MAX_TITLE_CHARS));
            *agent_type = agent_type
                .take()
                .and_then(|kind| safe_untrusted_label(&kind, turn_pty::MAX_TITLE_CHARS));
            *task = task
                .take()
                .and_then(|task| safe_untrusted_label(&task, MAX_AGENT_TASK_CHARS));
        }
        EventKind::ProcessSpawnedChild {
            command, args, cwd, ..
        } => {
            *command = safe_untrusted_label(command, MAX_DISCOVERED_COMMAND_CHARS)
                .unwrap_or_else(|| "process".into());
            *args = safe_untrusted_args(std::mem::take(args));
            *cwd = cwd.take().map(|cwd| {
                safe_untrusted_label(&cwd, MAX_DISCOVERED_CWD_CHARS)
                    .unwrap_or_else(|| UNPRINTABLE_LABEL.into())
            });
        }
        EventKind::AgentStarted { tool, model, .. } => {
            *tool = safe_untrusted_label(tool, turn_pty::MAX_TITLE_CHARS)
                .unwrap_or_else(|| "agent".into());
            *model = model
                .take()
                .and_then(|model| turn_agents::safe_model_name(&model));
        }
        _ => {}
    }
}

impl Core {
    /// Applies one event: state, store, attention, pushes.
    pub(crate) fn ingest(&mut self, event: TurnEvent, now_ms: i64) {
        // Give older gaps the first chance to recover before this event advances
        // the durable projection. Retries only publish after their own atomic
        // checkpoint commits.
        self.retry_failed_ingest_checkpoints(now_ms);
        if !self.failed_ingest_checkpoints.is_empty() {
            let already_held = self
                .failed_ingest_checkpoints
                .iter()
                .any(|pending| pending.event.id == event.id)
                || self
                    .deferred_runtime_inputs
                    .iter()
                    .any(|pending| {
                        matches!(pending, DeferredRuntimeInput::Event { event: held, .. } if held.id == event.id)
                    });
            if !already_held {
                tracing::warn!(
                    event = %event.id,
                    session = %event.session_id,
                    "deferred a runtime event behind a failed atomic checkpoint"
                );
                self.deferred_runtime_inputs
                    .push_back(DeferredRuntimeInput::Event {
                        event: Box::new(event),
                        now_ms,
                    });
            }
            return;
        }
        self.ingest_after_checkpoint_barrier(event, now_ms);
    }

    fn ingest_after_checkpoint_barrier(&mut self, mut event: TurnEvent, now_ms: i64) {
        // Typed does not mean trusted. Future adapters and supervisor backends can
        // construct these events without going through today's payload filters.
        // Normalise once before state, pushes, and persistence so every consumer
        // observes the same bounded single-line projection.
        normalise_untrusted_tree_fields(&mut event);
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
        // Legacy adapters attached SubagentStop to the hook runtime as `node_id`.
        // Preserve that authenticated boundary before external-id lookup; otherwise
        // a same-session id from another parent could be accepted globally.
        if event.parent_node_id.is_none() {
            if let EventKind::AgentSubagentStopped { agent_id } = &event.kind {
                let node_is_exact_subject = event
                    .node_id
                    .as_ref()
                    .and_then(|node| session.tree.get(node))
                    .is_some_and(|node| {
                        node.kind == NodeKind::Subagent
                            && agent_id.as_deref().is_some()
                            && node.agent.as_ref().is_some_and(|agent| {
                                agent_id
                                    .as_deref()
                                    .is_some_and(|id| agent.matches_external_id(id))
                            })
                    });
                if !node_is_exact_subject {
                    event.parent_node_id = event.node_id.take();
                }
            }
        }
        // AgentSpawned names the child in `agent`, while `node_id` is the
        // authenticated parent. Remapping that event to an existing child would
        // pass the child back as its own parent and create a phantom grandchild.
        if !matches!(&event.kind, EventKind::AgentSpawned { .. }) {
            if let Some(external_id) = event.agent.external_id.as_deref() {
                if let Some(subject) = session.tree.find_by_external_id(external_id) {
                    let belongs_to_hook_parent =
                        event.parent_node_id.as_ref().is_none_or(|parent| {
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
        }
        self.correlate_unbound_agent_event(&mut event);
        self.correlate_lifecycle_subject(&mut event);
        self.promote_authenticated_integration(&event);
        let policy = self.attention_policy_for_session(&session_id);

        let mut changed = self.apply(&event, now_ms);
        if matches!(&event.kind, EventKind::AgentSubagentStopped { .. }) && event.node_id.is_none()
        {
            if let Some(node) = changed.node.as_ref() {
                event.node_id = Some(node.clone());
                event.dedup_key = format!("{}|lifecycle-subject:{node}", event.dedup_key);
            }
        }
        let terminal_dependents = if matches!(
            &event.kind,
            EventKind::AgentSubagentStopped { .. }
                | EventKind::ProcessExited { .. }
                | EventKind::ProcessFailed { .. }
        ) {
            let stopped = changed.node.clone();
            stopped.map_or_else(Vec::new, |node| {
                self.mark_runtime_dependents(&session_id, &node, now_ms)
            })
        } else {
            Vec::new()
        };
        changed.structure |= !terminal_dependents.is_empty();
        let preview_changed = !changed.refused
            && self.update_preview_from_event(&event, changed.node.as_ref(), now_ms);
        // Capacity/status refreshes are telemetry, not operator or agent
        // activity. Letting a periodic quota probe touch the Session would move
        // otherwise idle work to the top of the attention-oriented navigator.
        if !matches!(&event.kind, EventKind::AgentRuntimeObserved { .. }) {
            if let Some(session) = self.sessions.get_mut(&session_id) {
                session.touch(now_ms);
            }
        }

        // An event too weak to change the state is also too weak to demand attention.
        // Letting a refused guess into the queue would produce the worst of both: the
        // node saying one thing and the sidebar asking about another. It is still
        // recorded and still pushed, so the user can see what the heuristic thought.
        let attention_before = self.attention.queue().clone();
        for (node, parent, external_id) in &terminal_dependents {
            self.attention.resolve_lifecycle(
                &session_id,
                node,
                parent.as_ref(),
                external_id.as_deref(),
            );
        }
        let effects = if changed.refused {
            Vec::new()
        } else {
            self.attention
                .ingest(&event, &policy, &self.user.clone(), now_ms)
        };
        let attention_changed = self.attention.queue() != &attention_before;
        let attention_change_reported = effects.iter().any(|effect| {
            matches!(
                effect,
                turn_core::attention::Effect::Enqueued { .. }
                    | turn_core::attention::Effect::Cleared { .. }
            )
        });

        let checkpoint = self.sessions.get(&session_id).map_or(Ok(()), |session| {
            self.store
                .checkpoint_event_session_attention(session, &event, self.attention.queue())
        });
        if let Err(error) = checkpoint {
            tracing::error!(
                %error,
                session = %session_id,
                event = %event.id,
                kind = turn_core::event::event_name(&event.kind),
                "atomic runtime-event checkpoint failed; suppressing projections and scheduling retry"
            );
            if !self
                .failed_ingest_checkpoints
                .iter()
                .any(|pending| pending.event.id == event.id)
            {
                self.failed_ingest_checkpoints
                    .push_back(FailedIngestCheckpoint { event, effects });
            }
            return;
        }

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
        self.emit_checkpointed_effects(effects, now_ms);
        if attention_changed && !attention_change_reported {
            self.push_attention_queue(now_ms);
        }

        if changed.refused {
            tracing::debug!(
                session = %session_id,
                kind = turn_core::event::event_name(&event.kind),
                confidence = event.confidence.label(),
                "kept the existing state: the event was less trustworthy than what set it"
            );
        }
    }

    /// A configured hook is only a promise. The first authenticated event is the
    /// evidence that promotes a launch and retires its output heuristic.
    fn promote_authenticated_integration(&mut self, event: &TurnEvent) {
        let promoted = match &event.source {
            EventSource::Hook { .. } => IntegrationLevel::Structured,
            EventSource::SideChannel { .. } => IntegrationLevel::Wrapper,
            _ => return,
        };
        let candidates = [event.node_id.as_ref(), event.parent_node_id.as_ref()];
        let runtime_id = candidates.into_iter().flatten().find_map(|candidate| {
            self.processes
                .iter()
                .find(|(runtime_id, process)| {
                    *runtime_id == candidate || process.hosted.as_ref() == Some(candidate)
                })
                .map(|(runtime_id, _)| runtime_id.clone())
        });
        let Some(runtime_id) = runtime_id else {
            return;
        };
        let Some(process) = self.processes.get_mut(&runtime_id) else {
            return;
        };
        if process.level >= promoted {
            return;
        }
        process.level = promoted;
        process.heuristic = None;

        let semantic_id = process.hosted.as_ref().unwrap_or(&runtime_id).clone();
        if let Some(session) = self.sessions.get_mut(&process.session_id) {
            if let Some(node) = session.tree.get_mut(&semantic_id) {
                node.env_highlights
                    .insert("TURN_INTEGRATION".into(), promoted.label().to_string());
            }
        }
    }

    /// Retries event checkpoints that failed before any client saw them.
    ///
    /// The current Session and attention queue are complete projections, so using
    /// their newest values is safe even if unrelated output arrived while SQLite
    /// was unavailable. Events themselves remain FIFO and append idempotently.
    pub(crate) fn retry_failed_ingest_checkpoints(&mut self, now_ms: i64) {
        while let Some(pending) = self.failed_ingest_checkpoints.pop_front() {
            let event = &pending.event;
            let Some(session) = self.sessions.get(&event.session_id) else {
                tracing::warn!(
                    event = %event.id,
                    session = %event.session_id,
                    "discarded a failed checkpoint after its Session was removed"
                );
                continue;
            };
            if let Err(error) = self.store.checkpoint_event_session_attention(
                session,
                event,
                self.attention.queue(),
            ) {
                tracing::warn!(
                    %error,
                    event = %event.id,
                    session = %event.session_id,
                    "runtime-event checkpoint retry is still blocked"
                );
                self.failed_ingest_checkpoints.push_front(pending);
                break;
            }

            let session_id = event.session_id.clone();
            tracing::info!(
                event = %event.id,
                session = %session_id,
                "runtime-event checkpoint retry committed"
            );
            self.push_all(ServerEvent::TurnEventEmitted {
                turn_event: event.clone(),
            });
            self.push_tree(&session_id, now_ms);
            if let Some(node) = &event.node_id {
                self.push_node_state(&session_id, node, Some(event.clone()), now_ms);
            }
            self.push_session_state(&session_id, now_ms);
            let queue_change_reported = pending.effects.iter().any(|effect| {
                matches!(
                    effect,
                    turn_core::attention::Effect::Enqueued { .. }
                        | turn_core::attention::Effect::Cleared { .. }
                )
            });
            self.emit_checkpointed_effects(pending.effects, now_ms);
            if !queue_change_reported {
                self.push_attention_queue(now_ms);
            }
        }

        while self.failed_ingest_checkpoints.is_empty() {
            let Some(input) = self.deferred_runtime_inputs.pop_front() else {
                break;
            };
            match input {
                DeferredRuntimeInput::Event { event, now_ms } => {
                    self.ingest_after_checkpoint_barrier(*event, now_ms);
                }
                DeferredRuntimeInput::Exit {
                    session_id,
                    node_id,
                    info,
                    now_ms,
                } => self.record_exit(&session_id, &node_id, info, now_ms),
            }
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

    /// Gives terminal lifecycle events the exact tree identity Attention needs.
    ///
    /// Supervisor exits start with only a node id, while `SubagentStop` arrives
    /// through its parent's hook endpoint. The tree already knows the declared
    /// node, parent and tool-owned id; filling only missing fields lets lifecycle
    /// retire a pre-declaration attention scope without making response callbacks
    /// any broader.
    fn correlate_lifecycle_subject(&self, event: &mut TurnEvent) {
        let is_process_terminal = matches!(
            &event.kind,
            EventKind::ProcessExited { .. } | EventKind::ProcessFailed { .. }
        );
        let stopped_external = match &event.kind {
            EventKind::AgentSubagentStopped { agent_id } => Some(agent_id.clone()),
            _ => None,
        };
        if !is_process_terminal && stopped_external.is_none() {
            return;
        }

        let Some(session) = self.sessions.get(&event.session_id) else {
            return;
        };
        let mut target = event.node_id.clone();

        if let Some(agent_id) = stopped_external {
            if event.agent.external_id.is_none() {
                event.agent.external_id = agent_id.clone();
            }
            let hook_parent = event.parent_node_id.clone().or_else(|| {
                target.as_ref().and_then(|target| {
                    session.tree.get(target).and_then(|node| {
                        if node.kind == NodeKind::Subagent {
                            node.parent.clone()
                        } else {
                            Some(node.id.clone())
                        }
                    })
                })
            });
            let belongs_to_hook_parent = |candidate: &NodeId| {
                hook_parent.as_ref().is_none_or(|parent| {
                    session
                        .tree
                        .descendants(parent)
                        .into_iter()
                        .any(|descendant| descendant.id == *candidate)
                })
            };
            let current_is_valid_subagent = target
                .as_ref()
                .and_then(|node| session.tree.get(node))
                .is_some_and(|node| {
                    let external_matches = agent_id.as_deref().is_none_or(|external_id| {
                        node.agent
                            .as_ref()
                            .is_some_and(|agent| agent.matches_external_id(external_id))
                    });
                    node.kind == NodeKind::Subagent
                        && belongs_to_hook_parent(&node.id)
                        && external_matches
                });
            if !current_is_valid_subagent {
                let mut inferred = false;
                target = match agent_id.as_deref() {
                    Some(external_id) => session
                        .tree
                        .find_by_external_id(external_id)
                        .filter(|node| {
                            node.kind == NodeKind::Subagent && belongs_to_hook_parent(&node.id)
                        })
                        .map(|node| node.id.clone()),
                    None => hook_parent.as_ref().and_then(|parent| {
                        let candidates: Vec<_> = session
                            .tree
                            .children(parent)
                            .into_iter()
                            .filter(|node| node.kind == NodeKind::Subagent && node.is_running())
                            .map(|node| node.id.clone())
                            .collect();
                        match candidates.as_slice() {
                            [only] => {
                                inferred = true;
                                Some(only.clone())
                            }
                            _ => {
                                event.confidence = Confidence::Unknown;
                                None
                            }
                        }
                    }),
                };
                if target.is_none() {
                    event.parent_node_id = hook_parent;
                    event.node_id = None;
                    return;
                }
                if inferred {
                    event.confidence = event.confidence.min(Confidence::InferredHigh);
                }
            }
            event.parent_node_id = hook_parent;
        }

        let Some(target_id) = target else {
            return;
        };
        let Some(node) = session.tree.get(&target_id) else {
            return;
        };
        event.node_id = Some(target_id.clone());
        if event.parent_node_id.is_none() {
            event.parent_node_id = node.parent.clone();
        }
        if event.agent.external_id.is_none() {
            event.agent.external_id = node.agent.as_ref().and_then(|agent| {
                agent
                    .external_id
                    .clone()
                    .or_else(|| agent.agent.external_id.clone())
            });
        }
        event.dedup_key = format!("{}|lifecycle-subject:{target_id}", event.dedup_key);
    }

    /// Applies an event to the session's process tree.
    fn apply(&mut self, event: &TurnEvent, now_ms: i64) -> Changed {
        let session_id = event.session_id.clone();
        if let EventKind::AgentSubagentStopped { agent_id } = &event.kind {
            return match event.node_id.as_ref() {
                Some(target) => self.stop_subagent(&session_id, target, now_ms),
                None => match (event.parent_node_id.as_ref(), agent_id.as_ref()) {
                    (Some(parent), Some(agent_id)) => self.insert_stopped_subagent(
                        &session_id,
                        parent,
                        agent_id.clone(),
                        subagent_identity_source(&event.source),
                        now_ms,
                    ),
                    _ => Changed::default(),
                },
            };
        }
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
            return self.insert_subagent_from(
                &session_id,
                &node_id,
                declared_name.clone(),
                agent_type.clone(),
                agent_id.clone(),
                task.clone(),
                subagent_identity_source(&event.source),
                now_ms,
            );
        }
        if let EventKind::ProcessSpawnedChild {
            child,
            pid,
            ppid,
            command,
            args,
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
                args.clone(),
                cwd.clone(),
                *confirmed_parent,
                now_ms,
            );
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
                if node.lifecycle.is_terminal() {
                    return Changed {
                        node: Some(node_id),
                        structure: false,
                        refused: true,
                    };
                }
                node.pid = Some(*pid);
                node.command = command.clone();
                node.lifecycle = Lifecycle::Alive;
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
                clear_interaction_state(node);
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
                clear_interaction_state(node);
            }

            EventKind::AgentStarted {
                tool,
                model,
                external_id,
            } => {
                // SessionStart initialises a fresh node; later callbacks may use
                // this event to refresh model/session metadata mid-turn.
                if node.turn.is_none() {
                    node.turn = Some(Turn::Idle);
                }
                let agent = node.agent.get_or_insert_with(Default::default);
                agent.agent.tool = Some(tool.clone());
                if let Some(model) = model {
                    let mut configuration = match &agent.runtime.launch.current {
                        Observable::Observed { value, .. } => value.clone(),
                        Observable::Waiting
                        | Observable::Unsupported { .. }
                        | Observable::Stale { .. }
                        | Observable::Failed { .. } => LaunchConfiguration::default(),
                    };
                    configuration.model = Some(model.clone());
                    let observed = Observable::observed(
                        configuration,
                        ObservationSource::new(
                            ObservationSourceKind::Provider,
                            format!("{tool} event"),
                        ),
                        event.timestamp_ms,
                        None,
                    );
                    agent.runtime.launch.current =
                        std::mem::take(&mut agent.runtime.launch.current).prefer_newer(observed);
                    if let Some(current_model) = agent
                        .runtime
                        .launch
                        .current
                        .value()
                        .and_then(|configuration| configuration.model.clone())
                    {
                        agent.agent.model = Some(current_model);
                    }
                }
                // The tool's own session id, which is what a resume needs and what a
                // later hook callback identifies itself by.
                if external_id.is_some() {
                    agent.external_id = external_id.clone();
                    agent.agent.external_id = external_id.clone();
                }
            }
            EventKind::AgentRuntimeObserved { runtime } => {
                let agent = node.agent.get_or_insert_with(Default::default);
                if event.agent.provider.is_some() {
                    agent.agent.provider = event.agent.provider.clone();
                }
                if event.agent.tool.is_some() {
                    agent.agent.tool = event.agent.tool.clone();
                }
                agent.runtime =
                    std::mem::take(&mut agent.runtime).prefer_newer(runtime.as_ref().clone());
                // Mirror the winning current-model observation into the legacy
                // compact AgentRef. Reading it after `prefer_newer` is important:
                // an older detached transcript read must not roll the header back.
                if let Some(model) = agent
                    .runtime
                    .launch
                    .current
                    .value()
                    .and_then(|current| current.model.clone())
                {
                    agent.agent.model = Some(model);
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
                clear_interaction_state(node);
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
                clear_interaction_state(node);
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
                clear_interaction_state(node);
            }
            EventKind::AgentIdle => {
                node.turn = Some(Turn::Idle);
                clear_interaction_state(node);
            }

            // Session-level events say nothing about a particular node.
            EventKind::SessionNeedsAttention { .. }
            | EventKind::SessionAttentionResolved
            | EventKind::ContextHandoffFinished { .. }
            | EventKind::AgentRenamed { .. }
            | EventKind::AgentRelationshipCorrected { .. } => return Changed::default(),

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
        if self
            .sessions
            .get(&event.session_id)
            .and_then(|session| session.tree.get(node))
            .is_some_and(|subject| subject.lifecycle.is_terminal())
        {
            return false;
        }
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

/// A node that is no longer waiting cannot still expose an actionable permission
/// or question. Keep this cleanup shared across semantic completion and runtime
/// termination so the persisted inspector state stays honest.
pub(super) fn clear_interaction_state(node: &mut ProcessNode) {
    node.interaction_pending = false;
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

fn subagent_identity_source(source: &EventSource) -> Option<AgentIdentitySource> {
    match source {
        EventSource::Hook { tool, event_name } if tool == "claude-code" => {
            match event_name.as_str() {
                "SubagentStart" | "SubagentStop" => Some(AgentIdentitySource::Lifecycle),
                "PostToolUse" => Some(AgentIdentitySource::ParentSpawn),
                _ => None,
            }
        }
        _ => None,
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::testing::Harness;
    use turn_core::ids::{PaneId, SessionId};
    use turn_core::model::{
        AgentLaunchFacts, AgentRuntimeMetadata, ContextUsageSnapshot, LaunchConfiguration,
        Observable, ObservationSource, ObservationSourceKind, QuotaSnapshot, QuotaWindow,
        UsageMeasurement, UsageMeasurementKind, UsageUnit,
    };

    const NOW: i64 = 1_700_000_000_000;

    fn observed_runtime(
        at_ms: i64,
        used: u64,
        model: &str,
        effort: &str,
        quota_remaining: f64,
    ) -> AgentRuntimeMetadata {
        let source = ObservationSource::new(ObservationSourceKind::Provider, "provider transcript");
        AgentRuntimeMetadata {
            launch: AgentLaunchFacts {
                current: Observable::observed(
                    LaunchConfiguration {
                        model: Some(model.into()),
                        effort_level: Some(effort.into()),
                        ..LaunchConfiguration::default()
                    },
                    source.clone(),
                    at_ms,
                    None,
                ),
                ..AgentLaunchFacts::default()
            },
            context: Observable::observed(
                ContextUsageSnapshot {
                    scope_id: Some("conversation-1".into()),
                    measurement: UsageMeasurement {
                        kind: UsageMeasurementKind::Used,
                        amount: used as f64,
                        unit: UsageUnit::Tokens,
                        total: Some(200_000.0),
                    },
                    effective_window: None,
                    window_size_tokens: None,
                    used_percentage: None,
                    remaining_percentage: None,
                    current_usage: None,
                },
                source.clone(),
                at_ms,
                None,
            ),
            quota: Observable::observed(
                QuotaSnapshot {
                    scope_id: None,
                    scope_label: Some("provider account".into()),
                    windows: vec![QuotaWindow {
                        label: "5h".into(),
                        measurement: UsageMeasurement {
                            kind: UsageMeasurementKind::Remaining,
                            amount: quota_remaining,
                            unit: UsageUnit::Percent,
                            total: Some(100.0),
                        },
                        resets_at_ms: Some(at_ms + 60_000),
                        hard_limit: None,
                    }],
                },
                source,
                at_ms,
                Some(at_ms + 60_000),
            ),
        }
    }

    #[tokio::test]
    async fn runtime_observations_merge_per_fact_and_never_move_the_turn_axis() {
        let mut harness = Harness::new().await;
        let session_id = SessionId::from_stored("sess_runtime_observation");
        harness.add_session(session_id.clone(), PaneId::new(), NOW);

        let mut node = ProcessNode::agent(session_id.clone(), "codex", "/repo", NOW);
        node.lifecycle = Lifecycle::Alive;
        node.turn = Some(Turn::Active);
        let node_id = node.id.clone();
        harness
            .core
            .sessions
            .get_mut(&session_id)
            .unwrap()
            .tree
            .insert(node);

        let event = |runtime, at_ms| {
            TurnEvent::new(
                session_id.clone(),
                EventKind::AgentRuntimeObserved {
                    runtime: Box::new(runtime),
                },
                EventSource::SideChannel {
                    tool: "codex".into(),
                    channel: "provider transcript".into(),
                },
                Confidence::Explicit,
                at_ms,
            )
            .with_node(node_id.clone())
        };

        let newer = event(
            observed_runtime(NOW + 20, 82_000, "gpt-new", "xhigh", 76.5),
            NOW + 20,
        );
        let late_older = event(
            observed_runtime(NOW + 10, 41_000, "gpt-old", "low", 12.0),
            NOW + 30,
        );
        assert_eq!(
            harness.core.apply(&newer, NOW + 20).node,
            Some(node_id.clone())
        );
        assert_eq!(
            harness.core.apply(&late_older, NOW + 30).node,
            Some(node_id.clone())
        );

        let node = harness.core.sessions[&session_id]
            .tree
            .get(&node_id)
            .unwrap();
        let agent = node.agent.as_ref().unwrap();
        assert_eq!(node.turn, Some(Turn::Active));
        assert_eq!(agent.agent.model.as_deref(), Some("gpt-new"));
        assert_eq!(
            agent
                .runtime
                .launch
                .current
                .value()
                .unwrap()
                .effort_level
                .as_deref(),
            Some("xhigh")
        );
        assert_eq!(
            agent.runtime.context.observed_at_ms(),
            Some(NOW + 20),
            "a detached read that finishes late must not overwrite newer evidence"
        );
        assert_eq!(
            agent.runtime.context.value().unwrap().measurement.amount,
            82_000.0
        );
        assert_eq!(
            agent.runtime.quota.value().unwrap().windows[0]
                .measurement
                .amount,
            76.5,
            "late older provider capacity must not roll back the current quota"
        );
    }

    #[tokio::test]
    async fn runtime_telemetry_does_not_make_an_idle_session_look_recent() {
        let mut harness = Harness::new().await;
        let session_id = SessionId::from_stored("sess_runtime_recency");
        harness.add_session(session_id.clone(), PaneId::new(), NOW);

        let mut node = ProcessNode::agent(session_id.clone(), "codex", "/repo", NOW);
        node.lifecycle = Lifecycle::Alive;
        node.turn = Some(Turn::Idle);
        let node_id = node.id.clone();
        harness
            .core
            .sessions
            .get_mut(&session_id)
            .unwrap()
            .tree
            .insert(node);

        let event = TurnEvent::new(
            session_id.clone(),
            EventKind::AgentRuntimeObserved {
                runtime: Box::new(observed_runtime(
                    NOW + 60_000,
                    42_000,
                    "gpt-current",
                    "high",
                    58.0,
                )),
            },
            EventSource::SideChannel {
                tool: "codex".into(),
                channel: "account quota".into(),
            },
            Confidence::Explicit,
            NOW + 60_000,
        )
        .with_node(node_id);

        harness.core.ingest(event, NOW + 60_000);
        assert_eq!(harness.core.sessions[&session_id].last_activity_ms, NOW);
    }
}
