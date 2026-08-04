//! Attention glue: effects out, and the two operations the manager only exposes
//! indirectly.

use super::Core;
use turn_core::attention::Effect;
use turn_core::ids::{AttentionId, SessionId};
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
        if effects.is_empty() {
            return;
        }
        let mut queue_changed = false;
        for effect in effects {
            if matches!(effect, Effect::Enqueued { .. } | Effect::Cleared { .. }) {
                queue_changed = true;
            }
            // A granted focus change is applied to our own idea of where the user is,
            // optimistically. The UI confirms it with `update_user_activity`; until
            // then this is what stops the governor from granting the same jump twice.
            if let Effect::Focus { session_id, .. } = &effect {
                self.user.active_session = Some(session_id.clone());
            }
            self.push_all(ServerEvent::AttentionEffect { effect });
        }
        if queue_changed {
            self.persist_attention();
            self.push_attention_queue(now_ms);
        }
    }

    /// Writes the queue through to the store.
    ///
    /// The queue is state the user would notice losing: a permission request that
    /// arrived while they were away must still be waiting after a restart.
    pub(crate) fn persist_attention(&self) {
        if let Err(error) = self.store.attention().replace_all(self.attention.queue()) {
            tracing::warn!(%error, "could not save the attention queue");
        }
    }

    /// Marks a demand as seen without jumping to it.
    ///
    /// [`turn_core::AttentionManager`] exposes acknowledgement only through its jump
    /// operations, because acknowledging is what happens when the user is taken
    /// somewhere. `acknowledge_attention` asks for the mark without the jump, so this
    /// drives the same primitive against the entry *before* the target and discards
    /// the focus effect the manager returns — `goto_after(previous)` acknowledges
    /// exactly the entry we mean, and `goto_next` covers the case where the target is
    /// already at the head of the queue.
    ///
    /// The side effect that survives is the governor being reset, which is correct
    /// here for the reason it is documented in `turn-core`: the user acting by hand is
    /// not an interruption, and automatic focus should not immediately fight it.
    pub(crate) fn acknowledge(&mut self, id: &AttentionId, now_ms: i64) -> bool {
        if self.attention.queue().get(id).is_none() {
            return false;
        }
        let ordered: Vec<AttentionId> = self
            .attention
            .queue()
            .ordered(now_ms)
            .into_iter()
            .map(|entry| entry.id.clone())
            .collect();
        let Some(position) = ordered.iter().position(|candidate| candidate == id) else {
            // Snoozed and not yet due. It is already out of the user's way, and
            // marking it seen would say something untrue about a demand they have
            // deliberately postponed.
            return true;
        };
        let _discarded_focus = match position {
            0 => self.attention.goto_next(now_ms),
            index => self.attention.goto_after(&ordered[index - 1], now_ms),
        };
        true
    }

    /// The session a demand belongs to, for routing a jump.
    pub(crate) fn attention_session(&self, id: &AttentionId) -> Option<SessionId> {
        self.attention
            .queue()
            .get(id)
            .map(|entry| entry.session_id.clone())
    }
}
