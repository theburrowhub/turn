//! The event log, and the retention that keeps it from eating the disk.
//!
//! Events are append-only. Nothing rewrites one: a wrong state is corrected by a
//! new event with [`turn_core::EventSource::UserCorrection`], which is what makes
//! the log a usable account of what Turn believed and when.
//!
//! Every row keeps its [`turn_core::Confidence`] and its source, so weeks later
//! Turn can still say "the session read as waiting for you because a pty rule
//! matched, not because the tool said so".
//!
//! A hook callback is deliberately *not* part of that durable account. It is
//! hostile ingress that adapters reduce to typed [`turn_core::EventKind`] facts.
//! The source still records which tool and hook supplied those facts, while the
//! callback body is discarded even if a caller accidentally leaves it in
//! [`turn_core::TurnEvent::raw`]. Arbitrary free text cannot be proven safe by a
//! credential scanner.

use crate::codec::{from_json, from_tag, json, tag};
use crate::error::{Result, StoreError};
use crate::redact::{redact_json, redact_secrets};
use rusqlite::{params, Connection, Row};
use turn_core::event::{event_name, AgentRef, EventKind};
use turn_core::ids::{EventId, NodeId, SessionId, WorkspaceId};
use turn_core::{Confidence, EventSource, Severity, TurnEvent};

const COLUMNS: &str = "id, timestamp_ms, workspace_id, session_id, node_id, parent_node_id, \
     kind_slug, kind_json, agent_json, confidence, source_json, severity, dedup_key, raw";

/// How much history to keep.
///
/// Age and count are independent limits and both are optional, because the two
/// failure modes are different: a machine left running for a year needs the age
/// limit, and a runaway adapter emitting thousands of events an hour needs the
/// count limit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Retention {
    /// Drop events older than this. `None` keeps everything, however old.
    pub max_age_ms: Option<i64>,
    /// Keep at most this many events in total, newest first. `None` is unbounded.
    pub max_events: Option<usize>,
    /// Never drop a session below this many events, however old they are or
    /// however full the log is.
    ///
    /// The floor is what makes pruning safe to run unattended: without it, a
    /// month away from a machine would erase the entire history of the session
    /// the user is about to open and ask "what happened here?".
    pub keep_per_session: usize,
}

impl Default for Retention {
    fn default() -> Self {
        Self {
            // Long enough to explain last month's session, short enough that the
            // file stays in the tens of megabytes.
            max_age_ms: Some(30 * 24 * 60 * 60 * 1_000),
            max_events: Some(50_000),
            keep_per_session: 50,
        }
    }
}

impl Retention {
    /// Keeps everything. For tests and for a user debugging an adapter.
    pub fn unlimited() -> Self {
        Self {
            max_age_ms: None,
            max_events: None,
            keep_per_session: 0,
        }
    }

    pub fn with_max_age_ms(mut self, max_age_ms: i64) -> Self {
        self.max_age_ms = Some(max_age_ms);
        self
    }

    pub fn with_max_events(mut self, max_events: usize) -> Self {
        self.max_events = Some(max_events);
        self
    }

    pub fn keeping_per_session(mut self, keep: usize) -> Self {
        self.keep_per_session = keep;
        self
    }
}

/// What a prune removed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PruneOutcome {
    pub removed_by_age: usize,
    pub removed_by_count: usize,
}

impl PruneOutcome {
    pub fn total(&self) -> usize {
        self.removed_by_age + self.removed_by_count
    }
}

pub struct EventRepo<'a> {
    conn: &'a Connection,
}

impl<'a> EventRepo<'a> {
    pub(crate) fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Appends one event.
    pub fn append(&self, event: &TurnEvent) -> Result<()> {
        insert(self.conn, event)
    }

    /// Appends a batch in one transaction.
    ///
    /// A burst of hook callbacks arrives together; committing once instead of per
    /// event is the difference between one fsync and forty.
    pub fn append_all(&self, events: &[TurnEvent]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        for event in events {
            insert(&tx, event)?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn get(&self, id: &EventId) -> Result<Option<TurnEvent>> {
        let sql = format!("SELECT {COLUMNS} FROM events WHERE id = ?1");
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query(params![id.as_str()])?;
        match rows.next()? {
            Some(row) => Ok(Some(from_row(row)?)),
            None => Ok(None),
        }
    }

    /// A session's most recent events, newest first.
    pub fn list_for_session(&self, session: &SessionId, limit: usize) -> Result<Vec<TurnEvent>> {
        self.query(
            &format!(
                "SELECT {COLUMNS} FROM events WHERE session_id = ?1 \
                 ORDER BY timestamp_ms DESC, id DESC LIMIT ?2"
            ),
            params![session.as_str(), limit as i64],
        )
    }

    /// A session's events from a point in time onwards, oldest first.
    ///
    /// This is the shape the event panel needs when catching up after a
    /// reconnect: it reads forwards from where it left off.
    pub fn since(&self, session: &SessionId, from_ms: i64) -> Result<Vec<TurnEvent>> {
        self.query(
            &format!(
                "SELECT {COLUMNS} FROM events WHERE session_id = ?1 AND timestamp_ms >= ?2 \
                 ORDER BY timestamp_ms ASC, id ASC"
            ),
            params![session.as_str(), from_ms],
        )
    }

    /// The most recent events across every session, newest first.
    pub fn list_recent(&self, limit: usize) -> Result<Vec<TurnEvent>> {
        self.query(
            &format!("SELECT {COLUMNS} FROM events ORDER BY timestamp_ms DESC, id DESC LIMIT ?1"),
            params![limit as i64],
        )
    }

    /// Events of one kind for a session, newest first. Uses the slug index.
    pub fn list_of_kind(
        &self,
        session: &SessionId,
        kind_slug: &str,
        limit: usize,
    ) -> Result<Vec<TurnEvent>> {
        self.query(
            &format!(
                "SELECT {COLUMNS} FROM events WHERE session_id = ?1 AND kind_slug = ?2 \
                 ORDER BY timestamp_ms DESC, id DESC LIMIT ?3"
            ),
            params![session.as_str(), kind_slug, limit as i64],
        )
    }

    pub fn count(&self) -> Result<usize> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))?;
        Ok(count as usize)
    }

    pub fn count_for_session(&self, session: &SessionId) -> Result<usize> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM events WHERE session_id = ?1",
            params![session.as_str()],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }

    /// Applies a retention policy.
    ///
    /// Age first, then count, both protected by the per-session floor. The floor
    /// wins: a store holding many sessions can legitimately end up above
    /// `max_events` rather than erase a session's entire history.
    pub fn prune(&self, retention: &Retention, now_ms: i64) -> Result<PruneOutcome> {
        let floor = retention.keep_per_session as i64;
        let tx = self.conn.unchecked_transaction()?;
        let mut outcome = PruneOutcome::default();

        if let Some(max_age_ms) = retention.max_age_ms {
            let cutoff = now_ms.saturating_sub(max_age_ms);
            outcome.removed_by_age = tx.execute(
                "DELETE FROM events WHERE id IN ( \
                     SELECT id FROM ( \
                         SELECT id, timestamp_ms, ROW_NUMBER() OVER ( \
                             PARTITION BY session_id ORDER BY timestamp_ms DESC, id DESC \
                         ) AS in_session FROM events \
                     ) WHERE timestamp_ms < ?1 AND in_session > ?2 )",
                params![cutoff, floor],
            )?;
        }

        if let Some(max_events) = retention.max_events {
            outcome.removed_by_count = tx.execute(
                "DELETE FROM events WHERE id IN ( \
                     SELECT id FROM ( \
                         SELECT id, \
                             ROW_NUMBER() OVER (ORDER BY timestamp_ms DESC, id DESC) AS overall, \
                             ROW_NUMBER() OVER ( \
                                 PARTITION BY session_id ORDER BY timestamp_ms DESC, id DESC \
                             ) AS in_session \
                         FROM events \
                     ) WHERE overall > ?1 AND in_session > ?2 )",
                params![max_events as i64, floor],
            )?;
        }

        tx.commit()?;
        Ok(outcome)
    }

    /// Drops a session's events without touching the session itself.
    pub fn delete_for_session(&self, session: &SessionId) -> Result<usize> {
        let removed = self.conn.execute(
            "DELETE FROM events WHERE session_id = ?1",
            params![session.as_str()],
        )?;
        Ok(removed)
    }

    fn query(&self, sql: &str, args: impl rusqlite::Params) -> Result<Vec<TurnEvent>> {
        let mut stmt = self.conn.prepare(sql)?;
        let mut rows = stmt.query(args)?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(from_row(row)?);
        }
        Ok(out)
    }
}

pub(crate) fn insert(conn: &Connection, event: &TurnEvent) -> Result<()> {
    conn.execute(
        "INSERT INTO events (id, timestamp_ms, workspace_id, session_id, node_id, \
             parent_node_id, kind_slug, kind_json, agent_json, confidence, source_json, \
             severity, dedup_key, raw) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14) \
         ON CONFLICT(id) DO NOTHING",
        params![
            event.id.as_str(),
            event.timestamp_ms,
            event.workspace_id.as_ref().map(|w| w.as_str()),
            event.session_id.as_str(),
            event.node_id.as_ref().map(|n| n.as_str()),
            event.parent_node_id.as_ref().map(|n| n.as_str()),
            event_name(&event.kind),
            // Redacted like the raw payload, and for the same reason. The kind is
            // where a permission request keeps the command it is asking about and a
            // turn start keeps what the user typed: both routinely carry a token
            // under a key called `command` or `prompt_excerpt`, which no key-name
            // rule would ever notice.
            redact_json(&json("event kind", &event.kind)?),
            redact_json(&json("event agent", &event.agent)?),
            tag("confidence", &event.confidence)?,
            redact_json(&json("event source", &event.source)?),
            tag("severity", &event.severity)?,
            redact_secrets(&event.dedup_key),
            raw_for_persistence(event),
        ],
    )
    .map_err(|error| StoreError::from_write("event", event.session_id.as_str(), error))?;
    Ok(())
}

/// Returns the diagnostic note that may cross the durable boundary.
///
/// Hook payloads never do. This repository-level check is intentional defence in
/// depth: adapters should avoid attaching raw callbacks in the first place, but
/// persistence must remain safe if a future adapter or test constructs a
/// [`TurnEvent`] incorrectly. Non-hook notes (for example an OS exit description)
/// retain their existing redacted persistence semantics.
fn raw_for_persistence(event: &TurnEvent) -> Option<String> {
    match &event.source {
        EventSource::Hook { .. } => None,
        _ => event.raw.as_deref().map(redact_json),
    }
}

fn from_row(row: &Row<'_>) -> Result<TurnEvent> {
    let id: String = row.get("id")?;
    Ok(TurnEvent {
        id: EventId::from_stored(id.clone()),
        timestamp_ms: row.get("timestamp_ms")?,
        workspace_id: row
            .get::<_, Option<String>>("workspace_id")?
            .map(WorkspaceId::from_stored),
        session_id: SessionId::from_stored(row.get::<_, String>("session_id")?),
        node_id: row
            .get::<_, Option<String>>("node_id")?
            .map(NodeId::from_stored),
        parent_node_id: row
            .get::<_, Option<String>>("parent_node_id")?
            .map(NodeId::from_stored),
        agent: from_json::<AgentRef>("event agent", &id, &row.get::<_, String>("agent_json")?)?,
        kind: from_json::<EventKind>("event kind", &id, &row.get::<_, String>("kind_json")?)?,
        confidence: from_tag::<Confidence>(
            "confidence",
            &id,
            &row.get::<_, String>("confidence")?,
        )?,
        source: from_json::<EventSource>(
            "event source",
            &id,
            &row.get::<_, String>("source_json")?,
        )?,
        severity: from_tag::<Severity>("severity", &id, &row.get::<_, String>("severity")?)?,
        dedup_key: row.get("dedup_key")?,
        raw: row.get("raw")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::redact::REDACTED;
    use crate::testing;
    use turn_core::event::Risk;
    use turn_core::state::AwaitingReason;

    const T0: i64 = 1_700_000_000_000;
    const DAY: i64 = 24 * 60 * 60 * 1_000;

    fn hook_event(session: &SessionId, at_ms: i64) -> TurnEvent {
        TurnEvent::new(
            session.clone(),
            EventKind::AgentPermissionRequired {
                summary: "run make verify".into(),
                command: Some("make verify".into()),
                tool_name: Some("Bash".into()),
                risk: Risk::Medium,
            },
            EventSource::Hook {
                tool: "claude-code".into(),
                event_name: "PermissionRequest".into(),
            },
            Confidence::Explicit,
            at_ms,
        )
    }

    fn guessed_event(session: &SessionId, at_ms: i64) -> TurnEvent {
        TurnEvent::new(
            session.clone(),
            EventKind::AgentWaitingForUser {
                reason: AwaitingReason::Input,
                summary: None,
            },
            EventSource::PtyHeuristic {
                rule: "idle_prompt".into(),
            },
            Confidence::Explicit,
            at_ms,
        )
    }

    #[test]
    fn an_event_round_trips_with_its_payload_agent_and_provenance() {
        let store = testing::store();
        let session = testing::saved_session_anywhere(&store, "events");
        let event = hook_event(&session.id, T0)
            .with_node(NodeId::from_stored("proc_agent"))
            .with_parent(NodeId::from_stored("proc_root"))
            .with_workspace(session.workspace_id.clone())
            .with_agent(AgentRef {
                provider: Some("anthropic".into()),
                tool: Some("claude-code".into()),
                model: Some("opus".into()),
                external_id: None,
            });

        store.events().append(&event).unwrap();
        let back = store.events().get(&event.id).unwrap().expect("stored");

        assert_eq!(back, event);
        assert_eq!(back.attention_reason(), Some(AwaitingReason::Permission));
        assert_eq!(back.severity, Severity::Warning);
    }

    /// The product rule that must outlive a restart: a guess stays a guess.
    #[test]
    fn a_heuristic_event_is_still_a_guess_after_it_comes_back_from_the_database() {
        let store = testing::store();
        let session = testing::saved_session_anywhere(&store, "events");
        let event = guessed_event(&session.id, T0);
        assert_eq!(event.confidence, Confidence::InferredHigh);

        store.events().append(&event).unwrap();
        let back = store.events().get(&event.id).unwrap().unwrap();

        assert_eq!(back.confidence, Confidence::InferredHigh);
        assert!(back.confidence.is_provisional());
        assert!(
            !back.confidence.may_steal_focus(),
            "a stored guess must not gain authority by having been written down"
        );
        assert_eq!(
            back.source,
            EventSource::PtyHeuristic {
                rule: "idle_prompt".into()
            },
            "and Turn can still name the rule that guessed"
        );
    }

    #[test]
    fn every_event_kind_survives_storage_with_its_fields() {
        let store = testing::store();
        let session = testing::saved_session_anywhere(&store, "kinds");
        let kinds = [
            EventKind::ProcessStarted {
                pid: 4242,
                command: "claude".into(),
            },
            EventKind::ProcessExited { code: 0 },
            EventKind::ProcessFailed {
                code: None,
                signal: Some(9),
            },
            EventKind::ProcessSpawnedChild {
                child: NodeId::from_stored("proc_child"),
                pid: 4243,
                ppid: Some(4242),
                command: "cargo test".into(),
                cwd: Some("/repo".into()),
                confirmed_parent: false,
            },
            EventKind::AgentTurnCompleted {
                last_message: Some("done".into()),
                background_tasks: 3,
            },
            EventKind::AgentSpawned {
                declared_name: None,
                agent_type: Some("Explore".into()),
                agent_id: Some("sub-1".into()),
                task: None,
            },
            EventKind::AgentTaskCompleted {
                summary: Some("all green".into()),
            },
            EventKind::SessionAttentionResolved,
        ];

        let events: Vec<TurnEvent> = kinds
            .iter()
            .enumerate()
            .map(|(i, kind)| {
                TurnEvent::new(
                    session.id.clone(),
                    kind.clone(),
                    EventSource::Supervisor,
                    Confidence::Explicit,
                    T0 + i as i64,
                )
            })
            .collect();
        store.events().append_all(&events).unwrap();

        let back = store.events().since(&session.id, T0).unwrap();
        assert_eq!(back.len(), kinds.len());
        for (stored, original) in back.iter().zip(events.iter()) {
            assert_eq!(stored, original);
        }
        // The background-task count is what tells the brief's Case E apart from a
        // finished job, so it must be a number and not a boolean.
        let turn_completed = back
            .iter()
            .find(|e| matches!(e.kind, EventKind::AgentTurnCompleted { .. }))
            .unwrap();
        assert!(matches!(
            turn_completed.kind,
            EventKind::AgentTurnCompleted {
                background_tasks: 3,
                ..
            }
        ));
    }

    #[test]
    fn a_raw_hook_payload_is_never_persisted_even_when_free_text_is_not_redactable() {
        let store = testing::store();
        let session = testing::saved_session_anywhere(&store, "raw");
        let secret = "free-text-secret-with-no-recognisable-shape-8675309";
        let event = hook_event(&session.id, T0).with_raw(format!(
            r#"{{"cwd":"/repo","diagnostic_note":"{secret}","prompt":"fix the tests"}}"#
        ));
        store.events().append(&event).unwrap();

        let stored = store.events().get(&event.id).unwrap().unwrap();
        assert_eq!(stored.raw, None);
        let raw_column: Option<String> = store
            .connection()
            .query_row(
                "SELECT raw FROM events WHERE id = ?1",
                [event.id.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(raw_column, None);
        assert!(
            !format!("{stored:?}").contains(secret),
            "the ignored free text survived elsewhere in the event"
        );
    }

    #[test]
    fn a_non_hook_diagnostic_note_is_still_redacted_and_persisted() {
        let store = testing::store();
        let session = testing::saved_session_anywhere(&store, "diagnostic");
        let event = TurnEvent::new(
            session.id.clone(),
            EventKind::AgentIdle,
            EventSource::Supervisor,
            Confidence::Explicit,
            T0,
        )
        .with_raw(r#"{"GITHUB_TOKEN":"ghp_leaked","note":"process disappeared"}"#);
        store.events().append(&event).unwrap();

        let raw = store.events().get(&event.id).unwrap().unwrap().raw.unwrap();
        assert!(!raw.contains("ghp_leaked"), "got {raw}");
        assert!(raw.contains(REDACTED), "got {raw}");
        assert!(raw.contains("process disappeared"), "got {raw}");
    }

    /// The raw payload was not the only copy. A permission request stores the
    /// command it is asking about in the event *kind*, and that command is written
    /// by a model: `curl -H "Authorization: Bearer …"` is exactly the shape of
    /// request a user is asked to approve.
    #[test]
    fn a_secret_inside_the_event_kind_is_redacted_before_it_is_stored() {
        use turn_core::event::Risk;

        let store = testing::store();
        let session = testing::saved_session_anywhere(&store, "kind");
        let secret = "ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
        let event = TurnEvent::new(
            session.id.clone(),
            EventKind::AgentPermissionRequired {
                summary: format!("Run `gh auth login --with-token {secret}`"),
                command: Some(format!("gh auth login --with-token {secret}")),
                tool_name: Some("Bash".into()),
                risk: Risk::Medium,
            },
            EventSource::Hook {
                tool: "claude-code".into(),
                event_name: "PermissionRequest".into(),
            },
            Confidence::Explicit,
            T0,
        );
        store.events().append(&event).unwrap();

        let stored = store.events().get(&event.id).unwrap().unwrap();
        match &stored.kind {
            EventKind::AgentPermissionRequired {
                summary, command, ..
            } => {
                assert!(!summary.contains(secret), "got {summary}");
                assert!(
                    !command.as_deref().unwrap().contains(secret),
                    "got {command:?}"
                );
                assert!(
                    command.as_deref().unwrap().contains("gh auth login"),
                    "what the user needs to understand the request survives: {command:?}"
                );
            }
            other => panic!("unexpected {other:?}"),
        }

        // And the file itself, not only what the read path hands back.
        let raw_column: String = store
            .connection()
            .query_row(
                "SELECT kind_json FROM events WHERE id = ?1",
                [event.id.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!raw_column.contains(secret), "got {raw_column}");
    }

    #[test]
    fn events_are_listed_newest_first_and_read_forwards_from_a_timestamp() {
        let store = testing::store();
        let session = testing::saved_session_anywhere(&store, "order");
        let events: Vec<TurnEvent> = (0..5)
            .map(|i| hook_event(&session.id, T0 + i * 1_000))
            .collect();
        store.events().append_all(&events).unwrap();

        let newest = store.events().list_for_session(&session.id, 3).unwrap();
        assert_eq!(newest.len(), 3);
        assert_eq!(newest[0].timestamp_ms, T0 + 4_000);
        assert_eq!(newest[2].timestamp_ms, T0 + 2_000);

        let forwards = store.events().since(&session.id, T0 + 3_000).unwrap();
        assert_eq!(forwards.len(), 2);
        assert_eq!(forwards[0].timestamp_ms, T0 + 3_000);
    }

    #[test]
    fn events_can_be_filtered_by_kind_without_scanning_payloads() {
        let store = testing::store();
        let session = testing::saved_session_anywhere(&store, "kinds");
        store
            .events()
            .append_all(&[
                hook_event(&session.id, T0),
                guessed_event(&session.id, T0 + 1),
                hook_event(&session.id, T0 + 2),
            ])
            .unwrap();

        let permissions = store
            .events()
            .list_of_kind(&session.id, "agent.permission_required", 10)
            .unwrap();
        assert_eq!(permissions.len(), 2);
        assert!(permissions
            .iter()
            .all(|e| matches!(e.kind, EventKind::AgentPermissionRequired { .. })));
    }

    #[test]
    fn one_sessions_events_are_never_returned_for_another() {
        let store = testing::store();
        let mine = testing::saved_session_anywhere(&store, "mine");
        let yours = testing::saved_session_anywhere(&store, "yours");
        store.events().append(&hook_event(&mine.id, T0)).unwrap();
        store.events().append(&hook_event(&yours.id, T0)).unwrap();

        assert_eq!(store.events().count_for_session(&mine.id).unwrap(), 1);
        assert_eq!(
            store.events().list_for_session(&mine.id, 10).unwrap().len(),
            1
        );
        assert_eq!(store.events().count().unwrap(), 2);
    }

    #[test]
    fn appending_the_same_event_twice_stores_it_once() {
        let store = testing::store();
        let session = testing::saved_session_anywhere(&store, "retry");
        let event = hook_event(&session.id, T0);
        store.events().append(&event).unwrap();
        store
            .events()
            .append(&event)
            .expect("a retried delivery must not be an error");
        assert_eq!(store.events().count().unwrap(), 1);
    }

    #[test]
    fn an_event_for_an_unknown_session_is_refused_rather_than_orphaned() {
        let store = testing::store();
        let error = store
            .events()
            .append(&hook_event(&SessionId::from_stored("sess_ghost"), T0))
            .expect_err("an unattributable event must not be stored");
        match error {
            StoreError::UnknownReference { what, missing } => {
                assert_eq!(what, "event");
                assert_eq!(missing, "sess_ghost");
            }
            other => panic!("expected UnknownReference, got {other:?}"),
        }
    }

    #[test]
    fn pruning_by_age_keeps_the_newest_and_drops_only_what_is_past_the_cutoff() {
        let store = testing::store();
        let session = testing::saved_session_anywhere(&store, "old");
        let events: Vec<TurnEvent> = (0..10)
            .map(|i| hook_event(&session.id, T0 + i * DAY))
            .collect();
        store.events().append_all(&events).unwrap();

        // "Now" is a day after the last event; keep three days, no floor.
        let now = T0 + 10 * DAY;
        let retention = Retention::unlimited().with_max_age_ms(3 * DAY);
        let outcome = store.events().prune(&retention, now).unwrap();

        assert_eq!(outcome.removed_by_age, 7);
        assert_eq!(outcome.removed_by_count, 0);
        let left = store.events().list_for_session(&session.id, 100).unwrap();
        assert_eq!(left.len(), 3);
        assert_eq!(left[0].timestamp_ms, T0 + 9 * DAY, "the newest is kept");
        assert!(
            left.iter().all(|e| e.timestamp_ms >= now - 3 * DAY),
            "everything left is inside the window"
        );
    }

    #[test]
    fn the_per_session_floor_beats_the_age_limit() {
        let store = testing::store();
        let session = testing::saved_session_anywhere(&store, "ancient");
        let events: Vec<TurnEvent> = (0..10)
            .map(|i| hook_event(&session.id, T0 + i * 1_000))
            .collect();
        store.events().append_all(&events).unwrap();

        // Every event is a year old; the floor still protects the newest five.
        let retention = Retention::unlimited()
            .with_max_age_ms(DAY)
            .keeping_per_session(5);
        let outcome = store.events().prune(&retention, T0 + 365 * DAY).unwrap();

        assert_eq!(outcome.removed_by_age, 5);
        let left = store.events().list_for_session(&session.id, 100).unwrap();
        assert_eq!(left.len(), 5, "the floor holds even for ancient history");
        assert_eq!(left[0].timestamp_ms, T0 + 9_000);
        assert_eq!(
            left[4].timestamp_ms,
            T0 + 5_000,
            "and it keeps the newest five"
        );
    }

    #[test]
    fn pruning_by_count_keeps_the_newest_across_sessions() {
        let store = testing::store();
        let first = testing::saved_session_anywhere(&store, "first");
        let second = testing::saved_session_anywhere(&store, "second");
        for i in 0..10 {
            store
                .events()
                .append(&hook_event(&first.id, T0 + i))
                .unwrap();
        }
        for i in 0..10 {
            store
                .events()
                .append(&hook_event(&second.id, T0 + 100 + i))
                .unwrap();
        }

        let retention = Retention::unlimited().with_max_events(6);
        let outcome = store.events().prune(&retention, T0 + 1_000).unwrap();

        assert_eq!(outcome.removed_by_count, 14);
        assert_eq!(store.events().count().unwrap(), 6);
        let left = store.events().list_recent(100).unwrap();
        assert!(
            left.iter().all(|e| e.session_id == second.id),
            "the newest six all belong to the session that was busy last"
        );
        assert_eq!(left[0].timestamp_ms, T0 + 109);
    }

    #[test]
    fn the_floor_can_legitimately_leave_more_events_than_the_count_limit() {
        let store = testing::store();
        let a = testing::saved_session_anywhere(&store, "a");
        let b = testing::saved_session_anywhere(&store, "b");
        for i in 0..10 {
            store.events().append(&hook_event(&a.id, T0 + i)).unwrap();
            store
                .events()
                .append(&hook_event(&b.id, T0 + 100 + i))
                .unwrap();
        }

        let retention = Retention::unlimited()
            .with_max_events(2)
            .keeping_per_session(3);
        store.events().prune(&retention, T0 + 1_000).unwrap();

        assert_eq!(
            store.events().count().unwrap(),
            6,
            "two sessions keep three each: no session is erased to satisfy a total"
        );
        assert_eq!(store.events().count_for_session(&a.id).unwrap(), 3);
        assert_eq!(store.events().count_for_session(&b.id).unwrap(), 3);
    }

    #[test]
    fn pruning_an_empty_or_already_small_log_removes_nothing() {
        let store = testing::store();
        let session = testing::saved_session_anywhere(&store, "quiet");
        let empty = store.events().prune(&Retention::default(), T0).unwrap();
        assert_eq!(empty.total(), 0);

        store.events().append(&hook_event(&session.id, T0)).unwrap();
        let outcome = store
            .events()
            .prune(&Retention::default(), T0 + 1_000)
            .unwrap();
        assert_eq!(outcome.total(), 0);
        assert_eq!(store.events().count().unwrap(), 1);
    }

    #[test]
    fn an_unlimited_retention_never_removes_anything() {
        let store = testing::store();
        let session = testing::saved_session_anywhere(&store, "hoarder");
        let events: Vec<TurnEvent> = (0..50)
            .map(|i| hook_event(&session.id, T0 - i * DAY))
            .collect();
        store.events().append_all(&events).unwrap();

        let outcome = store
            .events()
            .prune(&Retention::unlimited(), T0 + 1_000 * DAY)
            .unwrap();
        assert_eq!(outcome.total(), 0);
        assert_eq!(store.events().count().unwrap(), 50);
    }

    #[test]
    fn deleting_a_session_takes_its_events_with_it() {
        let store = testing::store();
        let session = testing::saved_session_anywhere(&store, "doomed");
        store.events().append(&hook_event(&session.id, T0)).unwrap();

        store.sessions().delete(&session.id).unwrap();
        assert_eq!(store.events().count().unwrap(), 0);
    }
}
