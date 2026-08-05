//! Attention glue: effects out, and the two operations the manager only exposes
//! indirectly.

use super::Core;
use turn_core::attention::Effect;
use turn_core::ids::{AttentionId, NodeId, SessionId};
use turn_proto::ServerEvent;

impl Core {
    /// Pushes effects to the UI and keeps the queue's projection current.
    ///
    /// The daemon performs none of them. Every effect here is something only a window
    /// can do — badge a row, play a sound, move focus — and `RunCustom` is forwarded
    /// rather than executed: the daemon shelling out on an agent's behalf is exactly
    /// the shape of the thing Turn promises not to do, even when the command itself
    /// came from the user's own settings.
    pub(crate) fn emit_effects(&mut self, effects: Vec<Effect>, now_ms: i64) {
        self.emit_effects_with_queue_persistence(effects, now_ms, true);
    }

    /// Publishes effects whose queue was already committed by an event checkpoint.
    pub(crate) fn emit_checkpointed_effects(&mut self, effects: Vec<Effect>, now_ms: i64) {
        self.emit_effects_with_queue_persistence(effects, now_ms, false);
    }

    fn emit_effects_with_queue_persistence(
        &mut self,
        effects: Vec<Effect>,
        now_ms: i64,
        persist_queue_changes: bool,
    ) {
        if effects.is_empty() {
            return;
        }
        let queue_changed = effects
            .iter()
            .any(|effect| matches!(effect, Effect::Enqueued { .. } | Effect::Cleared { .. }));
        if queue_changed && persist_queue_changes && !self.persist_attention() {
            return;
        }
        for effect in effects {
            // A granted focus change is applied to our own idea of where the user is,
            // optimistically. The UI confirms it with `update_user_activity`; until
            // then this is what stops the governor from granting the same jump twice.
            if let Effect::Focus { session_id, .. } = &effect {
                self.user.active_session = Some(session_id.clone());
            }
            self.push_all(ServerEvent::AttentionEffect { effect });
        }
        if queue_changed {
            self.push_attention_queue(now_ms);
        }
    }

    /// Writes the queue through to the store.
    ///
    /// The queue is state the user would notice losing: a permission request that
    /// arrived while they were away must still be waiting after a restart.
    pub(crate) fn persist_attention(&self) -> bool {
        if !self.failed_ingest_checkpoints.is_empty() {
            tracing::warn!(
                pending = self.failed_ingest_checkpoints.len(),
                "deferred a standalone attention write behind failed atomic checkpoints"
            );
            return false;
        }
        match self.store.attention().replace_all(self.attention.queue()) {
            Ok(()) => true,
            Err(error) => {
                tracing::warn!(%error, "could not save the attention queue");
                false
            }
        }
    }

    /// Clears terminal subjects and publishes the durable queue change once.
    ///
    /// Some lifecycle paths do not travel through `Core::ingest` (process-table
    /// disappearance, relaunch and user correction). They must still use the same
    /// ownership rules and, crucially, persist the removal before restart can
    /// resurrect it.
    pub(crate) fn resolve_lifecycle_attention(
        &mut self,
        session_id: &SessionId,
        subjects: &[(NodeId, Option<NodeId>, Option<String>)],
        now_ms: i64,
    ) -> usize {
        let cleared = subjects
            .iter()
            .map(|(node, parent, external_id)| {
                self.attention.resolve_lifecycle(
                    session_id,
                    node,
                    parent.as_ref(),
                    external_id.as_deref(),
                )
            })
            .sum();
        if cleared > 0 {
            if self
                .attention
                .queue()
                .iter()
                .any(|entry| &entry.session_id == session_id)
            {
                if self.persist_attention() {
                    self.push_attention_queue(now_ms);
                }
            } else {
                self.emit_effects(
                    vec![Effect::Cleared {
                        session_id: session_id.clone(),
                    }],
                    now_ms,
                );
            }
        }
        cleared
    }

    /// Clears only one exact node for a non-terminal user correction, with the
    /// same durable notification contract as lifecycle cleanup.
    pub(crate) fn resolve_exact_attention(
        &mut self,
        session_id: &SessionId,
        node_id: &NodeId,
        now_ms: i64,
    ) -> usize {
        let cleared = self.attention.resolve_node_in_session(session_id, node_id);
        if cleared > 0 {
            if self
                .attention
                .queue()
                .iter()
                .any(|entry| &entry.session_id == session_id)
            {
                if self.persist_attention() {
                    self.push_attention_queue(now_ms);
                }
            } else {
                self.emit_effects(
                    vec![Effect::Cleared {
                        session_id: session_id.clone(),
                    }],
                    now_ms,
                );
            }
        }
        cleared
    }

    /// Removes all queue/deferred references to nodes that an explicit user
    /// action deletes from the tree. Lifecycle evidence may survive a crash, but
    /// it must never point at an identity that no longer exists.
    pub(crate) fn remove_attention_for_deleted_nodes(
        &mut self,
        session_id: &SessionId,
        nodes: &[NodeId],
        now_ms: i64,
    ) -> usize {
        let cleared = nodes
            .iter()
            .map(|node| self.attention.remove_owner_in_session(session_id, node))
            .sum();
        if cleared > 0 {
            if self
                .attention
                .queue()
                .iter()
                .any(|entry| &entry.session_id == session_id)
            {
                if self.persist_attention() {
                    self.push_attention_queue(now_ms);
                }
            } else {
                self.emit_effects(
                    vec![Effect::Cleared {
                        session_id: session_id.clone(),
                    }],
                    now_ms,
                );
            }
        }
        cleared
    }

    /// Marks a demand as seen without jumping to it.
    ///
    /// Acknowledgement is deliberately not navigation: it must not record a focus
    /// grant or perturb the governor when the user merely marks an item seen.
    pub(crate) fn acknowledge(&mut self, id: &AttentionId, now_ms: i64) -> bool {
        let _ = now_ms;
        self.attention.acknowledge(id)
    }

    /// The session a demand belongs to, for routing a jump.
    pub(crate) fn attention_session(&self, id: &AttentionId) -> Option<SessionId> {
        self.attention
            .queue()
            .get(id)
            .map(|entry| entry.session_id.clone())
    }
}
