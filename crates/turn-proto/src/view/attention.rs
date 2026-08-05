//! The attention queue as the queue panel draws it.

use serde::{Deserialize, Serialize};
use turn_core::attention::{AttentionEntry, AttentionQueue};
use turn_core::ids::SessionId;

/// One demand for the user's attention, ready to render.
///
/// The [`AttentionEntry`] is embedded whole rather than re-described: it is
/// already the domain's answer to "what is being asked and how urgently". The
/// added fields are the two things the UI cannot work out on its own — the
/// session's name, which lives elsewhere, and the ranking score, which is a
/// function of policy the client must not reimplement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AttentionView {
    pub entry: AttentionEntry,
    /// The session's name, so the queue reads as "Fix the flaky test needs
    /// permission" rather than as an id.
    pub session_name: String,
    /// Whether this demand came from a heuristic and must be shown as a guess.
    /// Derived from the entry's confidence so the rule stays in one place.
    pub provisional: bool,
    /// The queue score at the moment of projection. Sent so a client can keep a
    /// list ordered locally between pushes; it is not stable across time, because
    /// the age bonus grows.
    pub score: i32,
    /// Whether this demand is eligible right now. A snoozed entry is listed —
    /// hiding it would make the snooze feel like a deletion — but greyed out.
    pub actionable: bool,
}

impl AttentionView {
    pub fn from_entry(
        entry: &AttentionEntry,
        session_name: impl Into<String>,
        now_ms: i64,
    ) -> Self {
        Self {
            entry: entry.clone(),
            session_name: session_name.into(),
            provisional: entry.confidence.is_provisional(),
            score: entry.score(now_ms),
            actionable: entry.is_actionable(now_ms),
        }
    }

    /// Projects the whole queue in the daemon's own order, most urgent first.
    ///
    /// `name_of` resolves a session id to its name; unknown sessions fall back to
    /// their id rather than to an empty row.
    pub fn from_queue<F>(queue: &AttentionQueue, now_ms: i64, mut name_of: F) -> Vec<AttentionView>
    where
        F: FnMut(&SessionId) -> Option<String>,
    {
        queue
            .ordered_all(now_ms)
            .into_iter()
            .map(|entry| {
                let name = name_of(&entry.session_id)
                    .unwrap_or_else(|| entry.session_id.as_str().to_string());
                AttentionView::from_entry(entry, name, now_ms)
            })
            .collect()
    }

    pub fn session_id(&self) -> &SessionId {
        &self.entry.session_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use turn_core::attention::EntryState;
    use turn_core::event::Confidence;
    use turn_core::ids::AttentionId;
    use turn_core::state::AwaitingReason;

    const T0: i64 = 1_700_000_000_000;

    fn entry(session: &str, reason: AwaitingReason, confidence: Confidence) -> AttentionEntry {
        AttentionEntry {
            id: AttentionId::new(),
            session_id: SessionId::from_stored(session),
            node_id: None,
            parent_node_id: None,
            subject_external_id: None,
            reason,
            summary: Some("run make verify".into()),
            confidence,
            created_ms: T0,
            updated_ms: T0,
            state: EntryState::Pending,
            priority_boost: 0,
            survives_owner_exit: false,
            demand_kind: Default::default(),
        }
    }

    #[test]
    fn a_guessed_demand_is_projected_as_provisional() {
        let guessed = entry(
            "sess_a",
            AwaitingReason::Permission,
            Confidence::InferredHigh,
        );
        let view = AttentionView::from_entry(&guessed, "Guessy session", T0);
        assert!(
            view.provisional,
            "a heuristic demand must be visibly a guess in the queue panel"
        );

        let told = entry("sess_b", AwaitingReason::Permission, Confidence::Explicit);
        assert!(!AttentionView::from_entry(&told, "Certain session", T0).provisional);
    }

    #[test]
    fn the_queue_projects_in_the_daemons_order_not_the_clients() {
        let mut queue = AttentionQueue::new();
        queue.upsert(entry(
            "sess_idle",
            AwaitingReason::Input,
            Confidence::Explicit,
        ));
        queue.upsert(entry(
            "sess_blocked",
            AwaitingReason::Permission,
            Confidence::Explicit,
        ));
        queue.upsert(entry(
            "sess_ask",
            AwaitingReason::Question,
            Confidence::Explicit,
        ));

        let names = |id: &SessionId| Some(format!("name of {id}"));
        let views = AttentionView::from_queue(&queue, T0, names);

        let order: Vec<&str> = views.iter().map(|v| v.session_id().as_str()).collect();
        assert_eq!(order, vec!["sess_blocked", "sess_ask", "sess_idle"]);
        assert_eq!(views[0].session_name, "name of sess_blocked");
        // Scores must be consistent with the order the daemon chose.
        assert!(views[0].score > views[1].score);
        assert!(views[1].score > views[2].score);
    }

    #[test]
    fn a_session_whose_name_is_unknown_falls_back_to_its_id() {
        let mut queue = AttentionQueue::new();
        queue.upsert(entry(
            "sess_orphan",
            AwaitingReason::Question,
            Confidence::Explicit,
        ));
        let views = AttentionView::from_queue(&queue, T0, |_| None);
        assert_eq!(views[0].session_name, "sess_orphan");
    }

    #[test]
    fn a_snoozed_demand_is_still_listed_but_marked_unactionable() {
        let mut snoozed = entry("sess_later", AwaitingReason::Question, Confidence::Explicit);
        snoozed.state = EntryState::Snoozed {
            until_ms: T0 + 60_000,
        };
        let view = AttentionView::from_entry(&snoozed, "Later", T0);
        assert!(!view.actionable, "a snoozed demand is not offered");

        let after = AttentionView::from_entry(&snoozed, "Later", T0 + 60_000);
        assert!(after.actionable, "and comes back on time");
    }

    #[test]
    fn the_whole_queue_projection_keeps_sleeping_snoozes_at_the_bottom() {
        let mut queue = AttentionQueue::new();
        let mut snoozed = entry(
            "sess_later",
            AwaitingReason::Permission,
            Confidence::Explicit,
        );
        snoozed.state = EntryState::Snoozed {
            until_ms: T0 + 60_000,
        };
        queue.upsert(snoozed);
        queue.upsert(entry(
            "sess_now",
            AwaitingReason::Input,
            Confidence::Explicit,
        ));

        let views = AttentionView::from_queue(&queue, T0, |_| None);
        assert_eq!(views.len(), 2);
        assert_eq!(views[0].session_id().as_str(), "sess_now");
        assert!(views[0].actionable);
        assert_eq!(views[1].session_id().as_str(), "sess_later");
        assert!(!views[1].actionable);
    }

    #[test]
    fn an_empty_queue_projects_to_no_rows() {
        assert!(AttentionView::from_queue(&AttentionQueue::new(), T0, |_| None).is_empty());
    }

    #[test]
    fn an_attention_view_round_trips_with_the_embedded_domain_entry() {
        let view = AttentionView::from_entry(
            &entry("sess_a", AwaitingReason::Permission, Confidence::Explicit),
            "Fix the flaky test",
            T0,
        );
        let json = serde_json::to_string(&view).unwrap();
        assert!(json.contains("\"reason\":\"permission\""), "got {json}");
        assert!(json.contains("\"provisional\":false"));
        assert_eq!(serde_json::from_str::<AttentionView>(&json).unwrap(), view);
    }
}
