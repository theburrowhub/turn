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
    subject_is_resolved, AttentionEntry, AttentionQueue, EntryState, SubjectRef,
};
use crate::event::{EventKind, TurnEvent};
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
    action: Action,
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
    action: Action,
    policy: &'a AttentionPolicy,
}

/// Per-session bookkeeping the manager keeps on top of the policy.
#[derive(Debug, Clone, Default)]
struct SessionRuntime {
    /// Last time any effect fired, for the cooldown.
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
        // Some events end a demand rather than raise one.
        if let Some(effects) = self.handle_resolving_event(event, now_ms) {
            return effects;
        }

        let Some(trigger) = Trigger::from_event(&event.kind) else {
            return Vec::new();
        };

        let session = event.session_id.clone();

        // A muted session accrues nothing but a badge, so the sidebar still
        // shows something happened.
        if self.is_muted(&session, now_ms) {
            let count = self.queue.count_for_session(&session, now_ms);
            return vec![Effect::Badge {
                session_id: session,
                count,
            }];
        }

        let actions = policy.resolve(trigger, event.confidence);
        if actions.is_empty() {
            return Vec::new();
        }

        let mut effects = Vec::new();
        let mut enqueued_id = None;

        for action in &actions {
            match action {
                Action::Nothing => {}
                Action::Enqueue => {
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
                    let last_effect = self.runtimes.get(&session).and_then(|r| r.last_effect_ms);
                    let decision = self.governor.evaluate(
                        *focus_action,
                        &session,
                        policy,
                        ctx,
                        last_effect,
                        now_ms,
                    );
                    effects.push(self.apply_focus_decision(
                        decision,
                        FocusRequest {
                            session: &session,
                            node_id: event.node_id.clone(),
                            parent_node_id: event.parent_node_id.clone(),
                            subject_external_id: event.agent.external_id.clone(),
                            action: *focus_action,
                            policy,
                        },
                        ctx,
                        now_ms,
                    ));
                }
                _ => {}
            }
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
            // The demand may have been handled in the meantime.
            if self.queue.count_for_session(&item.session_id, now_ms) == 0 {
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
                    effects.push(Effect::Focus {
                        session_id: item.session_id.clone(),
                        node_id: item.node_id.clone(),
                    });
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
        self.queue.acknowledge(&id);
        self.governor.reset();
        self.governor.record_grant(None, now_ms);
        Some(Effect::Focus {
            session_id,
            node_id,
        })
    }

    /// Advances past the demand the user is currently on.
    pub fn goto_after(&mut self, current: &AttentionId, now_ms: i64) -> Option<Effect> {
        let entry = self.queue.next_after(current, now_ms)?;
        let session_id = entry.session_id.clone();
        let node_id = entry.node_id.clone();
        let id = entry.id.clone();
        self.queue.acknowledge(&id);
        self.governor.reset();
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
        self.queue.snooze(id, until_ms)
    }

    pub fn dismiss(&mut self, id: &AttentionId) -> bool {
        self.queue.dismiss(id)
    }

    /// Drops demands tied to a node that has gone away.
    pub fn resolve_node(&mut self, node: &NodeId) -> usize {
        self.deferred.retain(|d| d.node_id.as_ref() != Some(node));
        self.queue.resolve_node(node)
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
    ) -> Effect {
        let FocusRequest {
            session,
            node_id,
            parent_node_id,
            subject_external_id,
            action,
            policy,
        } = request;
        match decision {
            FocusDecision::Grant => {
                self.governor
                    .record_grant(ctx.active_session.clone(), now_ms);
                Effect::Focus {
                    session_id: session.clone(),
                    node_id,
                }
            }
            FocusDecision::Defer { until_ms, reason } => {
                self.deferred.push(DeferredFocus {
                    session_id: session.clone(),
                    node_id,
                    parent_node_id,
                    subject_external_id,
                    action,
                    until_ms,
                    created_ms: now_ms,
                    policy: policy.clone(),
                });
                Effect::FocusDeferred {
                    session_id: session.clone(),
                    until_ms,
                    reason,
                }
            }
            FocusDecision::Deny { reason } => Effect::FocusDenied {
                session_id: session.clone(),
                reason,
            },
        }
    }

    /// Handles events that close demands instead of raising them.
    fn handle_resolving_event(&mut self, event: &TurnEvent, now_ms: i64) -> Option<Vec<Effect>> {
        let closes = match &event.kind {
            // The user answered, so a new turn is under way.
            EventKind::AgentTurnStarted { .. } => true,
            EventKind::AgentPermissionResolved { .. } => true,
            EventKind::SessionAttentionResolved => true,
            // A dead process cannot still be waiting on you.
            EventKind::ProcessExited { .. } | EventKind::ProcessFailed { .. } => false,
            _ => false,
        };

        if let EventKind::ProcessExited { .. } = &event.kind {
            if let Some(node) = &event.node_id {
                let removed = self.resolve_node(node);
                if removed > 0 {
                    return Some(vec![Effect::Cleared {
                        session_id: event.session_id.clone(),
                    }]);
                }
            }
            return Some(Vec::new());
        }

        if !closes {
            return None;
        }

        let clear_entire_session = matches!(&event.kind, EventKind::SessionAttentionResolved);
        let cleared = if clear_entire_session {
            self.queue.resolve_session(&event.session_id)
        } else {
            self.queue.resolve_subject(
                &event.session_id,
                event.node_id.as_ref(),
                event.parent_node_id.as_ref(),
                event.agent.external_id.as_deref(),
            )
        };
        self.deferred.retain(|deferred| {
            if clear_entire_session {
                return deferred.session_id != event.session_id;
            }
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
        let _ = now_ms;
        Some(if cleared > 0 {
            vec![Effect::Cleared {
                session_id: event.session_id.clone(),
            }]
        } else {
            Vec::new()
        })
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
    use crate::event::{Confidence, EventSource, Risk};

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
    fn a_muted_session_badges_and_nothing_more() {
        let mut m = AttentionManager::new();
        m.mute_session(&sess("sess_a"), T0 + 60_000);
        let effects = m.ingest(
            &permission("sess_a"),
            &AttentionPolicy::default(),
            &ctx(),
            T0,
        );
        assert_eq!(effects.len(), 1);
        assert!(matches!(effects[0], Effect::Badge { .. }));
        assert!(m.queue().is_empty());

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
    fn an_unassigned_resume_only_clears_its_parent_scope_and_deferral() {
        let mut m = AttentionManager::new();
        let parent_a = NodeId::from_stored("parent_a");
        let parent_b = NodeId::from_stored("parent_b");
        let reviewer = NodeId::from_stored("reviewer");
        let tests = NodeId::from_stored("tests");

        // B lands first and gets the focus grant. A is deferred by the cooldown,
        // which lets this test prove the deferral is scoped as narrowly as the
        // queue entry itself.
        m.ingest(
            &permission("sess_a").with_parent(parent_b.clone()),
            &AttentionPolicy::default(),
            &ctx(),
            T0,
        );
        m.ingest(
            &permission("sess_a").with_parent(parent_a.clone()),
            &AttentionPolicy::default(),
            &ctx(),
            T0 + 1,
        );
        m.ingest(
            &permission("sess_a").with_node(reviewer.clone()),
            &AttentionPolicy::default(),
            &ctx(),
            T0 + 2,
        );
        m.ingest(
            &permission("sess_a").with_node(tests.clone()),
            &AttentionPolicy::default(),
            &ctx(),
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
    fn a_dead_process_leaves_the_queue_behind() {
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
