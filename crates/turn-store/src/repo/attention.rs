//! The attention queue, persisted.
//!
//! A demand for the user's attention is the one piece of live state that must not
//! evaporate on a restart: an agent that blocked on a permission at 17:58 is
//! still blocked at 18:02, and a queue rebuilt from nothing would quietly drop it
//! until the agent happened to say so again.

use crate::codec::{from_json, from_tag, json, tag};
use crate::error::{Result, StoreError};
use crate::redact::redact_secrets;
use rusqlite::{params, Connection, OptionalExtension, Row};
use turn_core::attention::{AttentionEntry, AttentionQueue, EntryState};
use turn_core::ids::{AttentionId, NodeId, SessionId};
use turn_core::state::AwaitingReason;
use turn_core::Confidence;

const COLUMNS: &str = "id, session_id, node_id, reason, summary, confidence, created_ms, \
     updated_ms, state_json, priority_boost";

pub struct AttentionRepo<'a> {
    conn: &'a Connection,
}

impl<'a> AttentionRepo<'a> {
    pub(crate) fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Stores a demand, collapsing it into the existing one it duplicates.
    ///
    /// Mirrors `AttentionQueue::upsert`, including the two parts that matter: the
    /// stored `created_ms` and id are kept, so a chatty agent cannot reset its own
    /// age and jump the queue by asking again, and confidence only ever rises.
    /// Returns the id now on record — which is the pre-existing one when a
    /// duplicate collapsed.
    pub fn upsert(&self, entry: &AttentionEntry) -> Result<AttentionId> {
        let tx = self.conn.unchecked_transaction()?;
        let id = upsert_entry(&tx, entry)?;
        tx.commit()?;
        Ok(id)
    }

    pub fn get(&self, id: &AttentionId) -> Result<Option<AttentionEntry>> {
        let sql = format!("SELECT {COLUMNS} FROM attention_entries WHERE id = ?1");
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query(params![id.as_str()])?;
        match rows.next()? {
            Some(row) => Ok(Some(from_row(row)?)),
            None => Ok(None),
        }
    }

    /// Every stored demand, oldest first.
    ///
    /// Ordering is by age rather than by score on purpose: scoring depends on the
    /// current time, and the store does not read the clock.
    pub fn list(&self) -> Result<Vec<AttentionEntry>> {
        self.query(
            &format!("SELECT {COLUMNS} FROM attention_entries ORDER BY created_ms ASC, id ASC"),
            params![],
        )
    }

    pub fn list_for_session(&self, session: &SessionId) -> Result<Vec<AttentionEntry>> {
        self.query(
            &format!(
                "SELECT {COLUMNS} FROM attention_entries WHERE session_id = ?1 \
                 ORDER BY created_ms ASC, id ASC"
            ),
            params![session.as_str()],
        )
    }

    /// Rebuilds the in-memory queue from storage.
    pub fn load_queue(&self) -> Result<AttentionQueue> {
        let mut queue = AttentionQueue::new();
        for entry in self.list()? {
            queue.upsert(entry);
        }
        Ok(queue)
    }

    /// Replaces everything stored with the contents of a queue.
    ///
    /// Used when the daemon shuts down or checkpoints: the in-memory queue is
    /// authoritative, so demands it has resolved must not come back to life. One
    /// transaction, so a failure halfway cannot leave the user with an empty
    /// queue and no idea that three agents are blocked.
    pub fn replace_all(&self, queue: &AttentionQueue) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM attention_entries", [])?;
        for entry in queue.iter() {
            upsert_entry(&tx, entry)?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn delete(&self, id: &AttentionId) -> Result<bool> {
        let removed = self.conn.execute(
            "DELETE FROM attention_entries WHERE id = ?1",
            params![id.as_str()],
        )?;
        Ok(removed > 0)
    }

    /// Clears a session's demands: the user has engaged with it, or it died.
    pub fn clear_session(&self, session: &SessionId) -> Result<usize> {
        let removed = self.conn.execute(
            "DELETE FROM attention_entries WHERE session_id = ?1",
            params![session.as_str()],
        )?;
        Ok(removed)
    }

    pub fn count(&self) -> Result<usize> {
        let count: i64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM attention_entries", [], |row| {
                    row.get(0)
                })?;
        Ok(count as usize)
    }

    fn query(&self, sql: &str, args: impl rusqlite::Params) -> Result<Vec<AttentionEntry>> {
        let mut stmt = self.conn.prepare(sql)?;
        let mut rows = stmt.query(args)?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(from_row(row)?);
        }
        Ok(out)
    }
}

/// Writes one demand without opening a transaction, so a caller can batch.
fn upsert_entry(conn: &Connection, entry: &AttentionEntry) -> Result<AttentionId> {
    let key = entry.dedup_key();

    // The same demand may have been re-keyed (its reason changed, say). Drop the
    // stale row first so the id can be reused under the new key instead of
    // colliding with it.
    conn.execute(
        "DELETE FROM attention_entries WHERE id = ?1 AND dedup_key <> ?2",
        params![entry.id.as_str(), key],
    )?;

    // The summary is agent-supplied text — the command line a permission request
    // is about, the question an agent asked — so it carries credentials for
    // exactly the reasons the event log's payloads do, and gets exactly the same
    // scan before it reaches a column.
    let summary = entry.summary.as_deref().map(redact_secrets);
    let confidence = confidence_to_store(conn, &key, entry.confidence)?;

    conn.execute(
        "INSERT INTO attention_entries (id, session_id, node_id, reason, summary, \
             confidence, created_ms, updated_ms, state_json, priority_boost, dedup_key) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11) \
         ON CONFLICT(dedup_key) DO UPDATE SET \
             summary = COALESCE(excluded.summary, summary), \
             confidence = excluded.confidence, updated_ms = excluded.updated_ms, \
             state_json = excluded.state_json, priority_boost = excluded.priority_boost",
        params![
            entry.id.as_str(),
            entry.session_id.as_str(),
            entry.node_id.as_ref().map(|n| n.as_str()),
            tag("attention reason", &entry.reason)?,
            summary,
            tag("confidence", &confidence)?,
            entry.created_ms,
            entry.updated_ms,
            json("attention state", &entry.state)?,
            entry.priority_boost,
            key,
        ],
    )
    .map_err(|error| StoreError::from_write("attention entry", entry.session_id.as_str(), error))?;

    let stored: String = conn.query_row(
        "SELECT id FROM attention_entries WHERE dedup_key = ?1",
        params![key],
        |row| row.get(0),
    )?;
    Ok(AttentionId::from_stored(stored))
}

/// The confidence a duplicate demand may be stored with, which is never lower
/// than the one already on record.
///
/// `AttentionQueue::upsert` raises confidence and never lowers it, and this row is
/// the same demand: a permission the tool itself reported must not become a guess
/// because a pty rule matched the same screen a moment later. Writing
/// `excluded.confidence` did precisely that, and a downgraded demand loses the
/// right to move the user's focus — the product rule inverted by a storage detail.
///
/// The comparison happens here rather than in SQL so the ordering stays
/// [`Confidence`]'s own. A stored tag no build can decode is treated as no floor:
/// the row is being rewritten regardless, and refusing the write would drop a live
/// demand over a column nothing can read.
fn confidence_to_store(
    conn: &Connection,
    dedup_key: &str,
    incoming: Confidence,
) -> Result<Confidence> {
    let stored: Option<String> = conn
        .query_row(
            "SELECT confidence FROM attention_entries WHERE dedup_key = ?1",
            params![dedup_key],
            |row| row.get(0),
        )
        .optional()?;
    let Some(stored) = stored else {
        return Ok(incoming);
    };
    match from_tag::<Confidence>("confidence", dedup_key, &stored) {
        Ok(previous) => Ok(incoming.max(previous)),
        Err(_) => Ok(incoming),
    }
}

fn from_row(row: &Row<'_>) -> Result<AttentionEntry> {
    let id: String = row.get("id")?;
    Ok(AttentionEntry {
        id: AttentionId::from_stored(id.clone()),
        session_id: SessionId::from_stored(row.get::<_, String>("session_id")?),
        node_id: row
            .get::<_, Option<String>>("node_id")?
            .map(NodeId::from_stored),
        reason: from_tag::<AwaitingReason>(
            "attention reason",
            &id,
            &row.get::<_, String>("reason")?,
        )?,
        summary: row.get("summary")?,
        confidence: from_tag::<Confidence>(
            "confidence",
            &id,
            &row.get::<_, String>("confidence")?,
        )?,
        created_ms: row.get("created_ms")?,
        updated_ms: row.get("updated_ms")?,
        state: from_json::<EntryState>(
            "attention state",
            &id,
            &row.get::<_, String>("state_json")?,
        )?,
        priority_boost: row.get("priority_boost")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing;

    const T0: i64 = 1_700_000_000_000;

    fn entry(session: &SessionId, reason: AwaitingReason, created_ms: i64) -> AttentionEntry {
        AttentionEntry {
            id: AttentionId::new(),
            session_id: session.clone(),
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

    #[test]
    fn a_demand_round_trips_with_its_reason_confidence_and_boost() {
        let store = testing::store();
        let session = testing::saved_session_anywhere(&store, "blocked");
        let mut demand = entry(&session.id, AwaitingReason::Permission, T0);
        demand.node_id = Some(NodeId::from_stored("proc_agent"));
        demand.summary = Some("run make verify".into());
        demand.confidence = Confidence::InferredHigh;
        demand.priority_boost = -20;
        demand.updated_ms = T0 + 500;

        store.attention().upsert(&demand).unwrap();
        let back = store.attention().get(&demand.id).unwrap().expect("stored");
        assert_eq!(back, demand);
    }

    #[test]
    fn a_snoozed_demand_keeps_its_deadline_across_a_restart() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("turn.db");
        let session_id;
        {
            let store = crate::Store::open_at(&path).unwrap();
            let session = testing::saved_session_anywhere(&store, "snoozed");
            session_id = session.id.clone();
            let mut demand = entry(&session.id, AwaitingReason::Question, T0);
            demand.state = EntryState::Snoozed {
                until_ms: T0 + 600_000,
            };
            store.attention().upsert(&demand).unwrap();
        }

        let store = crate::Store::open_at(&path).unwrap();
        let queue = store.attention().load_queue().unwrap();
        assert_eq!(queue.len(), 1);
        assert!(
            queue.next(T0 + 1_000).is_none(),
            "a snooze that outlives the daemon is still a snooze"
        );
        let woken = queue.next(T0 + 600_000).expect("and it wakes on time");
        assert_eq!(woken.session_id, session_id);
    }

    #[test]
    fn re_saving_the_same_demand_keeps_its_original_identity_and_age() {
        let store = testing::store();
        let session = testing::saved_session_anywhere(&store, "chatty");
        let first = entry(&session.id, AwaitingReason::Question, T0);
        let stored_id = store.attention().upsert(&first).unwrap();
        assert_eq!(stored_id, first.id);

        // The agent asks again, ten minutes later, with a fresh entry object.
        let mut again = entry(&session.id, AwaitingReason::Question, T0 + 600_000);
        again.updated_ms = T0 + 600_000;
        again.summary = Some("still waiting".into());
        let second_id = store.attention().upsert(&again).unwrap();

        assert_eq!(store.attention().count().unwrap(), 1, "one demand, not two");
        assert_eq!(second_id, first.id, "the original identity survives");
        let back = store.attention().get(&first.id).unwrap().unwrap();
        assert_eq!(back.created_ms, T0, "age cannot be reset by asking again");
        assert_eq!(back.updated_ms, T0 + 600_000, "recency still moves");
        assert_eq!(back.summary.as_deref(), Some("still waiting"));
    }

    #[test]
    fn a_repeat_that_carries_no_summary_does_not_erase_the_one_stored() {
        let store = testing::store();
        let session = testing::saved_session_anywhere(&store, "summary");
        let mut first = entry(&session.id, AwaitingReason::Permission, T0);
        first.summary = Some("run make verify".into());
        store.attention().upsert(&first).unwrap();

        let bare = entry(&session.id, AwaitingReason::Permission, T0 + 1_000);
        store.attention().upsert(&bare).unwrap();

        let back = store.attention().get(&first.id).unwrap().unwrap();
        assert_eq!(back.summary.as_deref(), Some("run make verify"));
    }

    /// The summary is written by a model: it is the command a permission request
    /// is about, or the question an agent asked. `gh auth login --with-token …` is
    /// an ordinary thing to be asked to approve, and the token in it must not
    /// outlive the moment on disk.
    #[test]
    fn a_credential_inside_a_permission_summary_never_reaches_the_column() {
        let store = testing::store();
        let session = testing::saved_session_anywhere(&store, "leaky summary");
        let secret = "ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
        let mut demand = entry(&session.id, AwaitingReason::Permission, T0);
        demand.summary = Some(format!("Run `gh auth login --with-token {secret}`"));
        store.attention().upsert(&demand).unwrap();

        let stored: String = store
            .connection()
            .query_row(
                "SELECT summary FROM attention_entries WHERE id = ?1",
                [demand.id.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!stored.contains(secret), "got {stored}");
        assert!(
            stored.contains("gh auth login"),
            "what the user needs in order to recognise the demand survives: {stored}"
        );

        let back = store.attention().get(&demand.id).unwrap().unwrap();
        assert!(!back.summary.unwrap().contains(secret));
    }

    /// A guess arriving after a confirmation must not take the demand's authority
    /// away: `Confidence::Explicit` may move the user's focus and
    /// `Confidence::InferredLow` may not, so a downgrade here silently demotes a
    /// real permission request to a badge.
    #[test]
    fn a_later_guess_never_downgrades_a_demand_the_tool_itself_reported() {
        let store = testing::store();
        let session = testing::saved_session_anywhere(&store, "confirmed then guessed");
        let mut confirmed = entry(&session.id, AwaitingReason::Permission, T0);
        confirmed.confidence = Confidence::Explicit;
        store.attention().upsert(&confirmed).unwrap();

        let mut guessed = entry(&session.id, AwaitingReason::Permission, T0 + 1_000);
        guessed.confidence = Confidence::InferredLow;
        guessed.updated_ms = T0 + 1_000;
        store.attention().upsert(&guessed).unwrap();

        let back = store.attention().get(&confirmed.id).unwrap().unwrap();
        assert_eq!(back.confidence, Confidence::Explicit);
        assert!(
            back.confidence.may_steal_focus(),
            "a pty rule must not take a hook's word away"
        );
        assert_eq!(back.updated_ms, T0 + 1_000, "recency still moves");
    }

    /// The other direction, which is the whole reason confidence is stored: the
    /// hook that confirms a guess upgrades it.
    #[test]
    fn a_hook_confirming_a_guess_upgrades_the_stored_confidence() {
        let store = testing::store();
        let session = testing::saved_session_anywhere(&store, "guessed then confirmed");
        let mut guessed = entry(&session.id, AwaitingReason::Permission, T0);
        guessed.confidence = Confidence::InferredLow;
        store.attention().upsert(&guessed).unwrap();

        let mut confirmed = entry(&session.id, AwaitingReason::Permission, T0 + 1_000);
        confirmed.confidence = Confidence::Explicit;
        store.attention().upsert(&confirmed).unwrap();

        let back = store.attention().get(&guessed.id).unwrap().unwrap();
        assert_eq!(back.confidence, Confidence::Explicit);
    }

    #[test]
    fn two_subagents_blocked_in_one_session_are_two_stored_demands() {
        let store = testing::store();
        let session = testing::saved_session_anywhere(&store, "subagents");
        let mut first = entry(&session.id, AwaitingReason::Question, T0);
        first.node_id = Some(NodeId::from_stored("proc_one"));
        let mut second = entry(&session.id, AwaitingReason::Question, T0);
        second.node_id = Some(NodeId::from_stored("proc_two"));

        store.attention().upsert(&first).unwrap();
        store.attention().upsert(&second).unwrap();

        assert_eq!(store.attention().count().unwrap(), 2);
        assert_eq!(
            store
                .attention()
                .list_for_session(&session.id)
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn a_reloaded_queue_orders_demands_the_same_way_the_live_one_did() {
        let store = testing::store();
        let a = testing::saved_session_anywhere(&store, "idle prompt");
        let b = testing::saved_session_anywhere(&store, "blocked permission");
        let c = testing::saved_session_anywhere(&store, "question");
        store
            .attention()
            .upsert(&entry(&a.id, AwaitingReason::Input, T0))
            .unwrap();
        store
            .attention()
            .upsert(&entry(&b.id, AwaitingReason::Permission, T0))
            .unwrap();
        store
            .attention()
            .upsert(&entry(&c.id, AwaitingReason::Question, T0))
            .unwrap();

        let queue = store.attention().load_queue().unwrap();
        let order: Vec<SessionId> = queue
            .ordered(T0)
            .iter()
            .map(|e| e.session_id.clone())
            .collect();
        assert_eq!(order, vec![b.id, c.id, a.id], "permission first, idle last");
    }

    #[test]
    fn replacing_everything_drops_demands_the_queue_has_resolved() {
        let store = testing::store();
        let session = testing::saved_session_anywhere(&store, "resolved");
        store
            .attention()
            .upsert(&entry(&session.id, AwaitingReason::Permission, T0))
            .unwrap();
        store
            .attention()
            .upsert(&entry(&session.id, AwaitingReason::Question, T0))
            .unwrap();

        let mut queue = AttentionQueue::new();
        queue.upsert(entry(&session.id, AwaitingReason::Credentials, T0 + 10));
        store.attention().replace_all(&queue).unwrap();

        let stored = store.attention().list().unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].reason, AwaitingReason::Credentials);
    }

    #[test]
    fn a_dead_session_takes_its_demands_with_it() {
        let store = testing::store();
        let session = testing::saved_session_anywhere(&store, "dying");
        store
            .attention()
            .upsert(&entry(&session.id, AwaitingReason::Permission, T0))
            .unwrap();

        assert_eq!(store.attention().clear_session(&session.id).unwrap(), 1);
        assert_eq!(store.attention().count().unwrap(), 0);

        store
            .attention()
            .upsert(&entry(&session.id, AwaitingReason::Question, T0))
            .unwrap();
        store.sessions().delete(&session.id).unwrap();
        assert_eq!(
            store.attention().count().unwrap(),
            0,
            "cascade, so no demand outlives the thing it points at"
        );
    }

    #[test]
    fn a_demand_for_an_unknown_session_is_refused() {
        let store = testing::store();
        let error = store
            .attention()
            .upsert(&entry(
                &SessionId::from_stored("sess_ghost"),
                AwaitingReason::Permission,
                T0,
            ))
            .expect_err("nothing to jump to");
        assert!(
            matches!(error, StoreError::UnknownReference { .. }),
            "got {error:?}"
        );
    }
}
