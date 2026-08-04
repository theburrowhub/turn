//! The attention queue.
//!
//! When several agents finish at once, nothing good comes from each of them
//! shouting. The queue turns simultaneous demands into an ordered list with one
//! obvious next item, which is what makes `go-to-next-attention` a coherent
//! command instead of a lottery.

use crate::event::Confidence;
use crate::ids::{AttentionId, NodeId, SessionId};
use crate::state::AwaitingReason;
use serde::{Deserialize, Serialize};

/// Lifecycle of a single queued demand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EntryState {
    /// Waiting to be visited.
    Pending,
    /// Postponed until a wall-clock deadline.
    Snoozed { until_ms: i64 },
    /// The user has seen it but has not finished with it. Stays in the queue,
    /// ranked below anything pending.
    Acknowledged,
}

/// One demand for the user's attention.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttentionEntry {
    pub id: AttentionId,
    pub session_id: SessionId,
    /// The specific process inside the session, so jumping lands on the right pane.
    pub node_id: Option<NodeId>,
    pub reason: AwaitingReason,
    pub summary: Option<String>,
    pub confidence: Confidence,
    pub created_ms: i64,
    pub updated_ms: i64,
    pub state: EntryState,
    /// Session-level ranking adjustment, copied in at insert time.
    pub priority_boost: i16,
}

impl AttentionEntry {
    /// Stable identity for deduplication: the same agent blocking on the same
    /// kind of thing is one demand, however many times it says so.
    pub fn dedup_key(&self) -> String {
        format!(
            "{}|{}|{:?}",
            self.session_id,
            self.node_id.as_ref().map(|n| n.as_str()).unwrap_or("-"),
            self.reason
        )
    }

    /// Ranking score. Higher comes first.
    ///
    /// Age contributes a bounded bonus so a low-priority session cannot starve
    /// forever behind a stream of urgent ones, but the bonus is capped well
    /// below a single priority class so it never reorders permission-vs-idle.
    pub fn score(&self, now_ms: i64) -> i32 {
        let base = self.reason.base_priority() as i32;
        let state_penalty = match self.state {
            EntryState::Pending => 0,
            EntryState::Acknowledged => -40,
            EntryState::Snoozed { .. } => -1000,
        };
        // Provisional demands rank below confirmed ones.
        let confidence_penalty = if self.confidence.is_provisional() {
            -15
        } else {
            0
        };
        let age_minutes = ((now_ms - self.created_ms).max(0) / 60_000) as i32;
        let age_bonus = age_minutes.min(15);
        base + state_penalty + confidence_penalty + age_bonus + self.priority_boost as i32
    }

    /// Whether this entry is currently eligible to be visited.
    pub fn is_actionable(&self, now_ms: i64) -> bool {
        match self.state {
            EntryState::Pending | EntryState::Acknowledged => true,
            EntryState::Snoozed { until_ms } => now_ms >= until_ms,
        }
    }
}

/// An ordered set of attention demands, deduplicated by session+node+reason.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AttentionQueue {
    entries: Vec<AttentionEntry>,
}

impl AttentionQueue {
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts a demand, or refreshes the existing one it duplicates.
    ///
    /// Refreshing bumps `updated_ms` and upgrades confidence, but deliberately
    /// keeps the original `created_ms` so a chatty agent cannot keep resetting
    /// its own age bonus and jump the queue.
    pub fn upsert(&mut self, entry: AttentionEntry) -> AttentionId {
        let key = entry.dedup_key();
        if let Some(existing) = self.entries.iter_mut().find(|e| e.dedup_key() == key) {
            existing.updated_ms = entry.updated_ms;
            existing.confidence = existing.confidence.max(entry.confidence);
            existing.priority_boost = entry.priority_boost;
            if entry.summary.is_some() {
                existing.summary = entry.summary;
            }
            // A repeat demand un-acknowledges: the agent asked again.
            if matches!(existing.state, EntryState::Acknowledged) {
                existing.state = EntryState::Pending;
            }
            return existing.id.clone();
        }
        let id = entry.id.clone();
        self.entries.push(entry);
        id
    }

    /// The demand the user should handle next, if any.
    pub fn next(&self, now_ms: i64) -> Option<&AttentionEntry> {
        self.entries
            .iter()
            .filter(|e| e.is_actionable(now_ms))
            .max_by(|a, b| {
                a.score(now_ms)
                    .cmp(&b.score(now_ms))
                    // Ties break toward the older demand, so the queue drains
                    // in a predictable order instead of shuffling.
                    .then_with(|| b.created_ms.cmp(&a.created_ms))
            })
    }

    /// All actionable demands, most urgent first. Drives the queue panel.
    pub fn ordered(&self, now_ms: i64) -> Vec<&AttentionEntry> {
        let mut visible: Vec<_> = self
            .entries
            .iter()
            .filter(|e| e.is_actionable(now_ms))
            .collect();
        visible.sort_by(|a, b| {
            b.score(now_ms)
                .cmp(&a.score(now_ms))
                .then_with(|| a.created_ms.cmp(&b.created_ms))
        });
        visible
    }

    /// The demand after the one currently being visited, for repeated presses of
    /// the shortcut.
    pub fn next_after(&self, current: &AttentionId, now_ms: i64) -> Option<&AttentionEntry> {
        let ordered = self.ordered(now_ms);
        let pos = ordered.iter().position(|e| &e.id == current);
        match pos {
            Some(i) => ordered.get(i + 1).or_else(|| ordered.first()).copied(),
            None => ordered.first().copied(),
        }
    }

    /// Marks as seen without removing it.
    pub fn acknowledge(&mut self, id: &AttentionId) -> bool {
        match self.entries.iter_mut().find(|e| &e.id == id) {
            Some(entry) => {
                entry.state = EntryState::Acknowledged;
                true
            }
            None => false,
        }
    }

    /// Postpones a demand.
    pub fn snooze(&mut self, id: &AttentionId, until_ms: i64) -> bool {
        match self.entries.iter_mut().find(|e| &e.id == id) {
            Some(entry) => {
                entry.state = EntryState::Snoozed { until_ms };
                true
            }
            None => false,
        }
    }

    /// Removes a demand entirely.
    pub fn dismiss(&mut self, id: &AttentionId) -> bool {
        let before = self.entries.len();
        self.entries.retain(|e| &e.id != id);
        self.entries.len() != before
    }

    /// Drops every demand for a session. Called when the user actually engages
    /// with it, or when its process dies.
    pub fn resolve_session(&mut self, session: &SessionId) -> usize {
        let before = self.entries.len();
        self.entries.retain(|e| &e.session_id != session);
        before - self.entries.len()
    }

    /// Drops demands for one node, leaving its siblings alone.
    pub fn resolve_node(&mut self, node: &NodeId) -> usize {
        let before = self.entries.len();
        self.entries.retain(|e| e.node_id.as_ref() != Some(node));
        before - self.entries.len()
    }

    /// Number of demands pending for a session, for the sidebar badge.
    pub fn count_for_session(&self, session: &SessionId, now_ms: i64) -> usize {
        self.entries
            .iter()
            .filter(|e| &e.session_id == session && e.is_actionable(now_ms))
            .count()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &AttentionEntry> {
        self.entries.iter()
    }

    pub fn get(&self, id: &AttentionId) -> Option<&AttentionEntry> {
        self.entries.iter().find(|e| &e.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: i64 = 1_700_000_000_000;

    fn entry(session: &str, reason: AwaitingReason, created_ms: i64) -> AttentionEntry {
        AttentionEntry {
            id: AttentionId::new(),
            session_id: SessionId::from_stored(session),
            node_id: None,
            reason,
            summary: None,
            confidence: Confidence::Explicit,
            created_ms,
            updated_ms: created_ms,
            state: EntryState::Pending,
            priority_boost: 0,
        }
    }

    /// Case A from the brief: several agents finish at once. There must be one
    /// unambiguous next item, ordered by urgency.
    #[test]
    fn simultaneous_demands_produce_one_ordered_next() {
        let mut q = AttentionQueue::new();
        q.upsert(entry("sess_a", AwaitingReason::Input, T0));
        q.upsert(entry("sess_b", AwaitingReason::Permission, T0));
        q.upsert(entry("sess_c", AwaitingReason::Question, T0));

        let next = q.next(T0).expect("a next item");
        assert_eq!(next.session_id.as_str(), "sess_b", "permission goes first");

        let order: Vec<_> = q
            .ordered(T0)
            .iter()
            .map(|e| e.session_id.as_str().to_string())
            .collect();
        assert_eq!(order, vec!["sess_b", "sess_c", "sess_a"]);
    }

    #[test]
    fn repeated_demands_collapse_instead_of_piling_up() {
        let mut q = AttentionQueue::new();
        for i in 0..40 {
            let mut e = entry("sess_a", AwaitingReason::Question, T0);
            e.updated_ms = T0 + i * 100;
            q.upsert(e);
        }
        assert_eq!(q.len(), 1, "a chatty agent is still one demand");
    }

    #[test]
    fn a_repeated_demand_cannot_reset_its_age_to_jump_the_queue() {
        let mut q = AttentionQueue::new();
        let first = entry("sess_old", AwaitingReason::Question, T0);
        let created = first.created_ms;
        q.upsert(first);

        let mut again = entry("sess_old", AwaitingReason::Question, T0 + 600_000);
        again.updated_ms = T0 + 600_000;
        q.upsert(again);

        let stored = q.iter().next().unwrap();
        assert_eq!(stored.created_ms, created, "age is preserved");
        assert_eq!(stored.updated_ms, T0 + 600_000, "recency still updates");
    }

    #[test]
    fn two_subagents_in_one_session_are_two_demands() {
        let mut q = AttentionQueue::new();
        let mut a = entry("sess_a", AwaitingReason::Question, T0);
        a.node_id = Some(NodeId::from_stored("proc_one"));
        let mut b = entry("sess_a", AwaitingReason::Question, T0);
        b.node_id = Some(NodeId::from_stored("proc_two"));
        q.upsert(a);
        q.upsert(b);
        assert_eq!(q.len(), 2);
        assert_eq!(
            q.count_for_session(&SessionId::from_stored("sess_a"), T0),
            2
        );
    }

    #[test]
    fn snoozed_demands_disappear_until_their_deadline() {
        let mut q = AttentionQueue::new();
        let id = q.upsert(entry("sess_a", AwaitingReason::Permission, T0));
        q.snooze(&id, T0 + 60_000);

        assert!(q.next(T0).is_none(), "snoozed items are not offered");
        assert_eq!(
            q.count_for_session(&SessionId::from_stored("sess_a"), T0),
            0
        );
        assert!(q.next(T0 + 60_000).is_some(), "and come back on time");
    }

    #[test]
    fn acknowledged_demands_rank_below_pending_ones_but_stay_reachable() {
        let mut q = AttentionQueue::new();
        // Acknowledge the higher-priority one; the pending lesser one wins.
        let permission = q.upsert(entry("sess_p", AwaitingReason::Permission, T0));
        q.upsert(entry("sess_q", AwaitingReason::Input, T0));
        q.acknowledge(&permission);

        assert_eq!(q.next(T0).unwrap().session_id.as_str(), "sess_q");
        assert_eq!(q.ordered(T0).len(), 2, "acknowledged is still listed");
    }

    #[test]
    fn asking_again_un_acknowledges_a_demand() {
        let mut q = AttentionQueue::new();
        let id = q.upsert(entry("sess_a", AwaitingReason::Permission, T0));
        q.acknowledge(&id);
        q.upsert(entry("sess_a", AwaitingReason::Permission, T0 + 5_000));
        assert_eq!(q.get(&id).unwrap().state, EntryState::Pending);
    }

    #[test]
    fn advancing_cycles_through_every_demand_and_wraps() {
        let mut q = AttentionQueue::new();
        q.upsert(entry("sess_a", AwaitingReason::Permission, T0));
        q.upsert(entry("sess_b", AwaitingReason::Question, T0));
        q.upsert(entry("sess_c", AwaitingReason::Input, T0));

        let first = q.next(T0).unwrap().id.clone();
        let second = q.next_after(&first, T0).unwrap().id.clone();
        let third = q.next_after(&second, T0).unwrap().id.clone();
        let wrapped = q.next_after(&third, T0).unwrap().id.clone();

        assert_ne!(first, second);
        assert_ne!(second, third);
        assert_eq!(wrapped, first, "cycling wraps around");
    }

    #[test]
    fn aging_prevents_starvation_without_reordering_priority_classes() {
        let mut q = AttentionQueue::new();
        // An idle-input demand from an hour ago against a fresh permission.
        q.upsert(entry("sess_old", AwaitingReason::Input, T0 - 3_600_000));
        q.upsert(entry("sess_new", AwaitingReason::Permission, T0));

        assert_eq!(
            q.next(T0).unwrap().session_id.as_str(),
            "sess_new",
            "age must not let an idle prompt outrank a blocked permission"
        );

        // But against an equal-priority fresher demand, the old one wins.
        let mut q2 = AttentionQueue::new();
        q2.upsert(entry("sess_old", AwaitingReason::Input, T0 - 3_600_000));
        q2.upsert(entry("sess_new", AwaitingReason::Input, T0));
        assert_eq!(q2.next(T0).unwrap().session_id.as_str(), "sess_old");
    }

    #[test]
    fn provisional_demands_rank_below_confirmed_ones_of_the_same_kind() {
        let mut q = AttentionQueue::new();
        let mut guessed = entry("sess_guess", AwaitingReason::Question, T0);
        guessed.confidence = Confidence::InferredHigh;
        q.upsert(guessed);
        q.upsert(entry("sess_sure", AwaitingReason::Question, T0));

        assert_eq!(q.next(T0).unwrap().session_id.as_str(), "sess_sure");
    }

    #[test]
    fn upsert_upgrades_confidence_when_a_hook_confirms_a_guess() {
        let mut q = AttentionQueue::new();
        let mut guessed = entry("sess_a", AwaitingReason::Permission, T0);
        guessed.confidence = Confidence::InferredLow;
        let id = q.upsert(guessed);

        q.upsert(entry("sess_a", AwaitingReason::Permission, T0 + 100));
        assert_eq!(q.get(&id).unwrap().confidence, Confidence::Explicit);
        assert_eq!(q.len(), 1);
    }

    #[test]
    fn resolving_a_session_clears_all_its_demands_only() {
        let mut q = AttentionQueue::new();
        q.upsert(entry("sess_a", AwaitingReason::Permission, T0));
        q.upsert(entry("sess_a", AwaitingReason::Question, T0));
        q.upsert(entry("sess_b", AwaitingReason::Question, T0));

        assert_eq!(q.resolve_session(&SessionId::from_stored("sess_a")), 2);
        assert_eq!(q.len(), 1);
        assert_eq!(q.next(T0).unwrap().session_id.as_str(), "sess_b");
    }

    #[test]
    fn a_priority_boost_can_push_a_session_up_or_down() {
        let mut q = AttentionQueue::new();
        let mut boosted = entry("sess_boost", AwaitingReason::Input, T0);
        boosted.priority_boost = 50;
        q.upsert(boosted);
        q.upsert(entry("sess_plain", AwaitingReason::Question, T0));
        assert_eq!(q.next(T0).unwrap().session_id.as_str(), "sess_boost");
    }

    #[test]
    fn an_empty_queue_offers_nothing_rather_than_panicking() {
        let q = AttentionQueue::new();
        assert!(q.is_empty());
        assert!(q.next(T0).is_none());
        assert!(q.ordered(T0).is_empty());
        assert!(q.next_after(&AttentionId::new(), T0).is_none());
    }
}
