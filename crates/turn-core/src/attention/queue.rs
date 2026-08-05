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
use std::cmp::Ordering;

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

/// Semantic source of a queued demand.
///
/// Several events share the coarse `Input` priority but have different
/// lifecycle semantics. Persisting this kind lets failure supersede completion
/// and lets exact callback replays preserve acknowledgement/snooze state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttentionDemandKind {
    #[default]
    Interaction,
    TurnCompleted,
    TaskCompleted,
    AgentFailed,
    ProcessFailed,
}

/// One demand for the user's attention.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttentionEntry {
    pub id: AttentionId,
    pub session_id: SessionId,
    /// The specific process inside the session, so jumping lands on the right pane.
    pub node_id: Option<NodeId>,
    /// Authenticated runtime that received the callback when the callback could
    /// not identify its exact child. This is a correlation boundary, not the
    /// subject of the demand: focusing it must not pretend the parent asked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_node_id: Option<NodeId>,
    /// Tool-owned identity supplied by an out-of-order callback. It remains
    /// useful even before the matching AgentNode exists and prevents two unknown
    /// children beneath one parent from collapsing into one demand.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_external_id: Option<String>,
    pub reason: AwaitingReason,
    pub summary: Option<String>,
    pub confidence: Confidence,
    pub created_ms: i64,
    pub updated_ms: i64,
    pub state: EntryState,
    /// Session-level ranking adjustment, copied in at insert time.
    pub priority_boost: i16,
    /// Whether the demand remains meaningful after its owning runtime exits.
    ///
    /// Ordinary questions and permissions disappear with the process that could
    /// receive the answer. A process failure is different: it is post-mortem
    /// evidence for the user and must survive daemon restart until explicitly
    /// handled. Persisting the distinction avoids guessing from `reason` or text.
    #[serde(default)]
    pub survives_owner_exit: bool,
    #[serde(default)]
    pub demand_kind: AttentionDemandKind,
}

impl AttentionEntry {
    /// Stable identity for deduplication: the same agent blocking on the same
    /// kind of thing is one demand, however many times it says so.
    pub fn dedup_key(&self) -> String {
        let subject = match (
            self.node_id.as_ref(),
            self.parent_node_id.as_ref(),
            self.subject_external_id.as_deref(),
        ) {
            (Some(node), _, _) => format!("node:{node}"),
            (None, Some(parent), Some(external)) => {
                format!("parent:{parent}|external:{external}")
            }
            (None, Some(parent), None) => format!("parent:{parent}|unassigned"),
            (None, None, Some(external)) => format!("external:{external}|unanchored"),
            (None, None, None) => "unassigned".to_string(),
        };
        format!("{}|{}|{:?}", self.session_id, subject, self.reason)
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
            EntryState::Snoozed { until_ms } if now_ms >= until_ms => 0,
            // Larger than the complete i16 boost range plus every other score
            // component. The projected score therefore preserves the same
            // actionability classes as the queue comparator for clients that
            // keep an already-pushed list ordered locally.
            EntryState::Acknowledged => -100_000,
            EntryState::Snoozed { .. } => -200_000,
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

    /// Pending-like entries always precede acknowledged ones, independently of
    /// trigger priority or a session boost. An expired snooze is pending again.
    fn actionability_rank(&self, now_ms: i64) -> u8 {
        match self.state {
            EntryState::Pending => 2,
            EntryState::Snoozed { until_ms } if now_ms >= until_ms => 2,
            EntryState::Acknowledged => 1,
            EntryState::Snoozed { .. } => 0,
        }
    }
}

/// An ordered set of attention demands, deduplicated by session+node+reason.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
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
            // A different semantic demand can share the same queue key (for
            // example, a completed turn followed by an ordinary Input wait).
            // Persistence belongs to the current demand kind; inheriting it
            // would let that later live interaction survive its owner's exit.
            if existing.demand_kind == entry.demand_kind {
                existing.survives_owner_exit |= entry.survives_owner_exit;
            } else {
                existing.survives_owner_exit = entry.survives_owner_exit;
            }
            existing.demand_kind = entry.demand_kind;
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
        self.ordered(now_ms).first().copied()
    }

    /// All actionable demands, most urgent first. Drives the queue panel.
    pub fn ordered(&self, now_ms: i64) -> Vec<&AttentionEntry> {
        let mut visible: Vec<_> = self
            .entries
            .iter()
            .filter(|e| e.is_actionable(now_ms))
            .collect();
        visible.sort_by(|a, b| compare_entries(a, b, now_ms));
        visible
    }

    /// Every demand in the same order as [`Self::next`], with sleeping snoozes
    /// retained at the bottom for the queue panel.
    pub fn ordered_all(&self, now_ms: i64) -> Vec<&AttentionEntry> {
        let mut entries: Vec<_> = self.entries.iter().collect();
        entries.sort_by(|a, b| compare_entries(a, b, now_ms));
        entries
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

    /// Drops only the demand identified by a resolving event.
    ///
    /// A concrete node resolves that node. A node-less callback must carry its
    /// authenticated parent and resolves only the unresolved scope below that
    /// parent. An external id narrows the scope further. Nothing without either
    /// an exact node or parent is allowed to erase session-wide state.
    pub fn resolve_subject(
        &mut self,
        session: &SessionId,
        node: Option<&NodeId>,
        parent: Option<&NodeId>,
        external_id: Option<&str>,
    ) -> usize {
        self.resolve_subject_at_most(session, node, parent, external_id, Confidence::Explicit)
    }

    /// Drops matching evidence no stronger than the event withdrawing it.
    ///
    /// This is load-bearing for PTY heuristics: disappearance of a guessed
    /// prompt may retract that guess, but can never erase a hook-confirmed
    /// permission for the same node (or a sibling).
    pub fn resolve_subject_at_most(
        &mut self,
        session: &SessionId,
        node: Option<&NodeId>,
        parent: Option<&NodeId>,
        external_id: Option<&str>,
        confidence: Confidence,
    ) -> usize {
        let before = self.entries.len();
        self.entries.retain(|entry| {
            entry.confidence > confidence
                || !subject_is_resolved_by(entry, session, node, parent, external_id)
        });
        before - self.entries.len()
    }

    /// Drops only live interaction evidence for a matching subject.
    ///
    /// Runtime-idle callbacks close prompts, but are not an acknowledgement of
    /// a completed turn or failure that remains useful after the runtime exits.
    pub fn resolve_interaction_subject_at_most(
        &mut self,
        session: &SessionId,
        node: Option<&NodeId>,
        parent: Option<&NodeId>,
        external_id: Option<&str>,
        confidence: Confidence,
    ) -> usize {
        let before = self.entries.len();
        self.entries.retain(|entry| {
            entry.demand_kind != AttentionDemandKind::Interaction
                || entry.confidence > confidence
                || !subject_is_resolved_by(entry, session, node, parent, external_id)
        });
        before - self.entries.len()
    }

    /// Clears interaction demands for a subject while retaining post-mortem or
    /// completion evidence that does not depend on a live owner.
    pub fn prepare_informational_subject(
        &mut self,
        session: &SessionId,
        node: Option<&NodeId>,
        parent: Option<&NodeId>,
        external_id: Option<&str>,
        incoming_kind: AttentionDemandKind,
    ) -> usize {
        let before = self.entries.len();
        self.entries.retain(|entry| {
            (entry.survives_owner_exit && entry.demand_kind == incoming_kind)
                || !subject_is_resolved_by(entry, session, node, parent, external_id)
        });
        before - self.entries.len()
    }

    /// Whether a provisional, node-less demand already occupies this exact
    /// parent/external-id correlation scope.
    pub fn has_unresolved_scope(
        &self,
        session: &SessionId,
        parent: &NodeId,
        external_id: Option<&str>,
    ) -> bool {
        self.entries.iter().any(|entry| {
            entry.session_id == *session
                && entry.node_id.is_none()
                && entry.parent_node_id.as_ref() == Some(parent)
                && entry.subject_external_id.as_deref() == external_id
        })
    }

    /// Whether the exact subject behind a deferred focus is still actionable.
    pub fn has_actionable_subject(
        &self,
        session: &SessionId,
        node: Option<&NodeId>,
        parent: Option<&NodeId>,
        external_id: Option<&str>,
        now_ms: i64,
    ) -> bool {
        self.entries.iter().any(|entry| {
            entry.session_id == *session
                && entry.node_id.as_ref() == node
                && entry.parent_node_id.as_ref() == parent
                && entry.subject_external_id.as_deref() == external_id
                && entry.is_actionable(now_ms)
        })
    }

    /// Drops demands for one node, leaving its siblings alone.
    pub fn resolve_node(&mut self, node: &NodeId) -> usize {
        let before = self.entries.len();
        self.entries.retain(|e| e.node_id.as_ref() != Some(node));
        before - self.entries.len()
    }

    /// Drops demands owned by a runtime that has terminated.
    ///
    /// A node-less callback anchored at `node` belongs to that runtime even
    /// though its exact child was not known yet. Exact child demands are not
    /// owned by the parent and deliberately survive its exit.
    pub fn resolve_owner(&mut self, session: &SessionId, node: &NodeId) -> usize {
        let before = self.entries.len();
        self.entries
            .retain(|entry| !owner_is_resolved(entry, session, node));
        before - self.entries.len()
    }

    /// Drops one exact node inside its Session boundary.
    pub fn resolve_node_in_session(&mut self, session: &SessionId, node: &NodeId) -> usize {
        let before = self.entries.len();
        self.entries
            .retain(|entry| entry.session_id != *session || entry.node_id.as_ref() != Some(node));
        before - self.entries.len()
    }

    /// Removes every demand whose subject or unresolved owner is being deleted.
    /// Unlike lifecycle cleanup this is an explicit structural action, so even
    /// post-mortem evidence must not retain a dangling NodeId.
    pub fn remove_owner_in_session(&mut self, session: &SessionId, node: &NodeId) -> usize {
        let before = self.entries.len();
        self.entries.retain(|entry| {
            entry.session_id != *session
                || (entry.node_id.as_ref() != Some(node)
                    && entry.parent_node_id.as_ref() != Some(node))
        });
        before - self.entries.len()
    }

    /// Drops an exact lifecycle subject and its older out-of-order scope.
    ///
    /// This is intentionally separate from [`Self::resolve_subject`]. A process
    /// ending is an ownership fact, not evidence that the user answered another
    /// callback, so lifecycle correlation must not broaden response semantics.
    pub fn resolve_lifecycle_subject(
        &mut self,
        session: &SessionId,
        node: &NodeId,
        parent: Option<&NodeId>,
        external_id: Option<&str>,
    ) -> usize {
        let before = self.entries.len();
        self.entries.retain(|entry| {
            !lifecycle_subject_is_resolved(entry, session, node, parent, external_id)
        });
        before - self.entries.len()
    }

    /// Drops a terminal tool-owned scope whose AgentNode never materialised.
    pub fn resolve_lifecycle_scope(
        &mut self,
        session: &SessionId,
        parent: Option<&NodeId>,
        external_id: &str,
    ) -> usize {
        let before = self.entries.len();
        self.entries.retain(|entry| {
            entry.survives_owner_exit
                || entry.session_id != *session
                || entry.node_id.is_some()
                || entry.parent_node_id.as_ref() != parent
                || entry.subject_external_id.as_deref() != Some(external_id)
        });
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

    /// Keeps only demands that still have a live owner after durable state is
    /// reconciled with runtime state.
    ///
    /// This deliberately does not replay entries through [`Self::upsert`]: a
    /// restart must preserve every durable field, including acknowledgement,
    /// snooze deadline, age and identity. The predicate is only allowed to
    /// remove entries whose Session or live Process can no longer own a demand.
    pub fn retain(&mut self, mut keep: impl FnMut(&AttentionEntry) -> bool) {
        self.entries.retain(|entry| keep(entry));
    }
}

fn compare_entries(a: &AttentionEntry, b: &AttentionEntry, now_ms: i64) -> Ordering {
    b.actionability_rank(now_ms)
        .cmp(&a.actionability_rank(now_ms))
        .then_with(|| b.score(now_ms).cmp(&a.score(now_ms)))
        .then_with(|| a.created_ms.cmp(&b.created_ms))
        .then_with(|| a.id.as_str().cmp(b.id.as_str()))
}

/// Shared matching rule for queue entries and deferred focus requests.
///
/// An exact node may additionally close an older out-of-order demand carrying
/// the same parent and external id. That is not a session-wide guess: the tool's
/// own identity has become resolvable since the first callback arrived.
pub(super) struct SubjectRef<'a> {
    pub(super) session: &'a SessionId,
    pub(super) node: Option<&'a NodeId>,
    pub(super) parent: Option<&'a NodeId>,
    pub(super) external_id: Option<&'a str>,
}

pub(super) fn subject_is_resolved(candidate: SubjectRef<'_>, resolving: SubjectRef<'_>) -> bool {
    if candidate.session != resolving.session {
        return false;
    }

    if let Some(node) = resolving.node {
        if candidate.node == Some(node) {
            return true;
        }
        return candidate.node.is_none()
            && resolving.external_id.is_some()
            && candidate.parent == resolving.parent
            && candidate.external_id == resolving.external_id;
    }

    if let Some(parent) = resolving.parent {
        return candidate.node.is_none()
            && candidate.parent == Some(parent)
            && candidate.external_id == resolving.external_id;
    }

    // A tool-owned id is also an exact scope when no hook parent is available.
    // A completely anonymous event still resolves nothing.
    resolving.external_id.is_some()
        && candidate.node.is_none()
        && candidate.parent.is_none()
        && candidate.external_id == resolving.external_id
}

fn subject_is_resolved_by(
    entry: &AttentionEntry,
    session: &SessionId,
    node: Option<&NodeId>,
    parent: Option<&NodeId>,
    external_id: Option<&str>,
) -> bool {
    subject_is_resolved(
        SubjectRef {
            session: &entry.session_id,
            node: entry.node_id.as_ref(),
            parent: entry.parent_node_id.as_ref(),
            external_id: entry.subject_external_id.as_deref(),
        },
        SubjectRef {
            session,
            node,
            parent,
            external_id,
        },
    )
}

fn owner_is_resolved(entry: &AttentionEntry, session: &SessionId, owner: &NodeId) -> bool {
    !entry.survives_owner_exit
        && entry.session_id == *session
        && (entry.node_id.as_ref() == Some(owner)
            || (entry.node_id.is_none() && entry.parent_node_id.as_ref() == Some(owner)))
}

fn lifecycle_subject_is_resolved(
    entry: &AttentionEntry,
    session: &SessionId,
    node: &NodeId,
    parent: Option<&NodeId>,
    external_id: Option<&str>,
) -> bool {
    if entry.survives_owner_exit {
        return false;
    }
    if entry.session_id != *session {
        return false;
    }
    if entry.node_id.as_ref() == Some(node) {
        return true;
    }

    // An out-of-order callback can predate its AgentNode. Once lifecycle reports
    // the declared node together with the same tool-owned identity, that old
    // node-less scope is the same subject. The parent must match as an Option:
    // `None` is itself the boundary for an unanchored tool-owned identity.
    entry.node_id.is_none()
        && external_id.is_some()
        && entry.parent_node_id.as_ref() == parent
        && entry.subject_external_id.as_deref() == external_id
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
            parent_node_id: None,
            subject_external_id: None,
            reason,
            summary: None,
            confidence: Confidence::Explicit,
            created_ms,
            updated_ms: created_ms,
            state: EntryState::Pending,
            priority_boost: 0,
            survives_owner_exit: false,
            demand_kind: AttentionDemandKind::Interaction,
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
        // State class wins even across the largest possible policy boosts.
        let mut acknowledged = entry("sess_p", AwaitingReason::Permission, T0);
        acknowledged.priority_boost = i16::MAX;
        let permission = q.upsert(acknowledged);
        let mut pending = entry("sess_q", AwaitingReason::Input, T0);
        pending.priority_boost = i16::MIN;
        q.upsert(pending);
        q.acknowledge(&permission);

        assert_eq!(q.next(T0).unwrap().session_id.as_str(), "sess_q");
        assert_eq!(q.ordered(T0).len(), 2, "acknowledged is still listed");
    }

    #[test]
    fn an_expired_snooze_reenters_the_pending_class_without_starving() {
        let mut q = AttentionQueue::new();
        let due = q.upsert(entry("sess_due", AwaitingReason::Input, T0));
        q.snooze(&due, T0 + 1_000);
        let acknowledged = q.upsert(entry("sess_seen", AwaitingReason::Permission, T0));
        q.acknowledge(&acknowledged);

        assert_eq!(
            q.next(T0 + 1_000).unwrap().id,
            due,
            "an expired snooze is pending again and outranks seen work"
        );
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
    fn panel_order_and_next_share_one_stable_comparator() {
        let mut q = AttentionQueue::new();
        q.upsert(entry("sess_a", AwaitingReason::Question, T0));
        q.upsert(entry("sess_b", AwaitingReason::Question, T0));
        let snoozed = q.upsert(entry("sess_c", AwaitingReason::Permission, T0));
        q.snooze(&snoozed, T0 + 60_000);

        let all = q.ordered_all(T0);
        assert_eq!(q.next(T0).unwrap().id, all[0].id);
        assert_eq!(all.last().unwrap().id, snoozed);
        assert_eq!(q.ordered(T0)[0].id, all[0].id);
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
    fn resolving_an_unassigned_flow_is_scoped_to_its_parent() {
        let mut q = AttentionQueue::new();
        let parent_a = NodeId::from_stored("parent_a");
        let parent_b = NodeId::from_stored("parent_b");
        let mut under_a = entry("sess_a", AwaitingReason::Input, T0);
        under_a.parent_node_id = Some(parent_a.clone());
        q.upsert(under_a);
        let mut under_b = entry("sess_a", AwaitingReason::Input, T0);
        under_b.parent_node_id = Some(parent_b.clone());
        q.upsert(under_b);
        let mut reviewer = entry("sess_a", AwaitingReason::Permission, T0);
        reviewer.node_id = Some(NodeId::from_stored("reviewer"));
        q.upsert(reviewer);

        assert_eq!(
            q.resolve_subject(
                &SessionId::from_stored("sess_a"),
                None,
                Some(&parent_a),
                None,
            ),
            1
        );
        assert_eq!(q.len(), 2);
        assert!(q
            .iter()
            .any(|entry| entry.parent_node_id.as_ref() == Some(&parent_b)));
        assert!(q.iter().any(|entry| {
            entry
                .node_id
                .as_ref()
                .is_some_and(|node| node.as_str() == "reviewer")
        }));
    }

    #[test]
    fn unknown_external_subjects_do_not_deduplicate_or_resolve_each_other() {
        let mut q = AttentionQueue::new();
        let session = SessionId::from_stored("sess_a");
        let parent = NodeId::from_stored("parent_a");
        for external in ["future-reviewer", "existing-tests"] {
            let mut demand = entry("sess_a", AwaitingReason::Permission, T0);
            demand.parent_node_id = Some(parent.clone());
            demand.subject_external_id = Some(external.into());
            q.upsert(demand);
        }
        assert_eq!(q.len(), 2);

        assert_eq!(
            q.resolve_subject(&session, None, Some(&parent), Some("future-reviewer"),),
            1
        );
        let remaining = q.iter().next().unwrap();
        assert_eq!(
            remaining.subject_external_id.as_deref(),
            Some("existing-tests")
        );
    }

    #[test]
    fn an_exact_node_can_close_its_earlier_out_of_order_external_scope() {
        let mut q = AttentionQueue::new();
        let session = SessionId::from_stored("sess_a");
        let parent = NodeId::from_stored("parent_a");
        let reviewer = NodeId::from_stored("reviewer");
        let mut provisional = entry("sess_a", AwaitingReason::Permission, T0);
        provisional.parent_node_id = Some(parent.clone());
        provisional.subject_external_id = Some("future-reviewer".into());
        q.upsert(provisional);

        assert_eq!(
            q.resolve_subject(
                &session,
                Some(&reviewer),
                Some(&parent),
                Some("future-reviewer"),
            ),
            1
        );
        assert!(q.is_empty());
    }

    #[test]
    fn a_dead_owner_clears_its_unresolved_scopes_but_not_exact_children() {
        let mut q = AttentionQueue::new();
        let owner = NodeId::from_stored("parent_a");
        let other_parent = NodeId::from_stored("parent_b");
        let child = NodeId::from_stored("reviewer");

        let mut exact_owner = entry("sess_a", AwaitingReason::Question, T0);
        exact_owner.node_id = Some(owner.clone());
        q.upsert(exact_owner);
        let mut unresolved_owner = entry("sess_a", AwaitingReason::Permission, T0);
        unresolved_owner.parent_node_id = Some(owner.clone());
        unresolved_owner.subject_external_id = Some("future-reviewer".into());
        q.upsert(unresolved_owner);
        let mut exact_child = entry("sess_a", AwaitingReason::Permission, T0);
        exact_child.node_id = Some(child.clone());
        exact_child.parent_node_id = Some(owner.clone());
        q.upsert(exact_child);
        let mut unresolved_other = entry("sess_a", AwaitingReason::Permission, T0);
        unresolved_other.parent_node_id = Some(other_parent.clone());
        q.upsert(unresolved_other);

        assert_eq!(
            q.resolve_owner(&SessionId::from_stored("sess_a"), &owner),
            2
        );
        assert_eq!(q.len(), 2);
        assert!(q.iter().any(|entry| entry.node_id.as_ref() == Some(&child)));
        assert!(q.iter().any(|entry| {
            entry.node_id.is_none() && entry.parent_node_id.as_ref() == Some(&other_parent)
        }));
    }

    #[test]
    fn owner_cleanup_never_crosses_a_session_boundary_even_when_node_ids_match() {
        let mut q = AttentionQueue::new();
        let shared = NodeId::from_stored("imported-node");
        for session in ["sess_a", "sess_b"] {
            let mut demand = entry(session, AwaitingReason::Permission, T0);
            demand.node_id = Some(shared.clone());
            q.upsert(demand);
        }

        assert_eq!(
            q.resolve_owner(&SessionId::from_stored("sess_a"), &shared),
            1
        );
        assert_eq!(q.len(), 1);
        assert_eq!(q.iter().next().unwrap().session_id.as_str(), "sess_b");
    }

    #[test]
    fn lifecycle_keeps_postmortem_evidence_but_explicit_deletion_removes_it() {
        let mut q = AttentionQueue::new();
        let session = SessionId::from_stored("sess_a");
        let node = NodeId::from_stored("agent_a");
        let mut failure = entry("sess_a", AwaitingReason::Input, T0);
        failure.node_id = Some(node.clone());
        failure.survives_owner_exit = true;
        failure.demand_kind = AttentionDemandKind::ProcessFailed;
        q.upsert(failure);

        assert_eq!(q.resolve_owner(&session, &node), 0);
        assert_eq!(q.len(), 1);
        assert_eq!(q.remove_owner_in_session(&session, &node), 1);
        assert!(q.is_empty());
    }

    #[test]
    fn lifecycle_correlation_closes_predeclaration_and_unanchored_external_scopes() {
        let session = SessionId::from_stored("sess_a");
        let parent = NodeId::from_stored("parent_a");
        let reviewer = NodeId::from_stored("reviewer");

        let mut anchored = AttentionQueue::new();
        let mut demand = entry("sess_a", AwaitingReason::Permission, T0);
        demand.parent_node_id = Some(parent.clone());
        demand.subject_external_id = Some("future-reviewer".into());
        anchored.upsert(demand);
        assert_eq!(
            anchored.resolve_lifecycle_subject(
                &session,
                &reviewer,
                Some(&parent),
                Some("future-reviewer"),
            ),
            1
        );

        let mut unanchored = AttentionQueue::new();
        let mut demand = entry("sess_a", AwaitingReason::Permission, T0);
        demand.subject_external_id = Some("future-reviewer".into());
        unanchored.upsert(demand);
        assert_eq!(
            unanchored.resolve_lifecycle_subject(
                &session,
                &reviewer,
                None,
                Some("future-reviewer"),
            ),
            1
        );
        assert!(unanchored.is_empty());
    }

    #[test]
    fn terminal_external_scope_can_close_before_its_node_is_declared() {
        let mut queue = AttentionQueue::new();
        let session = SessionId::from_stored("sess_a");
        let parent = NodeId::from_stored("claude");
        let mut reviewer = entry("sess_a", AwaitingReason::Permission, T0);
        reviewer.parent_node_id = Some(parent.clone());
        reviewer.subject_external_id = Some("worker-reviewer".into());
        queue.upsert(reviewer);
        let mut sibling = entry("sess_a", AwaitingReason::Permission, T0);
        sibling.parent_node_id = Some(parent.clone());
        sibling.subject_external_id = Some("worker-tests".into());
        queue.upsert(sibling);

        assert_eq!(
            queue.resolve_lifecycle_scope(&session, Some(&parent), "worker-reviewer"),
            1
        );
        assert_eq!(queue.len(), 1);
        assert_eq!(
            queue.iter().next().unwrap().subject_external_id.as_deref(),
            Some("worker-tests")
        );
    }

    #[test]
    fn an_unanchored_node_less_resume_resolves_nothing() {
        let mut q = AttentionQueue::new();
        let session = SessionId::from_stored("sess_a");
        let parent = NodeId::from_stored("parent_a");
        let mut demand = entry("sess_a", AwaitingReason::Input, T0);
        demand.parent_node_id = Some(parent);
        q.upsert(demand);

        assert_eq!(q.resolve_subject(&session, None, None, None), 0);
        assert_eq!(q.len(), 1);
    }

    #[test]
    fn an_unanchored_external_id_is_still_an_exact_scope() {
        let mut q = AttentionQueue::new();
        let session = SessionId::from_stored("sess_a");
        for external in ["worker-a", "worker-b"] {
            let mut demand = entry("sess_a", AwaitingReason::Input, T0);
            demand.subject_external_id = Some(external.into());
            q.upsert(demand);
        }

        assert_eq!(q.resolve_subject(&session, None, None, Some("worker-a")), 1);
        assert_eq!(
            q.iter().next().unwrap().subject_external_id.as_deref(),
            Some("worker-b")
        );
    }

    #[test]
    fn legacy_serialised_entries_default_to_no_correlation_scope() {
        let mut scoped = entry("sess_a", AwaitingReason::Input, T0);
        scoped.parent_node_id = Some(NodeId::from_stored("parent_a"));
        scoped.subject_external_id = Some("worker-a".into());
        let mut wire = serde_json::to_value(scoped).unwrap();
        let object = wire.as_object_mut().unwrap();
        object.remove("parent_node_id");
        object.remove("subject_external_id");
        object.remove("survives_owner_exit");
        object.remove("demand_kind");

        let legacy: AttentionEntry = serde_json::from_value(wire).unwrap();
        assert_eq!(legacy.parent_node_id, None);
        assert_eq!(legacy.subject_external_id, None);
        assert!(!legacy.survives_owner_exit);
        assert_eq!(legacy.demand_kind, AttentionDemandKind::Interaction);
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
