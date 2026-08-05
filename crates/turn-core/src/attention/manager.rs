//! The attention manager: events in, concrete effects out.
//!
//! This is the only place that decides the user gets interrupted. It owns the
//! queue and the focus governor, applies per-session policy, and honours mutes
//! and cooldowns. Everything it emits is an [`Effect`] the UI layer performs —
//! the manager itself never touches the screen, which is what makes all of this
//! testable without a window.

use crate::attention::focus::{
    DeferReason, FocusDecision, FocusDenial, FocusGovernor, UserContext,
};
use crate::attention::policy::{Action, AttentionPolicy, Sound, Trigger};
use crate::attention::queue::{
    subject_is_resolved, AttentionDemandKind, AttentionEntry, AttentionQueue, EntryState,
    SubjectRef,
};
use crate::event::{Confidence, EventKind, EventSource, TurnEvent};
use crate::ids::{AttentionId, NodeId, SessionId};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// How long a deferred focus request stays valid. Past this, the badge stands on
/// its own and the pending jump is dropped — being yanked somewhere because of
/// something that happened two minutes ago is worse than not being moved.
pub const DEFERRED_FOCUS_TTL_MS: i64 = 60_000;

/// A concrete thing for the UI to do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "effect", rename_all = "snake_case")]
pub enum Effect {
    Badge {
        session_id: SessionId,
        count: usize,
    },
    Highlight {
        session_id: SessionId,
    },
    PlaySound {
        session_id: SessionId,
        sound: Sound,
    },
    Notify {
        session_id: SessionId,
        title: String,
        body: String,
    },
    Enqueued {
        attention_id: AttentionId,
        session_id: SessionId,
    },
    /// Move the user. Already cleared by the governor.
    Focus {
        session_id: SessionId,
        node_id: Option<NodeId>,
    },
    /// Focus was postponed; the UI shows the badge and waits for a later tick.
    FocusDeferred {
        session_id: SessionId,
        until_ms: i64,
        reason: DeferReason,
    },
    /// Focus was refused outright.
    FocusDenied {
        session_id: SessionId,
        reason: FocusDenial,
    },
    /// Run the session's custom command.
    RunCustom {
        session_id: SessionId,
        command: String,
    },
    /// A session no longer needs anything.
    Cleared {
        session_id: SessionId,
    },
}

/// A focus request waiting for a better moment.
#[derive(Debug, Clone)]
struct DeferredFocus {
    session_id: SessionId,
    node_id: Option<NodeId>,
    parent_node_id: Option<NodeId>,
    subject_external_id: Option<String>,
    confidence: Confidence,
    demand_kind: AttentionDemandKind,
    action: Action,
    /// Whether resolution is represented by a durable queue entry. Policies may
    /// request focus without enqueueing, in which case the deferred request is
    /// itself the short-lived evidence to re-evaluate.
    requires_queue_entry: bool,
    until_ms: i64,
    created_ms: i64,
    /// The policy in force when the request was made. Carried along because
    /// re-evaluating with defaults would ignore a session's own guard settings.
    policy: AttentionPolicy,
}

/// The inputs to one focus decision, grouped so the call site reads as a request
/// rather than a run of positional arguments.
struct FocusRequest<'a> {
    session: &'a SessionId,
    node_id: Option<NodeId>,
    parent_node_id: Option<NodeId>,
    subject_external_id: Option<String>,
    confidence: Confidence,
    demand_kind: AttentionDemandKind,
    action: Action,
    requires_queue_entry: bool,
    policy: &'a AttentionPolicy,
}

/// Per-session bookkeeping the manager keeps on top of the policy.
#[derive(Debug, Clone, Default)]
struct SessionRuntime {
    /// Last time an attention batch produced something visible or audible.
    last_effect_ms: Option<i64>,
    /// Silenced until this timestamp.
    muted_until_ms: Option<i64>,
}

/// Owns the queue, the governor and the deferral list.
#[derive(Debug, Default)]
pub struct AttentionManager {
    queue: AttentionQueue,
    governor: FocusGovernor,
    runtimes: HashMap<SessionId, SessionRuntime>,
    deferred: Vec<DeferredFocus>,
}

impl AttentionManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Restores the durable queue without replaying old demands as new events.
    ///
    /// Focus cooldowns, deferred focus requests and transient mute timers belong
    /// to the daemon process that created them, so they intentionally start
    /// empty. Queue entries, by contrast, retain their exact persisted identity,
    /// age, state and priority.
    pub fn from_persisted_queue(queue: AttentionQueue) -> Self {
        Self {
            queue,
            ..Self::default()
        }
    }

    pub fn queue(&self) -> &AttentionQueue {
        &self.queue
    }

    /// Feeds an event through policy and guards, returning what to do.
    pub fn ingest(
        &mut self,
        event: &TurnEvent,
        policy: &AttentionPolicy,
        ctx: &UserContext,
        now_ms: i64,
    ) -> Vec<Effect> {
        // A failure is both terminal lifecycle and a new attention trigger. Clear
        // anything the dead runtime used to own, then continue through policy so
        // the failure itself can be enqueued. A clean exit is consumed below.
        let incoming_demand_kind = attention_demand_kind(&event.kind);
        let repeated_durable_id = incoming_demand_kind.and_then(|kind| {
            self.queue
                .iter()
                .find(|entry| {
                    entry.survives_owner_exit
                        && entry.demand_kind == kind
                        && entry_matches_event_subject(entry, event)
                })
                .map(|entry| entry.id.clone())
        });
        let resolving_effects = if matches!(&event.kind, EventKind::ProcessFailed { .. }) {
            self.resolve_lifecycle_event(event, false)
        } else {
            Vec::new()
        };

        if let Some(kind) = incoming_demand_kind {
            self.prepare_informational_subject(event, kind);
        }

        // Some events end a demand rather than raise one.
        if let Some(effects) = self.handle_resolving_event(event, now_ms) {
            return effects;
        }

        let Some(trigger) = Trigger::from_event(&event.kind) else {
            return resolving_effects;
        };

        let session = event.session_id.clone();
        let existing_demand_state = queue_reason(&event.kind).and_then(|reason| {
            self.queue
                .iter()
                .find(|entry| entry.reason == reason && entry_matches_event_subject(entry, event))
                .map(|entry| entry.state)
        });
        let demand_already_queued = existing_demand_state.is_some();
        let replay_suppresses_interruptions = repeated_durable_id.is_some()
            || matches!(
                existing_demand_state,
                Some(EntryState::Pending | EntryState::Snoozed { .. })
            );

        // Muting suppresses interruption, not evidence. Queue bookkeeping still
        // follows policy; only perceptible/active actions are filtered below.
        let muted = self.is_muted(&session, now_ms);
        let last_effect = self
            .runtimes
            .get(&session)
            .and_then(|runtime| runtime.last_effect_ms);
        let cooldown_active = last_effect.is_some_and(|last| {
            now_ms.saturating_sub(last) < i64::from(policy.cooldown_seconds) * 1_000
        });

        let actions = policy.resolve(trigger, event.confidence);
        if actions.is_empty() {
            return resolving_effects;
        }

        let mut effects = resolving_effects;
        let mut enqueued_id = None;

        for action in &actions {
            if muted && !matches!(action, Action::Enqueue | Action::Badge) {
                continue;
            }
            if (cooldown_active || replay_suppresses_interruptions)
                && action_is_perceptible_non_focus(*action)
            {
                continue;
            }
            match action {
                Action::Nothing => {}
                Action::Enqueue => {
                    if repeated_durable_id.is_some() {
                        continue;
                    }
                    // Only demands that genuinely need the human get queued.
                    // A completed turn badges but does not necessarily block.
                    if let Some(reason) = queue_reason(&event.kind) {
                        let entry = AttentionEntry {
                            id: AttentionId::new(),
                            session_id: session.clone(),
                            node_id: event.node_id.clone(),
                            parent_node_id: event.parent_node_id.clone(),
                            subject_external_id: event.agent.external_id.clone(),
                            reason,
                            summary: summarise(&event.kind),
                            confidence: event.confidence,
                            created_ms: now_ms,
                            updated_ms: now_ms,
                            state: EntryState::Pending,
                            priority_boost: policy.priority_boost,
                            survives_owner_exit: demand_survives_owner_exit(&event.kind),
                            demand_kind: attention_demand_kind(&event.kind)
                                .unwrap_or(AttentionDemandKind::Interaction),
                        };
                        let id = self.queue.upsert(entry);
                        enqueued_id = Some(id.clone());
                        effects.push(Effect::Enqueued {
                            attention_id: id,
                            session_id: session.clone(),
                        });
                    }
                }
                Action::Badge => effects.push(Effect::Badge {
                    session_id: session.clone(),
                    count: self.queue.count_for_session(&session, now_ms).max(1),
                }),
                Action::Highlight => effects.push(Effect::Highlight {
                    session_id: session.clone(),
                }),
                Action::Sound => {
                    if policy.sound != Sound::None {
                        effects.push(Effect::PlaySound {
                            session_id: session.clone(),
                            sound: policy.sound,
                        });
                    }
                }
                Action::Notify => {
                    let (title, body) = notification_text(&event.kind);
                    effects.push(Effect::Notify {
                        session_id: session.clone(),
                        title,
                        body,
                    });
                }
                Action::Custom => {
                    if let Some(command) = &policy.custom_command {
                        effects.push(Effect::RunCustom {
                            session_id: session.clone(),
                            command: command.clone(),
                        });
                    }
                }
                focus_action if focus_action.is_focus() => {
                    // A replay refreshes durable evidence, but it must not create
                    // another future focus jump for the same queued demand.
                    if replay_suppresses_interruptions {
                        continue;
                    }
                    let decision = self.governor.evaluate(
                        *focus_action,
                        &session,
                        policy,
                        ctx,
                        last_effect,
                        now_ms,
                    );
                    effects.extend(
                        self.apply_focus_decision(
                            decision,
                            FocusRequest {
                                session: &session,
                                node_id: event.node_id.clone(),
                                parent_node_id: event.parent_node_id.clone(),
                                subject_external_id: event.agent.external_id.clone(),
                                confidence: event.confidence,
                                demand_kind: attention_demand_kind(&event.kind)
                                    .unwrap_or(AttentionDemandKind::Interaction),
                                action: *focus_action,
                                requires_queue_entry: demand_already_queued
                                    || (queue_reason(&event.kind).is_some()
                                        && actions.contains(&Action::Enqueue)),
                                policy,
                            },
                            ctx,
                            now_ms,
                        ),
                    );
                }
                _ => {}
            }
        }

        if muted
            && enqueued_id.is_some()
            && !cooldown_active
            && !replay_suppresses_interruptions
            && !effects
                .iter()
                .any(|effect| matches!(effect, Effect::Badge { .. }))
        {
            effects.push(Effect::Badge {
                session_id: session.clone(),
                count: self.queue.count_for_session(&session, now_ms),
            });
        }

        // Only something the user can actually perceive starts the cooldown.
        // Counting a deferral would let a single postponed jump silence its own
        // session for the whole cooldown, and the jump would never land.
        if effects.iter().any(is_perceptible) {
            self.runtimes.entry(session).or_default().last_effect_ms = Some(now_ms);
        }
        // Keep the enqueue effect first so consumers can find the queue id
        // without scanning.
        if enqueued_id.is_some() {
            effects.sort_by_key(|e| !matches!(e, Effect::Enqueued { .. }));
        }
        effects
    }

    /// Re-evaluates deferred focus requests. The UI calls this on a timer and
    /// whenever the user's activity state changes.
    pub fn tick(&mut self, ctx: &UserContext, now_ms: i64) -> Vec<Effect> {
        let mut effects = Vec::new();
        let mut still_deferred = Vec::new();
        // Take the list so the borrow checker lets us consult the governor.
        let pending = std::mem::take(&mut self.deferred);

        for item in pending {
            if now_ms.saturating_sub(item.created_ms) > DEFERRED_FOCUS_TTL_MS {
                // Too stale to act on. The badge already told the story.
                continue;
            }
            if now_ms < item.until_ms {
                still_deferred.push(item);
                continue;
            }
            if self.is_muted(&item.session_id, now_ms) {
                continue;
            }
            // The demand may have been handled in the meantime.
            if item.requires_queue_entry
                && !self.queue.has_actionable_subject(
                    &item.session_id,
                    item.node_id.as_ref(),
                    item.parent_node_id.as_ref(),
                    item.subject_external_id.as_deref(),
                    now_ms,
                )
            {
                continue;
            }

            // No session cooldown here: this is the tail of one already-approved
            // effect, not a new one. The governor's own guards (typing, rate
            // limit, ping-pong) still apply.
            let decision = self.governor.evaluate(
                item.action,
                &item.session_id,
                &item.policy,
                ctx,
                None,
                now_ms,
            );
            match decision {
                FocusDecision::Grant => {
                    self.governor
                        .record_grant(ctx.active_session.clone(), now_ms);
                    self.runtimes
                        .entry(item.session_id.clone())
                        .or_default()
                        .last_effect_ms = Some(now_ms);
                    effects.push(Effect::Focus {
                        session_id: item.session_id.clone(),
                        node_id: item.node_id.clone(),
                    });
                }
                FocusDecision::Defer {
                    reason: DeferReason::SessionCooldown | DeferReason::GlobalInterval,
                    ..
                } => {
                    // A different event won focus while this request waited.
                    // Keep its queue evidence, but never turn the queue into a
                    // timed sequence of involuntary focus changes.
                }
                FocusDecision::Defer { until_ms, .. } => {
                    still_deferred.push(DeferredFocus { until_ms, ..item });
                }
                FocusDecision::Deny { reason } => {
                    effects.push(Effect::FocusDenied {
                        session_id: item.session_id.clone(),
                        reason,
                    });
                }
            }
        }

        self.deferred = still_deferred;
        effects
    }

    /// The next demand the user should handle.
    pub fn next_attention(&self, now_ms: i64) -> Option<&AttentionEntry> {
        self.queue.next(now_ms)
    }

    /// Jumps to the next demand, marking it acknowledged.
    ///
    /// This is a user-initiated move, so it bypasses the governor's guards —
    /// pressing the shortcut is consent — but it still resets the rate limiter
    /// so automatic focus does not immediately fight the manual navigation.
    pub fn goto_next(&mut self, now_ms: i64) -> Option<Effect> {
        let entry = self.queue.next(now_ms)?;
        let session_id = entry.session_id.clone();
        let node_id = entry.node_id.clone();
        let id = entry.id.clone();
        self.acknowledge(&id);
        self.governor.reset();
        self.governor.record_grant(None, now_ms);
        Some(Effect::Focus {
            session_id,
            node_id,
        })
    }

    /// Jumps to one exact demand selected by the user.
    pub fn goto(&mut self, id: &AttentionId, now_ms: i64) -> Option<Effect> {
        let entry = self.queue.get(id)?;
        let session_id = entry.session_id.clone();
        let node_id = entry.node_id.clone();
        self.acknowledge(id);
        self.governor.reset();
        self.governor.record_grant(None, now_ms);
        Some(Effect::Focus {
            session_id,
            node_id,
        })
    }

    /// Marks one exact demand seen without pretending navigation occurred.
    pub fn acknowledge(&mut self, id: &AttentionId) -> bool {
        let subject = self.queue.get(id).cloned();
        let acknowledged = self.queue.acknowledge(id);
        if acknowledged {
            if let Some(subject) = subject {
                self.deferred
                    .retain(|deferred| !deferred_matches_entry(deferred, &subject));
            }
        }
        acknowledged
    }

    /// Advances past the demand the user is currently on.
    pub fn goto_after(&mut self, current: &AttentionId, now_ms: i64) -> Option<Effect> {
        let entry = self.queue.next_after(current, now_ms)?;
        let session_id = entry.session_id.clone();
        let node_id = entry.node_id.clone();
        let id = entry.id.clone();
        self.acknowledge(&id);
        self.governor.reset();
        self.governor.record_grant(None, now_ms);
        Some(Effect::Focus {
            session_id,
            node_id,
        })
    }

    /// Called when the user actually engages with a session: everything it was
    /// asking for is considered handled.
    pub fn engage_session(&mut self, session: &SessionId, now_ms: i64) -> Vec<Effect> {
        let cleared = self.queue.resolve_session(session);
        self.deferred.retain(|d| &d.session_id != session);
        self.governor.reset();
        let _ = now_ms;
        if cleared > 0 {
            vec![Effect::Cleared {
                session_id: session.clone(),
            }]
        } else {
            Vec::new()
        }
    }

    /// Silences a session until a deadline.
    pub fn mute_session(&mut self, session: &SessionId, until_ms: i64) {
        self.runtimes
            .entry(session.clone())
            .or_default()
            .muted_until_ms = Some(until_ms);
        self.deferred
            .retain(|deferred| &deferred.session_id != session);
    }

    pub fn unmute_session(&mut self, session: &SessionId) {
        if let Some(runtime) = self.runtimes.get_mut(session) {
            runtime.muted_until_ms = None;
        }
    }

    pub fn is_muted(&self, session: &SessionId, now_ms: i64) -> bool {
        self.runtimes
            .get(session)
            .and_then(|r| r.muted_until_ms)
            .is_some_and(|until| now_ms < until)
    }

    pub fn snooze(&mut self, id: &AttentionId, until_ms: i64) -> bool {
        let subject = self.queue.get(id).cloned();
        let snoozed = self.queue.snooze(id, until_ms);
        if snoozed {
            if let Some(subject) = subject {
                self.deferred
                    .retain(|deferred| !deferred_matches_entry(deferred, &subject));
            }
        }
        snoozed
    }

    pub fn dismiss(&mut self, id: &AttentionId) -> bool {
        let subject = self.queue.get(id).cloned();
        let dismissed = self.queue.dismiss(id);
        if dismissed {
            if let Some(subject) = subject {
                self.deferred
                    .retain(|deferred| !deferred_matches_entry(deferred, &subject));
            }
        }
        dismissed
    }

    /// Drops demands tied to a node that has gone away.
    pub fn resolve_node(&mut self, node: &NodeId) -> usize {
        self.deferred.retain(|d| d.node_id.as_ref() != Some(node));
        self.queue.resolve_node(node)
    }

    /// Drops exact demands and unresolved child scopes owned by a dead runtime.
    pub fn resolve_owner(&mut self, session: &SessionId, node: &NodeId) -> usize {
        self.deferred.retain(|deferred| {
            deferred.session_id != *session
                || (deferred.node_id.as_ref() != Some(node)
                    && !(deferred.node_id.is_none()
                        && deferred.parent_node_id.as_ref() == Some(node)))
        });
        self.queue.resolve_owner(session, node)
    }

    /// Drops one exact node inside its Session boundary.
    pub fn resolve_node_in_session(&mut self, session: &SessionId, node: &NodeId) -> usize {
        self.deferred.retain(|deferred| {
            deferred.session_id != *session || deferred.node_id.as_ref() != Some(node)
        });
        self.queue.resolve_node_in_session(session, node)
    }

    /// Removes all attention references to a structurally deleted owner.
    pub fn remove_owner_in_session(&mut self, session: &SessionId, node: &NodeId) -> usize {
        self.deferred.retain(|deferred| {
            deferred.session_id != *session
                || (deferred.node_id.as_ref() != Some(node)
                    && deferred.parent_node_id.as_ref() != Some(node))
        });
        self.queue.remove_owner_in_session(session, node)
    }

    /// Drops one exact lifecycle subject and its matching pre-declaration scope.
    pub fn resolve_lifecycle_subject(
        &mut self,
        session: &SessionId,
        node: &NodeId,
        parent: Option<&NodeId>,
        external_id: Option<&str>,
    ) -> usize {
        self.deferred.retain(|deferred| {
            !lifecycle_deferred_is_resolved(deferred, session, node, parent, external_id)
        });
        self.queue
            .resolve_lifecycle_subject(session, node, parent, external_id)
    }

    /// Applies all cleanup implied by one terminal lifecycle subject.
    pub fn resolve_lifecycle(
        &mut self,
        session: &SessionId,
        node: &NodeId,
        parent: Option<&NodeId>,
        external_id: Option<&str>,
    ) -> usize {
        let mut removed = self.resolve_owner(session, node);
        removed += self.resolve_lifecycle_subject(session, node, parent, external_id);
        removed
    }

    /// Resolves a terminal external identity before its AgentNode exists.
    pub fn resolve_lifecycle_scope(
        &mut self,
        session: &SessionId,
        parent: Option<&NodeId>,
        external_id: &str,
    ) -> usize {
        self.deferred.retain(|deferred| {
            deferred.session_id != *session
                || deferred.node_id.is_some()
                || deferred.parent_node_id.as_ref() != parent
                || deferred.subject_external_id.as_deref() != Some(external_id)
        });
        self.queue
            .resolve_lifecycle_scope(session, parent, external_id)
    }

    /// Focus changes granted recently, exposed for diagnostics.
    pub fn recent_focus_changes(&self, now_ms: i64) -> usize {
        self.governor.recent_grant_count(now_ms)
    }

    pub fn deferred_count(&self) -> usize {
        self.deferred.len()
    }

    fn apply_focus_decision(
        &mut self,
        decision: FocusDecision,
        request: FocusRequest<'_>,
        ctx: &UserContext,
        now_ms: i64,
    ) -> Option<Effect> {
        let FocusRequest {
            session,
            node_id,
            parent_node_id,
            subject_external_id,
            confidence,
            demand_kind,
            action,
            requires_queue_entry,
            policy,
        } = request;
        match decision {
            FocusDecision::Grant => {
                self.governor
                    .record_grant(ctx.active_session.clone(), now_ms);
                Some(Effect::Focus {
                    session_id: session.clone(),
                    node_id,
                })
            }
            FocusDecision::Defer {
                reason: DeferReason::SessionCooldown | DeferReason::GlobalInterval,
                ..
            } => {
                // Another attention event already won the automatic focus
                // grant. Queue this demand, but do not schedule a cascade of
                // future focus jumps; the user can advance with Next Attention.
                None
            }
            FocusDecision::Defer { until_ms, reason } => {
                if let Some(existing) = self.deferred.iter_mut().find(|deferred| {
                    deferred.session_id == *session
                        && deferred.node_id == node_id
                        && deferred.parent_node_id == parent_node_id
                        && deferred.subject_external_id == subject_external_id
                        && deferred.action == action
                }) {
                    existing.until_ms = existing.until_ms.max(until_ms);
                    existing.policy = policy.clone();
                    existing.requires_queue_entry = requires_queue_entry;
                    existing.confidence = existing.confidence.max(confidence);
                    existing.demand_kind = demand_kind;
                    return None;
                }
                self.deferred.push(DeferredFocus {
                    session_id: session.clone(),
                    node_id,
                    parent_node_id,
                    subject_external_id,
                    confidence,
                    demand_kind,
                    action,
                    requires_queue_entry,
                    until_ms,
                    created_ms: now_ms,
                    policy: policy.clone(),
                });
                Some(Effect::FocusDeferred {
                    session_id: session.clone(),
                    until_ms,
                    reason,
                })
            }
            FocusDecision::Deny { reason } => Some(Effect::FocusDenied {
                session_id: session.clone(),
                reason,
            }),
        }
    }

    /// Handles events that close demands instead of raising them.
    fn handle_resolving_event(&mut self, event: &TurnEvent, now_ms: i64) -> Option<Vec<Effect>> {
        let closes = match &event.kind {
            // The user answered, so a new turn is under way.
            EventKind::AgentTurnStarted { .. } => true,
            EventKind::AgentPermissionResolved { .. } => true,
            EventKind::SessionAttentionResolved | EventKind::AgentIdle => true,
            _ => false,
        };

        if let EventKind::ProcessExited { .. } = &event.kind {
            return Some(self.resolve_lifecycle_event(event, true));
        }

        if let EventKind::AgentSubagentStopped { .. } = &event.kind {
            return Some(self.resolve_lifecycle_event(event, true));
        }

        if !closes {
            return None;
        }

        let has_subject = event.node_id.is_some()
            || event.parent_node_id.is_some()
            || event.agent.external_id.is_some();
        let clear_entire_session = matches!(&event.kind, EventKind::SessionAttentionResolved)
            && !has_subject
            && session_resolution_is_authorised(event);
        let cleared = if clear_entire_session {
            self.queue.resolve_session(&event.session_id)
        } else if matches!(&event.kind, EventKind::SessionAttentionResolved) && !has_subject {
            // A PTY heuristic may withdraw its own scoped guess, but an
            // anonymous disappearance is not authority to empty a Session.
            0
        } else if matches!(&event.kind, EventKind::AgentIdle) {
            self.queue.resolve_interaction_subject_at_most(
                &event.session_id,
                event.node_id.as_ref(),
                event.parent_node_id.as_ref(),
                event.agent.external_id.as_deref(),
                event.confidence,
            )
        } else {
            self.queue.resolve_subject_at_most(
                &event.session_id,
                event.node_id.as_ref(),
                event.parent_node_id.as_ref(),
                event.agent.external_id.as_deref(),
                event.confidence,
            )
        };
        self.deferred.retain(|deferred| {
            if clear_entire_session {
                return deferred.session_id != event.session_id;
            }
            (matches!(&event.kind, EventKind::AgentIdle)
                && deferred.demand_kind != AttentionDemandKind::Interaction)
                || deferred.confidence > event.confidence
                || !subject_is_resolved(
                    SubjectRef {
                        session: &deferred.session_id,
                        node: deferred.node_id.as_ref(),
                        parent: deferred.parent_node_id.as_ref(),
                        external_id: deferred.subject_external_id.as_deref(),
                    },
                    SubjectRef {
                        session: &event.session_id,
                        node: event.node_id.as_ref(),
                        parent: event.parent_node_id.as_ref(),
                        external_id: event.agent.external_id.as_deref(),
                    },
                )
        });
        let _ = now_ms;
        Some(
            if cleared > 0
                && !self
                    .queue
                    .iter()
                    .any(|entry| entry.session_id == event.session_id)
            {
                vec![Effect::Cleared {
                    session_id: event.session_id.clone(),
                }]
            } else {
                Vec::new()
            },
        )
    }

    /// Applies terminal lifecycle ownership and correlation rules.
    fn resolve_lifecycle_event(
        &mut self,
        event: &TurnEvent,
        announce_session_clear: bool,
    ) -> Vec<Effect> {
        let removed = match event.node_id.as_ref() {
            Some(node) => self.resolve_lifecycle(
                &event.session_id,
                node,
                event.parent_node_id.as_ref(),
                event.agent.external_id.as_deref(),
            ),
            None => event.agent.external_id.as_deref().map_or(0, |external_id| {
                self.resolve_lifecycle_scope(
                    &event.session_id,
                    event.parent_node_id.as_ref(),
                    external_id,
                )
            }),
        };
        if removed > 0
            && announce_session_clear
            && !self
                .queue
                .iter()
                .any(|entry| entry.session_id == event.session_id)
        {
            vec![Effect::Cleared {
                session_id: event.session_id.clone(),
            }]
        } else {
            Vec::new()
        }
    }

    /// Replaces obsolete questions/permissions with semantic completion or
    /// failure evidence without discarding an earlier durable informational item.
    fn prepare_informational_subject(
        &mut self,
        event: &TurnEvent,
        incoming_kind: AttentionDemandKind,
    ) -> usize {
        let removed = self.queue.prepare_informational_subject(
            &event.session_id,
            event.node_id.as_ref(),
            event.parent_node_id.as_ref(),
            event.agent.external_id.as_deref(),
            incoming_kind,
        );
        self.deferred.retain(|deferred| {
            !subject_is_resolved(
                SubjectRef {
                    session: &deferred.session_id,
                    node: deferred.node_id.as_ref(),
                    parent: deferred.parent_node_id.as_ref(),
                    external_id: deferred.subject_external_id.as_deref(),
                },
                SubjectRef {
                    session: &event.session_id,
                    node: event.node_id.as_ref(),
                    parent: event.parent_node_id.as_ref(),
                    external_id: event.agent.external_id.as_deref(),
                },
            )
        });
        removed
    }
}

fn lifecycle_deferred_is_resolved(
    deferred: &DeferredFocus,
    session: &SessionId,
    node: &NodeId,
    parent: Option<&NodeId>,
    external_id: Option<&str>,
) -> bool {
    if deferred.session_id != *session {
        return false;
    }
    if deferred.node_id.as_ref() == Some(node) {
        return true;
    }
    deferred.node_id.is_none()
        && external_id.is_some()
        && deferred.parent_node_id.as_ref() == parent
        && deferred.subject_external_id.as_deref() == external_id
}

fn session_resolution_is_authorised(event: &TurnEvent) -> bool {
    matches!(event.source, EventSource::UserCorrection) && event.confidence == Confidence::Explicit
}

fn deferred_matches_entry(deferred: &DeferredFocus, entry: &AttentionEntry) -> bool {
    deferred.session_id == entry.session_id
        && deferred.node_id == entry.node_id
        && deferred.parent_node_id == entry.parent_node_id
        && deferred.subject_external_id == entry.subject_external_id
}

fn entry_matches_event_subject(entry: &AttentionEntry, event: &TurnEvent) -> bool {
    if entry.session_id != event.session_id {
        return false;
    }
    match event.node_id.as_ref() {
        Some(node) => entry.node_id.as_ref() == Some(node),
        None => {
            entry.node_id.is_none()
                && entry.parent_node_id.as_ref() == event.parent_node_id.as_ref()
                && entry.subject_external_id.as_deref() == event.agent.external_id.as_deref()
        }
    }
}

fn action_is_perceptible_non_focus(action: Action) -> bool {
    matches!(
        action,
        Action::Badge | Action::Highlight | Action::Sound | Action::Notify | Action::Custom
    )
}

fn demand_survives_owner_exit(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::AgentTurnCompleted { .. }
            | EventKind::AgentTaskCompleted { .. }
            | EventKind::AgentFailed { .. }
            | EventKind::ProcessFailed { .. }
    )
}

fn attention_demand_kind(kind: &EventKind) -> Option<AttentionDemandKind> {
    match kind {
        EventKind::AgentTurnCompleted { .. } => Some(AttentionDemandKind::TurnCompleted),
        EventKind::AgentTaskCompleted { .. } => Some(AttentionDemandKind::TaskCompleted),
        EventKind::AgentFailed { .. } => Some(AttentionDemandKind::AgentFailed),
        EventKind::ProcessFailed { .. } => Some(AttentionDemandKind::ProcessFailed),
        _ => None,
    }
}

/// Whether an effect is something the user can see or hear.
///
/// Bookkeeping effects — enqueued, deferred, denied, cleared — are invisible, so
/// they must not start the session cooldown.
fn is_perceptible(effect: &Effect) -> bool {
    matches!(
        effect,
        Effect::Badge { .. }
            | Effect::Highlight { .. }
            | Effect::PlaySound { .. }
            | Effect::Notify { .. }
            | Effect::Focus { .. }
            | Effect::RunCustom { .. }
    )
}

/// A one-line summary for the queue panel.
fn summarise(kind: &EventKind) -> Option<String> {
    match kind {
        EventKind::AgentPermissionRequired { summary, .. } => Some(summary.clone()),
        EventKind::AgentQuestionAsked { question } => Some(question.clone()),
        EventKind::AgentWaitingForUser { summary, .. } => summary.clone(),
        EventKind::AgentFailed { reason } => Some(reason.clone()),
        EventKind::AgentTaskCompleted { summary } => summary.clone(),
        _ => None,
    }
}

/// Queue semantics are policy semantics, not the same thing as an agent saying it
/// is blocked. A completed turn, completed task or failure can be configured to
/// enter the queue even though none of them is an `AwaitingUser` turn state.
fn queue_reason(kind: &EventKind) -> Option<crate::state::AwaitingReason> {
    use crate::state::AwaitingReason;
    match kind {
        EventKind::AgentPermissionRequired { .. } => Some(AwaitingReason::Permission),
        EventKind::AgentQuestionAsked { .. } => Some(AwaitingReason::Question),
        EventKind::AgentWaitingForUser { reason, .. }
        | EventKind::SessionNeedsAttention { reason } => Some(*reason),
        EventKind::AgentTurnCompleted { .. }
        | EventKind::AgentTaskCompleted { .. }
        | EventKind::AgentFailed { .. }
        | EventKind::ProcessFailed { .. } => Some(AwaitingReason::Input),
        _ => None,
    }
}

/// Title and body for an OS notification.
fn notification_text(kind: &EventKind) -> (String, String) {
    match kind {
        EventKind::AgentPermissionRequired { summary, .. } => {
            ("Permission needed".to_string(), summary.clone())
        }
        EventKind::AgentQuestionAsked { question } => ("Your turn".to_string(), question.clone()),
        // The count matters here: "turn complete" reads as finished, so when work
        // is still running the notification has to say so.
        EventKind::AgentTurnCompleted {
            last_message,
            background_tasks,
        } => {
            let title = if *background_tasks > 0 {
                format!("Turn complete · {background_tasks} still running")
            } else {
                "Turn complete".to_string()
            };
            (title, last_message.clone().unwrap_or_default())
        }
        EventKind::AgentTaskCompleted { summary } => (
            "Task complete".to_string(),
            summary.clone().unwrap_or_default(),
        ),
        EventKind::AgentFailed { reason } => ("Agent failed".to_string(), reason.clone()),
        EventKind::AgentWaitingForUser { summary, .. } => {
            ("Your turn".to_string(), summary.clone().unwrap_or_default())
        }
        other => (crate::event::event_name(other), String::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{AgentRef, Confidence, EventSource, Risk};

    const T0: i64 = 1_700_000_000_000;

    fn sess(name: &str) -> SessionId {
        SessionId::from_stored(name)
    }

    fn hook_event(session: &str, kind: EventKind) -> TurnEvent {
        TurnEvent::new(
            sess(session),
            kind,
            EventSource::Hook {
                tool: "claude-code".into(),
                event_name: "test".into(),
            },
            Confidence::Explicit,
            T0,
        )
    }

    fn permission(session: &str) -> TurnEvent {
        hook_event(
            session,
            EventKind::AgentPermissionRequired {
                summary: "run make verify".into(),
                command: Some("make verify".into()),
                tool_name: Some("Bash".into()),
                risk: Risk::Medium,
            },
        )
    }

    fn ctx() -> UserContext {
        UserContext {
            last_keystroke_ms: None,
            app_foreground: true,
            active_session: Some(sess("sess_elsewhere")),
            sensitive_operation: false,
        }
    }

    #[test]
    fn a_permission_request_enqueues_and_focuses() {
        let mut m = AttentionManager::new();
        let effects = m.ingest(
            &permission("sess_a"),
            &AttentionPolicy::default(),
            &ctx(),
            T0,
        );

        assert!(matches!(effects.first(), Some(Effect::Enqueued { .. })));
        assert!(
            effects.iter().any(|e| matches!(e, Effect::Focus { .. })),
            "an explicit permission earns focus: {effects:?}"
        );
        assert_eq!(m.queue().len(), 1);
    }

    #[test]
    fn a_session_configured_not_to_focus_only_badges() {
        let mut m = AttentionManager::new();
        let effects = m.ingest(
            &permission("sess_a"),
            &AttentionPolicy::silent(),
            &ctx(),
            T0,
        );
        assert!(!effects.iter().any(|e| matches!(e, Effect::Focus { .. })));
        assert!(effects.iter().any(|e| matches!(e, Effect::Badge { .. })));
    }

    /// Case B: focus is deferred while typing, then delivered by `tick`.
    #[test]
    fn focus_waits_for_the_user_to_stop_typing_then_happens() {
        let mut m = AttentionManager::new();
        let typing = UserContext {
            last_keystroke_ms: Some(T0),
            ..ctx()
        };
        let effects = m.ingest(
            &permission("sess_a"),
            &AttentionPolicy::default(),
            &typing,
            T0,
        );
        assert!(effects.iter().any(|e| matches!(
            e,
            Effect::FocusDeferred {
                reason: DeferReason::UserTyping,
                ..
            }
        )));
        assert_eq!(m.deferred_count(), 1);

        // Still typing a moment later: nothing yet.
        assert!(m.tick(&typing, T0 + 500).is_empty());

        // Hands off the keyboard: the jump lands.
        let idle = UserContext {
            last_keystroke_ms: Some(T0),
            ..ctx()
        };
        let later = m.tick(&idle, T0 + 2_000);
        assert!(
            later.iter().any(|e| matches!(e, Effect::Focus { .. })),
            "the deferred jump must eventually happen: {later:?}"
        );
        assert_eq!(m.deferred_count(), 0);
    }

    #[test]
    fn focus_only_policy_keeps_its_deferred_request_without_a_queue_entry() {
        let mut m = AttentionManager::new();
        let mut policy = AttentionPolicy::silent();
        policy.on_permission_required = vec![Action::FocusIfIdle];
        let typing = UserContext {
            last_keystroke_ms: Some(T0),
            ..ctx()
        };

        let effects = m.ingest(&permission("sess_a"), &policy, &typing, T0);
        assert!(effects
            .iter()
            .any(|effect| matches!(effect, Effect::FocusDeferred { .. })));
        assert!(m.queue().is_empty());
        assert_eq!(m.deferred_count(), 1);

        let effects = m.tick(&ctx(), T0 + 2_000);
        assert!(effects
            .iter()
            .any(|effect| matches!(effect, Effect::Focus { .. })));
        assert_eq!(m.deferred_count(), 0);
    }

    #[test]
    fn acknowledging_a_demand_cancels_its_deferred_focus() {
        let mut m = AttentionManager::new();
        let typing = UserContext {
            last_keystroke_ms: Some(T0),
            ..ctx()
        };
        m.ingest(
            &permission("sess_a"),
            &AttentionPolicy::default(),
            &typing,
            T0,
        );
        let id = m.queue().iter().next().unwrap().id.clone();
        assert_eq!(m.deferred_count(), 1);

        assert!(m.acknowledge(&id));
        assert_eq!(m.deferred_count(), 0);
        assert!(!m
            .tick(&ctx(), T0 + 2_000)
            .iter()
            .any(|effect| matches!(effect, Effect::Focus { .. })));
    }

    #[test]
    fn a_stale_deferred_jump_is_dropped_rather_than_fired_late() {
        let mut m = AttentionManager::new();
        let typing = UserContext {
            last_keystroke_ms: Some(T0),
            ..ctx()
        };
        m.ingest(
            &permission("sess_a"),
            &AttentionPolicy::default(),
            &typing,
            T0,
        );
        assert_eq!(m.deferred_count(), 1);

        let effects = m.tick(&ctx(), T0 + DEFERRED_FOCUS_TTL_MS + 1);
        assert!(effects.is_empty(), "no late ambush: {effects:?}");
        assert_eq!(m.deferred_count(), 0);
    }

    #[test]
    fn a_dismissed_or_snoozed_deferred_focus_cannot_borrow_a_siblings_demand() {
        for snooze in [false, true] {
            let mut m = AttentionManager::new();
            let typing = UserContext {
                last_keystroke_ms: Some(T0),
                ..ctx()
            };
            let first = NodeId::from_stored("agent_a");
            let second = NodeId::from_stored("agent_b");
            m.ingest(
                &permission("sess_a").with_node(first.clone()),
                &AttentionPolicy::default(),
                &typing,
                T0,
            );
            m.ingest(
                &permission("sess_a").with_node(second.clone()),
                &AttentionPolicy::default(),
                &typing,
                T0 + 1,
            );
            let first_id = m
                .queue()
                .iter()
                .find(|entry| entry.node_id.as_ref() == Some(&first))
                .unwrap()
                .id
                .clone();
            if snooze {
                assert!(m.snooze(&first_id, T0 + 60_000));
            } else {
                assert!(m.dismiss(&first_id));
            }
            assert_eq!(m.deferred_count(), 1);

            let effects = m.tick(&ctx(), T0 + 2_000);
            assert!(effects.iter().any(|effect| matches!(
                effect,
                Effect::Focus {
                    node_id: Some(node),
                    ..
                } if node == &second
            )));
            assert!(!effects.iter().any(|effect| matches!(
                effect,
                Effect::Focus {
                    node_id: Some(node),
                    ..
                } if node == &first
            )));
        }
    }

    /// Case A end to end: three agents block at once, the user walks the queue.
    #[test]
    fn three_simultaneous_demands_are_walked_one_at_a_time() {
        let mut m = AttentionManager::new();
        let policy = AttentionPolicy::default();
        for name in ["sess_a", "sess_b", "sess_c"] {
            m.ingest(&permission(name), &policy, &ctx(), T0);
        }
        assert_eq!(m.queue().len(), 3);

        let mut visited = Vec::new();
        for _ in 0..3 {
            match m.goto_next(T0) {
                Some(Effect::Focus { session_id, .. }) => {
                    visited.push(session_id.as_str().to_string());
                    // Visiting means engaging, which clears that session.
                    m.engage_session(&sess(visited.last().unwrap()), T0);
                }
                other => panic!("expected a focus effect, got {other:?}"),
            }
        }
        visited.sort();
        assert_eq!(visited, vec!["sess_a", "sess_b", "sess_c"]);
        assert!(m.queue().is_empty(), "the queue drains as it is walked");
    }

    #[test]
    fn the_focus_rate_limit_holds_under_a_burst_of_explicit_events() {
        let mut m = AttentionManager::new();
        let policy = AttentionPolicy {
            cooldown_seconds: 0,
            ..AttentionPolicy::default()
        };
        let mut focus_effects = 0;
        for i in 0..10 {
            let name = format!("sess_{i:08}");
            let now = T0 + i as i64 * 2_100;
            let effects = m.ingest(&permission(&name), &policy, &ctx(), now);
            focus_effects += effects
                .iter()
                .filter(|e| matches!(e, Effect::Focus { .. }))
                .count();
        }
        assert!(
            focus_effects <= 6,
            "ten simultaneous agents produced {focus_effects} focus jumps"
        );
        assert_eq!(m.queue().len(), 10, "but every demand is still reachable");
    }

    #[test]
    fn simultaneous_demands_never_become_a_timed_focus_cascade() {
        let mut m = AttentionManager::new();
        let policy = AttentionPolicy::default();
        let mut immediate_focus = 0;
        for session in ["sess_a", "sess_b", "sess_c"] {
            immediate_focus += m
                .ingest(&permission(session), &policy, &ctx(), T0)
                .iter()
                .filter(|effect| matches!(effect, Effect::Focus { .. }))
                .count();
        }

        assert_eq!(immediate_focus, 1);
        assert_eq!(m.queue().len(), 3, "every demand remains in Next Attention");
        assert_eq!(
            m.deferred_count(),
            0,
            "rate guards must not schedule a cascade"
        );
        assert!(!m
            .tick(&ctx(), T0 + 2_000)
            .into_iter()
            .chain(m.tick(&ctx(), T0 + 4_000))
            .any(|effect| matches!(effect, Effect::Focus { .. })));
    }

    #[test]
    fn a_muted_session_keeps_evidence_but_suppresses_interruptions() {
        let mut m = AttentionManager::new();
        m.mute_session(&sess("sess_a"), T0 + 60_000);
        let effects = m.ingest(
            &permission("sess_a"),
            &AttentionPolicy::default(),
            &ctx(),
            T0,
        );
        assert!(effects
            .iter()
            .any(|effect| matches!(effect, Effect::Enqueued { .. })));
        assert!(effects
            .iter()
            .any(|effect| matches!(effect, Effect::Badge { count: 1, .. })));
        assert!(!effects.iter().any(|effect| matches!(
            effect,
            Effect::Focus { .. }
                | Effect::FocusDeferred { .. }
                | Effect::PlaySound { .. }
                | Effect::Notify { .. }
                | Effect::Highlight { .. }
                | Effect::RunCustom { .. }
        )));
        assert_eq!(m.queue().len(), 1);

        m.unmute_session(&sess("sess_a"));
        let effects = m.ingest(
            &permission("sess_a"),
            &AttentionPolicy::default(),
            &ctx(),
            T0,
        );
        assert!(effects.iter().any(|e| matches!(e, Effect::Enqueued { .. })));
    }

    #[test]
    fn a_muted_badge_only_policy_still_emits_its_non_interruptive_evidence() {
        let mut m = AttentionManager::new();
        m.mute_session(&sess("sess_a"), T0 + 60_000);

        let effects = m.ingest(
            &permission("sess_a"),
            &AttentionPolicy::silent(),
            &ctx(),
            T0,
        );

        assert_eq!(effects.len(), 1);
        assert!(matches!(effects[0], Effect::Badge { count: 1, .. }));
        assert!(m.queue().is_empty(), "the policy did not request Enqueue");
    }

    #[test]
    fn muted_badge_and_enqueue_policy_emits_one_badge_not_two() {
        let mut m = AttentionManager::new();
        m.mute_session(&sess("sess_a"), T0 + 60_000);
        let failed = hook_event(
            "sess_a",
            EventKind::AgentFailed {
                reason: "boom".into(),
            },
        );

        let effects = m.ingest(&failed, &AttentionPolicy::default(), &ctx(), T0);
        assert_eq!(
            effects
                .iter()
                .filter(|effect| matches!(effect, Effect::Badge { .. }))
                .count(),
            1
        );
        assert_eq!(m.queue().len(), 1);
    }

    #[test]
    fn repeated_callbacks_inside_cooldown_do_not_burst_or_duplicate_deferrals() {
        let mut m = AttentionManager::new();
        let policy = AttentionPolicy {
            on_permission_required: vec![
                Action::Enqueue,
                Action::Badge,
                Action::Sound,
                Action::Notify,
                Action::FocusIfIdle,
            ],
            ..AttentionPolicy::default()
        };
        let typing = UserContext {
            last_keystroke_ms: Some(T0),
            ..ctx()
        };

        let first = m.ingest(&permission("sess_a"), &policy, &typing, T0);
        assert!(first
            .iter()
            .any(|effect| matches!(effect, Effect::Badge { .. })));
        assert!(first
            .iter()
            .any(|effect| matches!(effect, Effect::PlaySound { .. })));
        assert!(first
            .iter()
            .any(|effect| matches!(effect, Effect::Notify { .. })));
        assert_eq!(m.deferred_count(), 1);

        for offset in 1..=20 {
            let replay = m.ingest(&permission("sess_a"), &policy, &typing, T0 + offset);
            assert!(!replay.iter().any(|effect| matches!(
                effect,
                Effect::Badge { .. }
                    | Effect::PlaySound { .. }
                    | Effect::Notify { .. }
                    | Effect::FocusDeferred { .. }
                    | Effect::Focus { .. }
            )));
        }
        assert_eq!(m.queue().len(), 1);
        assert_eq!(m.deferred_count(), 1);
    }

    #[test]
    fn muting_cancels_deferred_focus_without_discarding_the_demand() {
        let mut m = AttentionManager::new();
        let typing = UserContext {
            last_keystroke_ms: Some(T0),
            ..ctx()
        };
        m.ingest(
            &permission("sess_a"),
            &AttentionPolicy::default(),
            &typing,
            T0,
        );
        assert_eq!(m.deferred_count(), 1);
        assert_eq!(m.queue().len(), 1);

        m.mute_session(&sess("sess_a"), T0 + 60_000);
        assert_eq!(m.deferred_count(), 0);
        let effects = m.tick(&ctx(), T0 + 2_000);
        assert!(!effects
            .iter()
            .any(|effect| matches!(effect, Effect::Focus { .. })));
        assert_eq!(m.queue().len(), 1);
    }

    #[test]
    fn answering_the_agent_clears_the_demand() {
        let mut m = AttentionManager::new();
        let node = NodeId::from_stored("agent_a");
        m.ingest(
            &permission("sess_a").with_node(node.clone()),
            &AttentionPolicy::default(),
            &ctx(),
            T0,
        );
        assert_eq!(m.queue().len(), 1);

        let resumed = hook_event(
            "sess_a",
            EventKind::AgentTurnStarted {
                prompt_excerpt: Some("yes".into()),
            },
        )
        .with_node(node);
        let effects = m.ingest(&resumed, &AttentionPolicy::default(), &ctx(), T0 + 1_000);
        assert!(effects.iter().any(|e| matches!(e, Effect::Cleared { .. })));
        assert!(m.queue().is_empty());
    }

    #[test]
    fn a_heuristic_withdrawal_is_scoped_and_cannot_erase_stronger_evidence() {
        let mut m = AttentionManager::new();
        let guessed_node = NodeId::from_stored("agent_a");
        let sibling = NodeId::from_stored("agent_b");
        let guessed = TurnEvent::new(
            sess("sess_a"),
            EventKind::AgentWaitingForUser {
                reason: crate::state::AwaitingReason::Input,
                summary: Some("possible prompt".into()),
            },
            EventSource::PtyHeuristic {
                rule: "prompt".into(),
            },
            Confidence::InferredHigh,
            T0,
        )
        .with_node(guessed_node.clone());
        m.ingest(&guessed, &AttentionPolicy::default(), &ctx(), T0);
        m.ingest(
            &permission("sess_a").with_node(guessed_node.clone()),
            &AttentionPolicy::default(),
            &ctx(),
            T0 + 1,
        );
        m.ingest(
            &permission("sess_a").with_node(sibling.clone()),
            &AttentionPolicy::default(),
            &ctx(),
            T0 + 2,
        );
        assert_eq!(m.queue().len(), 3);

        let withdrawal = TurnEvent::new(
            sess("sess_a"),
            EventKind::SessionAttentionResolved,
            EventSource::PtyHeuristic {
                rule: "prompt".into(),
            },
            Confidence::InferredHigh,
            T0 + 3,
        )
        .with_node(guessed_node.clone());
        m.ingest(&withdrawal, &AttentionPolicy::default(), &ctx(), T0 + 3);

        let remaining: Vec<_> = m.queue().iter().collect();
        assert_eq!(
            remaining.len(),
            2,
            "only the provisional guess is withdrawn"
        );
        assert!(remaining
            .iter()
            .all(|entry| entry.confidence == Confidence::Explicit));
        assert!(remaining
            .iter()
            .any(|entry| entry.node_id.as_ref() == Some(&guessed_node)));
        assert!(remaining
            .iter()
            .any(|entry| entry.node_id.as_ref() == Some(&sibling)));

        let anonymous_withdrawal = TurnEvent::new(
            sess("sess_a"),
            EventKind::SessionAttentionResolved,
            EventSource::PtyHeuristic {
                rule: "prompt".into(),
            },
            Confidence::InferredHigh,
            T0 + 4,
        );
        m.ingest(
            &anonymous_withdrawal,
            &AttentionPolicy::default(),
            &ctx(),
            T0 + 4,
        );
        assert_eq!(
            m.queue().len(),
            2,
            "a heuristic has no session-wide authority"
        );

        let anonymous_hook = hook_event("sess_a", EventKind::SessionAttentionResolved);
        m.ingest(&anonymous_hook, &AttentionPolicy::default(), &ctx(), T0 + 5);
        assert_eq!(
            m.queue().len(),
            2,
            "an ambiguous hook callback cannot clear sibling demands"
        );

        let user_resolution = TurnEvent::new(
            sess("sess_a"),
            EventKind::SessionAttentionResolved,
            EventSource::UserCorrection,
            Confidence::Explicit,
            T0 + 6,
        );
        m.ingest(
            &user_resolution,
            &AttentionPolicy::default(),
            &ctx(),
            T0 + 6,
        );
        assert!(
            m.queue().is_empty(),
            "the explicit user action remains authoritative"
        );
    }

    #[test]
    fn an_unassigned_resume_only_clears_its_parent_scope_and_deferral() {
        let mut m = AttentionManager::new();
        let parent_a = NodeId::from_stored("parent_a");
        let parent_b = NodeId::from_stored("parent_b");
        let reviewer = NodeId::from_stored("reviewer");
        let tests = NodeId::from_stored("tests");

        // B lands first and gets the focus grant. The user then starts typing,
        // which legitimately defers the other three and lets this test prove a
        // resolution cancels only its own scope.
        m.ingest(
            &permission("sess_a").with_parent(parent_b.clone()),
            &AttentionPolicy::default(),
            &ctx(),
            T0,
        );
        let typing = UserContext {
            last_keystroke_ms: Some(T0),
            ..ctx()
        };
        m.ingest(
            &permission("sess_a").with_parent(parent_a.clone()),
            &AttentionPolicy::default(),
            &typing,
            T0 + 1,
        );
        m.ingest(
            &permission("sess_a").with_node(reviewer.clone()),
            &AttentionPolicy::default(),
            &typing,
            T0 + 2,
        );
        m.ingest(
            &permission("sess_a").with_node(tests.clone()),
            &AttentionPolicy::default(),
            &typing,
            T0 + 3,
        );
        assert_eq!(m.queue().len(), 4);
        assert_eq!(
            m.deferred_count(),
            3,
            "A and the two exact siblings wait behind B's focus grant"
        );

        let resumed = hook_event(
            "sess_a",
            EventKind::AgentTurnStarted {
                prompt_excerpt: Some("continue".into()),
            },
        )
        .with_parent(parent_a.clone());
        m.ingest(&resumed, &AttentionPolicy::default(), &ctx(), T0 + 4);

        let remaining: Vec<_> = m.queue().iter().collect();
        assert_eq!(remaining.len(), 3);
        assert!(remaining.iter().any(|entry| {
            entry.node_id.is_none() && entry.parent_node_id.as_ref() == Some(&parent_b)
        }));
        assert!(remaining
            .iter()
            .any(|entry| entry.node_id.as_ref() == Some(&reviewer)));
        assert!(remaining
            .iter()
            .any(|entry| entry.node_id.as_ref() == Some(&tests)));
        assert_eq!(
            m.deferred_count(),
            2,
            "only A's deferred focus is cancelled"
        );
    }

    #[test]
    fn a_dead_process_clears_its_exact_demand() {
        let mut m = AttentionManager::new();
        let node = NodeId::from_stored("proc_a");
        let mut event = permission("sess_a");
        event = event.with_node(node.clone());
        m.ingest(&event, &AttentionPolicy::default(), &ctx(), T0);
        assert_eq!(m.queue().len(), 1);

        let exited =
            hook_event("sess_a", EventKind::ProcessExited { code: 0 }).with_node(node.clone());
        let effects = m.ingest(&exited, &AttentionPolicy::default(), &ctx(), T0 + 500);
        assert!(effects.iter().any(|e| matches!(e, Effect::Cleared { .. })));
        assert!(
            m.queue().is_empty(),
            "a demand from a dead process must not linger"
        );
    }

    #[test]
    fn terminal_owner_events_clear_node_less_scopes_and_deferred_focus() {
        for kind in [
            EventKind::ProcessExited { code: 0 },
            EventKind::AgentSubagentStopped {
                agent_id: Some("reviewer".into()),
            },
        ] {
            let mut m = AttentionManager::new();
            let parent = NodeId::from_stored("parent_a");
            let typing = UserContext {
                last_keystroke_ms: Some(T0),
                ..ctx()
            };
            let demand = permission("sess_a")
                .with_parent(parent.clone())
                .with_agent(AgentRef {
                    external_id: Some("reviewer".into()),
                    ..AgentRef::default()
                });
            m.ingest(&demand, &AttentionPolicy::default(), &typing, T0);
            assert_eq!(m.queue().len(), 1);
            assert_eq!(m.deferred_count(), 1);

            let terminal = hook_event("sess_a", kind.clone()).with_node(parent.clone());
            let effects = m.ingest(&terminal, &AttentionPolicy::default(), &typing, T0 + 1);
            assert!(effects
                .iter()
                .any(|effect| matches!(effect, Effect::Cleared { .. })));
            assert!(m.queue().is_empty());
            assert_eq!(m.deferred_count(), 0);
        }
    }

    #[test]
    fn a_dead_parent_preserves_exact_children_and_other_parent_scopes() {
        let mut m = AttentionManager::new();
        let parent = NodeId::from_stored("parent_a");
        let other_parent = NodeId::from_stored("parent_b");
        let child = NodeId::from_stored("reviewer");
        let typing = UserContext {
            last_keystroke_ms: Some(T0),
            ..ctx()
        };
        m.ingest(
            &permission("sess_a").with_parent(parent.clone()),
            &AttentionPolicy::default(),
            &typing,
            T0,
        );
        m.ingest(
            &permission("sess_a")
                .with_node(child.clone())
                .with_parent(parent.clone()),
            &AttentionPolicy::default(),
            &typing,
            T0 + 1,
        );
        m.ingest(
            &permission("sess_a").with_parent(other_parent.clone()),
            &AttentionPolicy::default(),
            &typing,
            T0 + 2,
        );
        assert_eq!(m.queue().len(), 3);
        assert_eq!(m.deferred_count(), 3);

        let exited =
            hook_event("sess_a", EventKind::ProcessExited { code: 0 }).with_node(parent.clone());
        let effects = m.ingest(&exited, &AttentionPolicy::default(), &typing, T0 + 3);

        let remaining: Vec<_> = m.queue().iter().collect();
        assert_eq!(remaining.len(), 2);
        assert!(remaining
            .iter()
            .any(|entry| entry.node_id.as_ref() == Some(&child)));
        assert!(remaining.iter().any(|entry| {
            entry.node_id.is_none() && entry.parent_node_id.as_ref() == Some(&other_parent)
        }));
        assert_eq!(m.deferred_count(), 2);
        assert!(!effects
            .iter()
            .any(|effect| matches!(effect, Effect::Cleared { .. })));
    }

    #[test]
    fn subagent_stop_clears_the_exact_child_and_its_predeclaration_scope() {
        let mut m = AttentionManager::new();
        let parent = NodeId::from_stored("claude");
        let reviewer = NodeId::from_stored("reviewer");
        let typing = UserContext {
            last_keystroke_ms: Some(T0),
            ..ctx()
        };
        let agent = AgentRef {
            external_id: Some("worker-reviewer".into()),
            ..AgentRef::default()
        };
        m.ingest(
            &permission("sess_a")
                .with_parent(parent.clone())
                .with_agent(agent.clone()),
            &AttentionPolicy::default(),
            &typing,
            T0,
        );

        let stopped = hook_event(
            "sess_a",
            EventKind::AgentSubagentStopped {
                agent_id: Some("worker-reviewer".into()),
            },
        )
        .with_node(reviewer)
        .with_parent(parent)
        .with_agent(agent);
        m.ingest(&stopped, &AttentionPolicy::default(), &typing, T0 + 1);
        assert!(m.queue().is_empty());
        assert_eq!(m.deferred_count(), 0);
    }

    #[test]
    fn process_failure_replaces_old_owner_attention_with_a_failure_demand() {
        let mut m = AttentionManager::new();
        let parent = NodeId::from_stored("claude");
        let typing = UserContext {
            last_keystroke_ms: Some(T0),
            ..ctx()
        };
        m.ingest(
            &permission("sess_a").with_parent(parent.clone()),
            &AttentionPolicy::default(),
            &typing,
            T0,
        );
        assert_eq!(m.deferred_count(), 1);

        let failed = hook_event(
            "sess_a",
            EventKind::ProcessFailed {
                code: Some(1),
                signal: None,
            },
        )
        .with_node(parent.clone());
        let effects = m.ingest(&failed, &AttentionPolicy::default(), &typing, T0 + 1);

        assert!(!effects
            .iter()
            .any(|effect| matches!(effect, Effect::Cleared { .. })));
        assert!(effects
            .iter()
            .any(|effect| matches!(effect, Effect::Enqueued { .. })));
        assert_eq!(m.deferred_count(), 0);
        let remaining: Vec<_> = m.queue().iter().collect();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].node_id.as_ref(), Some(&parent));
        assert_eq!(remaining[0].parent_node_id, None);
        assert_eq!(remaining[0].reason, crate::state::AwaitingReason::Input);
        assert!(remaining[0].survives_owner_exit);
        assert_eq!(remaining[0].demand_kind, AttentionDemandKind::ProcessFailed);
    }

    #[test]
    fn semantic_failure_replaces_blocking_attention_and_survives_owner_exit() {
        let mut m = AttentionManager::new();
        let node = NodeId::from_stored("claude");
        m.ingest(
            &permission("sess_a").with_node(node.clone()),
            &AttentionPolicy::default(),
            &ctx(),
            T0,
        );
        let failed = hook_event(
            "sess_a",
            EventKind::AgentFailed {
                reason: "runtime protocol failed".into(),
            },
        )
        .with_node(node.clone());
        m.ingest(&failed, &AttentionPolicy::default(), &ctx(), T0 + 1);

        let queued = m.queue().iter().next().expect("failure queued");
        assert_eq!(m.queue().len(), 1);
        assert_eq!(queued.demand_kind, AttentionDemandKind::AgentFailed);
        assert!(queued.survives_owner_exit);

        let exited = hook_event("sess_a", EventKind::ProcessExited { code: 1 }).with_node(node);
        m.ingest(&exited, &AttentionPolicy::default(), &ctx(), T0 + 2);
        assert_eq!(m.queue().len(), 1, "post-mortem evidence remains");
    }

    #[test]
    fn process_failure_supersedes_task_completion_instead_of_looking_like_a_replay() {
        let mut m = AttentionManager::new();
        let node = NodeId::from_stored("claude");
        let completed = hook_event(
            "sess_a",
            EventKind::AgentTaskCompleted {
                summary: Some("all done".into()),
            },
        )
        .with_node(node.clone());
        m.ingest(&completed, &AttentionPolicy::default(), &ctx(), T0);
        let completed_id = m.queue().iter().next().unwrap().id.clone();

        let failed = hook_event(
            "sess_a",
            EventKind::ProcessFailed {
                code: Some(1),
                signal: None,
            },
        )
        .with_node(node);
        m.ingest(&failed, &AttentionPolicy::default(), &ctx(), T0 + 1);

        let queued = m.queue().iter().next().unwrap();
        assert_eq!(m.queue().len(), 1);
        assert_ne!(queued.id, completed_id);
        assert_eq!(queued.demand_kind, AttentionDemandKind::ProcessFailed);
        assert_eq!(queued.summary, None);
    }

    #[test]
    fn a_new_interaction_does_not_inherit_postmortem_lifetime() {
        let mut m = AttentionManager::new();
        let node = NodeId::from_stored("claude");
        let completed = hook_event(
            "sess_a",
            EventKind::AgentTurnCompleted {
                last_message: Some("done".into()),
                background_tasks: 0,
            },
        )
        .with_node(node.clone());
        m.ingest(&completed, &AttentionPolicy::default(), &ctx(), T0);
        assert!(m.queue().iter().next().unwrap().survives_owner_exit);

        let waiting = hook_event(
            "sess_a",
            EventKind::AgentWaitingForUser {
                reason: crate::state::AwaitingReason::Input,
                summary: Some("new input needed".into()),
            },
        )
        .with_node(node.clone());
        m.ingest(&waiting, &AttentionPolicy::default(), &ctx(), T0 + 1);
        let current = m.queue().iter().next().unwrap();
        assert_eq!(m.queue().len(), 1);
        assert_eq!(current.demand_kind, AttentionDemandKind::Interaction);
        assert!(!current.survives_owner_exit);

        let exited = hook_event("sess_a", EventKind::ProcessExited { code: 0 }).with_node(node);
        m.ingest(&exited, &AttentionPolicy::default(), &ctx(), T0 + 2);
        assert!(
            m.queue().is_empty(),
            "a live prompt cannot outlive its owner"
        );
    }

    #[test]
    fn runtime_idle_and_exit_do_not_erase_completed_turn_evidence() {
        let mut m = AttentionManager::new();
        let node = NodeId::from_stored("claude");
        let completed = hook_event(
            "sess_a",
            EventKind::AgentTurnCompleted {
                last_message: Some("review the result".into()),
                background_tasks: 0,
            },
        )
        .with_node(node.clone());
        m.ingest(&completed, &AttentionPolicy::default(), &ctx(), T0);
        let id = m.queue().iter().next().unwrap().id.clone();

        let idle = hook_event("sess_a", EventKind::AgentIdle).with_node(node.clone());
        m.ingest(&idle, &AttentionPolicy::default(), &ctx(), T0 + 1);
        assert_eq!(m.queue().iter().next().unwrap().id, id);

        let exited = hook_event("sess_a", EventKind::ProcessExited { code: 0 }).with_node(node);
        m.ingest(&exited, &AttentionPolicy::default(), &ctx(), T0 + 2);
        let retained = m.queue().iter().next().expect("completion survives exit");
        assert_eq!(retained.id, id);
        assert_eq!(retained.demand_kind, AttentionDemandKind::TurnCompleted);
        assert!(retained.survives_owner_exit);
    }

    #[test]
    fn a_replayed_process_failure_preserves_attention_identity_age_and_snooze() {
        let mut m = AttentionManager::new();
        let node = NodeId::from_stored("claude");
        let failed = || {
            hook_event(
                "sess_a",
                EventKind::ProcessFailed {
                    code: Some(1),
                    signal: None,
                },
            )
            .with_node(node.clone())
        };

        let old_input = hook_event(
            "sess_a",
            EventKind::AgentWaitingForUser {
                reason: crate::state::AwaitingReason::Input,
                summary: None,
            },
        )
        .with_node(node.clone());
        m.ingest(&old_input, &AttentionPolicy::default(), &ctx(), T0);
        let old_id = m.queue().iter().next().unwrap().id.clone();
        assert!(m.snooze(&old_id, T0 + 60_000));

        m.ingest(&failed(), &AttentionPolicy::default(), &ctx(), T0 + 1);
        let first = m.queue().iter().next().unwrap().clone();
        assert_ne!(first.id, old_id, "the first failure replaces an Input wait");
        assert_eq!(first.state, EntryState::Pending);

        let _ = m.goto(&first.id, T0 + 2);
        assert_eq!(
            m.queue().get(&first.id).unwrap().state,
            EntryState::Acknowledged
        );

        m.ingest(&failed(), &AttentionPolicy::default(), &ctx(), T0 + 3);
        let replayed = m.queue().iter().next().unwrap().clone();
        assert_eq!(m.queue().len(), 1);
        assert_eq!(replayed.id, first.id);
        assert_eq!(replayed.created_ms, first.created_ms);
        assert_eq!(replayed.updated_ms, first.updated_ms);
        assert_eq!(replayed.state, EntryState::Acknowledged);

        assert!(m.snooze(&first.id, T0 + 60_000));
        let snoozed = m.queue().get(&first.id).unwrap().clone();
        let mut m = AttentionManager::from_persisted_queue(m.queue().clone());
        m.ingest(&failed(), &AttentionPolicy::default(), &ctx(), T0 + 4);
        let replayed = m.queue().iter().next().unwrap();
        assert_eq!(replayed.id, snoozed.id);
        assert_eq!(replayed.created_ms, snoozed.created_ms);
        assert_eq!(replayed.updated_ms, snoozed.updated_ms);
        assert_eq!(
            replayed.state,
            EntryState::Snoozed {
                until_ms: T0 + 60_000
            }
        );
    }

    #[test]
    fn a_guessed_permission_never_produces_a_focus_effect() {
        let mut m = AttentionManager::new();
        let guessed = TurnEvent::new(
            sess("sess_a"),
            EventKind::AgentWaitingForUser {
                reason: crate::state::AwaitingReason::Permission,
                summary: Some("looks like a prompt".into()),
            },
            EventSource::PtyHeuristic {
                rule: "permission_box".into(),
            },
            Confidence::Explicit, // requested, but the source caps it
            T0,
        );
        let effects = m.ingest(&guessed, &AttentionPolicy::default(), &ctx(), T0);
        assert!(
            !effects.iter().any(|e| matches!(e, Effect::Focus { .. })),
            "heuristics must never move the user: {effects:?}"
        );
        assert!(effects.iter().any(|e| matches!(e, Effect::Enqueued { .. })));
    }

    #[test]
    fn a_new_subagent_badges_without_moving_the_user() {
        let mut m = AttentionManager::new();
        let event = hook_event(
            "sess_a",
            EventKind::AgentSpawned {
                declared_name: None,
                agent_type: Some("Explore".into()),
                agent_id: Some("sub-1".into()),
                task: None,
            },
        );
        let effects = m.ingest(&event, &AttentionPolicy::default(), &ctx(), T0);
        assert_eq!(effects.len(), 1);
        assert!(matches!(effects[0], Effect::Badge { .. }));
    }

    #[test]
    fn a_completed_turn_enters_the_logical_queue_without_claiming_the_agent_is_blocked() {
        let mut m = AttentionManager::new();
        let event = hook_event(
            "sess_a",
            EventKind::AgentTurnCompleted {
                last_message: Some("done".into()),
                background_tasks: 0,
            },
        );
        let effects = m.ingest(&event, &AttentionPolicy::default(), &ctx(), T0);
        assert!(effects.iter().any(|e| matches!(e, Effect::Badge { .. })));
        assert!(effects.iter().any(|e| matches!(e, Effect::Enqueued { .. })));
        let queued = m
            .queue()
            .iter()
            .next()
            .expect("policy requested a queue entry");
        assert_eq!(queued.reason, crate::state::AwaitingReason::Input);
        assert_eq!(
            event.attention_reason(),
            None,
            "the agent itself is not blocked"
        );
    }

    #[test]
    fn snoozing_hides_a_demand_and_dismissing_removes_it() {
        let mut m = AttentionManager::new();
        m.ingest(
            &permission("sess_a"),
            &AttentionPolicy::default(),
            &ctx(),
            T0,
        );
        let id = m.queue().iter().next().unwrap().id.clone();

        assert!(m.snooze(&id, T0 + 30_000));
        assert!(m.next_attention(T0).is_none());
        assert!(m.next_attention(T0 + 30_000).is_some());

        assert!(m.dismiss(&id));
        assert!(m.queue().is_empty());
    }

    #[test]
    fn events_with_no_attention_meaning_produce_nothing() {
        let mut m = AttentionManager::new();
        let idle = hook_event("sess_a", EventKind::AgentIdle);
        assert!(m
            .ingest(&idle, &AttentionPolicy::default(), &ctx(), T0)
            .is_empty());
    }

    #[test]
    fn walking_the_queue_manually_is_not_rate_limited() {
        let mut m = AttentionManager::new();
        let policy = AttentionPolicy {
            cooldown_seconds: 0,
            ..AttentionPolicy::default()
        };
        for i in 0..5 {
            m.ingest(&permission(&format!("sess_{i:08}")), &policy, &ctx(), T0);
        }
        // The user presses the shortcut repeatedly; every press must move them.
        for _ in 0..5 {
            let effect = m.goto_next(T0);
            assert!(
                matches!(effect, Some(Effect::Focus { .. })),
                "manual navigation is consent and must always work"
            );
            if let Some(Effect::Focus { session_id, .. }) = effect {
                m.engage_session(&session_id, T0);
            }
        }
    }
}
