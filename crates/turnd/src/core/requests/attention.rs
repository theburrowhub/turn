//! Attention operations, and the user correcting a state Turn got wrong.

use super::Answer;
use crate::core::Core;
use turn_core::attention::Effect;
use turn_core::event::{Confidence, EventKind, EventSource, TurnEvent};
use turn_core::ids::{AttentionId, NodeId, SessionId};
use turn_core::state::{Lifecycle, Turn};
use turn_core::UserContext;
use turn_proto::{AttentionView, ErrorCode, ProtoError, Response, ServerEvent};

impl Core {
    /// The demand the user should handle next. A peek: nothing is marked or moved.
    pub(super) fn next_attention(&self, now_ms: i64) -> Option<AttentionView> {
        let entry = self.attention.next_attention(now_ms)?;
        let name = self
            .sessions
            .get(&entry.session_id)
            .map(|session| session.name.clone())
            .unwrap_or_else(|| entry.session_id.as_str().to_string());
        Some(AttentionView::from_entry(entry, name, now_ms))
    }

    pub(super) fn list_attention(
        &self,
        session: Option<&SessionId>,
        now_ms: i64,
    ) -> Vec<AttentionView> {
        self.attention_views(now_ms)
            .into_iter()
            .filter(|view| session.is_none_or(|id| view.session_id() == id))
            .collect()
    }

    /// Jumps to a demand and marks it acknowledged.
    ///
    /// A user-initiated move, so it bypasses the focus governor's guards — pressing the
    /// shortcut is consent. The governor is still reset, so automatic focus does not
    /// immediately fight the manual navigation.
    pub(super) fn goto_attention(
        &mut self,
        attention_id: Option<&AttentionId>,
        now_ms: i64,
    ) -> Answer {
        let effect = match attention_id {
            None => self.attention.goto_next(now_ms),
            Some(id) => {
                let Some(entry) = self.attention.queue().get(id) else {
                    return Err(ProtoError::not_found("attention entry", id.as_str()));
                };
                let target = Effect::Focus {
                    session_id: entry.session_id.clone(),
                    node_id: entry.node_id.clone(),
                };
                self.acknowledge(id, now_ms);
                Some(target)
            }
        };

        let effects: Vec<Effect> = effect.into_iter().collect();
        // Applied locally rather than pushed: focus belongs to the window that asked,
        // and a second UI showing the same daemon must not be dragged along.
        for effect in &effects {
            if let Effect::Focus { session_id, .. } = effect {
                self.user.active_session = Some(session_id.clone());
            }
        }
        self.persist_attention();
        self.push_attention_queue(now_ms);
        Ok(Response::Effects { effects })
    }

    pub(super) fn acknowledge_attention(&mut self, id: &AttentionId, now_ms: i64) -> Answer {
        if !self.acknowledge(id, now_ms) {
            return Err(ProtoError::not_found("attention entry", id.as_str()));
        }
        self.persist_attention();
        self.push_attention_queue(now_ms);
        Ok(Response::Ack)
    }

    pub(super) fn snooze_attention(
        &mut self,
        id: &AttentionId,
        until_ms: i64,
        now_ms: i64,
    ) -> Answer {
        if until_ms <= now_ms {
            // A snooze in the past would come back actionable immediately and read as
            // the button having done nothing.
            return Err(ProtoError::invalid("A snooze must end in the future"));
        }
        if !self.attention.snooze(id, until_ms) {
            return Err(ProtoError::not_found("attention entry", id.as_str()));
        }
        self.persist_attention();
        self.push_attention_queue(now_ms);
        Ok(Response::Ack)
    }

    pub(super) fn dismiss_attention(&mut self, id: &AttentionId, now_ms: i64) -> Answer {
        let session = self.attention_session(id);
        if !self.attention.dismiss(id) {
            return Err(ProtoError::not_found("attention entry", id.as_str()));
        }
        self.persist_attention();
        self.push_attention_queue(now_ms);
        if let Some(session) = session {
            self.push_session_state(&session, now_ms);
        }
        Ok(Response::Ack)
    }

    /// Silences a session, or lifts the silence.
    ///
    /// A muted session still badges. Muting quietens the interruption, not the evidence:
    /// the sidebar keeps showing that something happened, which is what makes it safe to
    /// mute something in the first place.
    pub(super) fn mute_session(
        &mut self,
        session: &SessionId,
        until_ms: Option<i64>,
        now_ms: i64,
    ) -> Answer {
        self.session(session)?;
        match until_ms {
            Some(until) if until <= now_ms => {
                return Err(ProtoError::invalid("A mute must end in the future"))
            }
            Some(until) => self.attention.mute_session(session, until),
            None => self.attention.unmute_session(session),
        }
        self.push_session_state(session, now_ms);
        Ok(Response::Ack)
    }

    /// The user fixing a state Turn got wrong.
    ///
    /// On the question of what is actually happening in their terminal the human
    /// outranks every heuristic, so the correction is recorded at
    /// [`EventSource::UserCorrection`] with explicit confidence — and that becomes the
    /// node's turn authority, which is what stops output inference from quietly putting
    /// the wrong state back a second later.
    pub(super) fn correct_state(
        &mut self,
        session_id: &SessionId,
        node_id: &NodeId,
        lifecycle: Option<Lifecycle>,
        turn: Option<Turn>,
        note: Option<String>,
        now_ms: i64,
    ) -> Answer {
        if lifecycle.is_none() && turn.is_none() {
            return Err(ProtoError::invalid(
                "A correction has to say what the state actually is",
            ));
        }
        let node = self.node_of(session_id, node_id)?.clone();
        if turn.is_some() && node.turn.is_none() {
            // A shell has no turn axis. Giving it one would put a state in the UI that
            // nothing can ever move again.
            return Err(ProtoError::new(
                ErrorCode::Conflict,
                "This process is not an agent, so it has no turn state to correct",
            ));
        }
        if let Some(lifecycle) = &lifecycle {
            if lifecycle.is_running() && node.pid.is_none() {
                return Err(ProtoError::invalid(
                    "Turn has no process id for this node, so it cannot be marked as running",
                ));
            }
        }

        let kind = correction_kind(lifecycle.as_ref(), turn.as_ref(), note.as_deref(), &node);
        let still_needs_user = turn.as_ref().is_some_and(Turn::needs_user);

        {
            let session = self.session_mut(session_id)?;
            let Some(node) = session.tree.get_mut(node_id) else {
                return Err(ProtoError::not_found("process", node_id.as_str()));
            };
            if let Some(lifecycle) = lifecycle.clone() {
                if lifecycle.is_terminal() {
                    node.ended_ms.get_or_insert(now_ms);
                } else {
                    node.ended_ms = None;
                }
                node.lifecycle = lifecycle;
            }
            if let Some(turn) = turn.clone() {
                node.turn = Some(turn);
            }
            node.interaction_pending = still_needs_user;
            if !still_needs_user {
                if let Some(agent) = node.agent.as_mut() {
                    agent.pending_permission = None;
                    agent.pending_question = None;
                }
            }
        }

        if turn.is_some() {
            // The correction, not the guess, is now the authority for this axis.
            self.turn_authority
                .insert(node_id.clone(), Confidence::Explicit);
        }
        if !still_needs_user {
            // Whatever Turn thought this node wanted, it does not want it.
            self.attention.resolve_node(node_id);
        }

        let mut event = TurnEvent::new(
            session_id.clone(),
            kind,
            EventSource::UserCorrection,
            Confidence::Explicit,
            now_ms,
        )
        .with_node(node_id.clone());
        // The whole correction is kept, in the user's words, so a misfiring rule can be
        // found later rather than guessed at.
        event = event.with_raw(
            serde_json::json!({
                "correction": {
                    "lifecycle": lifecycle,
                    "turn": turn,
                    "note": note,
                }
            })
            .to_string(),
        );

        self.record_correction(session_id, node_id, event, now_ms);

        let view = self
            .node_view(session_id, node_id, now_ms)
            .ok_or_else(|| ProtoError::internal("the corrected node is missing from the tree"))?;
        Ok(Response::Node {
            node: Box::new(view),
        })
    }

    /// Tells the daemon what the user is doing.
    ///
    /// Answers with effects because the governor may release a deferred focus jump the
    /// moment the user's hands leave the keyboard — which is the whole reason the
    /// deferral exists rather than a denial.
    pub(super) fn update_user_activity(&mut self, context: UserContext, now_ms: i64) -> Answer {
        self.user = context;
        let effects = self.attention.tick(&self.user.clone(), now_ms);
        for effect in &effects {
            if let Effect::Focus { session_id, .. } = effect {
                self.user.active_session = Some(session_id.clone());
            }
        }
        if effects.iter().any(|e| matches!(e, Effect::Cleared { .. })) {
            self.persist_attention();
            self.push_attention_queue(now_ms);
        }
        Ok(Response::Effects { effects })
    }

    /// Records a correction without letting it be re-derived from its own event.
    ///
    /// The state was already applied above, exactly as the user described it. Feeding
    /// the event back through the normal path would re-derive the state from an event
    /// kind, and the vocabulary is not rich enough to round-trip every correction — a
    /// permission the user says is *still* pending has no summary to reconstruct.
    fn record_correction(
        &mut self,
        session_id: &SessionId,
        node_id: &NodeId,
        event: TurnEvent,
        now_ms: i64,
    ) {
        self.persist_event(&event);
        self.persist_session_quietly(session_id);

        let policy = match self.sessions.get(session_id) {
            Some(session) => session.attention.clone(),
            None => return,
        };
        let effects = self
            .attention
            .ingest(&event, &policy, &self.user.clone(), now_ms);

        self.push_all(ServerEvent::TurnEventEmitted {
            turn_event: event.clone(),
        });
        self.push_node_state(session_id, node_id, Some(event), now_ms);
        self.push_session_state(session_id, now_ms);
        self.push_attention_queue(now_ms);
        self.emit_effects(effects, now_ms);
    }
}

/// The event kind that best describes a correction.
///
/// Chosen from the vocabulary rather than invented, and never a kind that would claim
/// more than the user said. A correction with no turn in it — "this process is actually
/// still running" — is recorded as the lifecycle event it is, and a correction that
/// leaves nothing outstanding is recorded as the demand being resolved.
fn correction_kind(
    lifecycle: Option<&Lifecycle>,
    turn: Option<&Turn>,
    note: Option<&str>,
    node: &turn_core::model::ProcessNode,
) -> EventKind {
    match turn {
        Some(Turn::Idle) => EventKind::AgentIdle,
        Some(Turn::Active) => EventKind::AgentTurnStarted {
            prompt_excerpt: note.map(str::to_string),
        },
        Some(Turn::Done) => EventKind::AgentTurnCompleted {
            last_message: note.map(str::to_string),
            background_tasks: 0,
        },
        Some(Turn::TaskDone) => EventKind::AgentTaskCompleted {
            summary: note.map(str::to_string),
        },
        Some(Turn::Failed { reason }) => EventKind::AgentFailed {
            reason: reason.clone(),
        },
        Some(Turn::AwaitingUser { reason }) => EventKind::AgentWaitingForUser {
            reason: *reason,
            summary: note.map(str::to_string),
        },
        Some(Turn::Unknown) | None => match lifecycle {
            Some(Lifecycle::Exited { code }) => EventKind::ProcessExited { code: *code },
            Some(Lifecycle::Signaled { .. }) => EventKind::ProcessFailed {
                code: None,
                signal: None,
            },
            Some(other) if other.is_running() => EventKind::ProcessStarted {
                // Checked by the caller: a node with no pid cannot be corrected to a
                // running state, so there is no invented number here.
                pid: node.pid.unwrap_or_default(),
                command: node.command.clone(),
            },
            // `Lost` and a bare turn of `Unknown` both mean "Turn does not know", and
            // the honest record of that is that nothing is outstanding any more.
            _ => EventKind::SessionAttentionResolved,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use turn_core::state::AwaitingReason;

    fn node() -> turn_core::model::ProcessNode {
        let mut node = turn_core::model::ProcessNode::agent(
            SessionId::from_stored("sess_x"),
            "claude",
            "/tmp",
            0,
        );
        node.pid = Some(4242);
        node
    }

    #[test]
    fn a_correction_to_active_records_a_turn_starting_with_the_users_note() {
        let kind = correction_kind(None, Some(&Turn::Active), Some("still working"), &node());
        match kind {
            EventKind::AgentTurnStarted { prompt_excerpt } => {
                assert_eq!(prompt_excerpt.as_deref(), Some("still working"));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn a_correction_that_says_it_is_still_waiting_keeps_the_reason() {
        let kind = correction_kind(
            None,
            Some(&Turn::AwaitingUser {
                reason: AwaitingReason::Permission,
            }),
            None,
            &node(),
        );
        assert!(matches!(
            kind,
            EventKind::AgentWaitingForUser {
                reason: AwaitingReason::Permission,
                ..
            }
        ));
    }

    #[test]
    fn a_lifecycle_only_correction_records_a_lifecycle_event() {
        assert!(matches!(
            correction_kind(Some(&Lifecycle::Exited { code: 3 }), None, None, &node()),
            EventKind::ProcessExited { code: 3 }
        ));
        assert!(matches!(
            correction_kind(Some(&Lifecycle::Alive), None, None, &node()),
            EventKind::ProcessStarted { pid: 4242, .. }
        ));
        assert!(matches!(
            correction_kind(
                Some(&Lifecycle::Signaled {
                    signal: "Killed".into()
                }),
                None,
                None,
                &node()
            ),
            EventKind::ProcessFailed {
                code: None,
                signal: None
            }
        ));
    }

    #[test]
    fn a_correction_that_amounts_to_we_do_not_know_records_the_demand_as_resolved() {
        // The common case behind this: a heuristic decided an idle shell was an agent
        // waiting for input, and the user says otherwise.
        assert!(matches!(
            correction_kind(Some(&Lifecycle::Lost), None, None, &node()),
            EventKind::SessionAttentionResolved
        ));
        assert!(matches!(
            correction_kind(None, Some(&Turn::Unknown), None, &node()),
            EventKind::SessionAttentionResolved
        ));
    }
}
