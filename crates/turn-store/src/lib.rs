//! # turn-store
//!
//! SQLite persistence for Turn's daemon. Its job is that closing the app and
//! opening it again puts the user back at the desk they left, and that anything
//! which could not be restored is reported rather than guessed at.
//!
//! ## What is persistent and what is not
//!
//! The boundary is the single most important thing about this crate, because
//! getting it wrong produces a convincing lie.
//!
//! **Persisted here** — the things that describe the user's work:
//! workspaces, sessions, the layout tree, templates, attention policies, the
//! attention queue, the event log, and *process metadata*: pid, command, args,
//! cwd, lifecycle, parent and relation, exit code, and the agent's own external
//! session id.
//!
//! The event log contains typed facts and provenance, not callback bodies.
//! [`EventRepo`] refuses to persist `TurnEvent::raw` for every hook source even
//! if an adapter accidentally supplies it; arbitrary hook free text cannot be
//! made safe by credential-pattern redaction.
//!
//! **Never persisted here** — the things that only mean something while a process
//! is alive: the pty master, the terminal grid and its scrollback, the output
//! broadcast channel, the vt100 parser state, live subscriptions. Those belong to
//! `turn-pty` and die with the process. A restored scrollback would show a
//! conversation the agent itself no longer remembers, and a restored pty handle
//! would be a handle to nothing.
//!
//! Process metadata is stored precisely so a fresh daemon can *try* to re-attach
//! and can say what it failed to re-attach. [`SessionRepo::load_for_restore`]
//! downgrades anything stored as running to
//! [`turn_core::state::Lifecycle::Orphaned`]: a stored `Alive` only ever meant
//! "alive when we last wrote". Turn never relaunches anything on restore — it
//! offers, and the user decides.
//!
//! ## Shape
//!
//! * [`migrations`] — the schema, versioned in SQLite's own `user_version`.
//! * [`repo`] — one small repository per entity, round-tripping `turn-core` types.
//! * [`redact`] — the secret hygiene every write goes through.
//! * [`location`] — where the file lives on each platform.
//!
//! Everything is synchronous: the daemon calls it from a blocking context and
//! owns the ordering. There is no runtime, no background thread and no lock in
//! here.
//!
//! ```no_run
//! use turn_store::Store;
//!
//! let store = Store::open_default()?;
//! for workspace in store.workspaces().list_active()? {
//!     for session in store.sessions().list_for_workspace(&workspace.id)? {
//!         println!("{} — {}", session.name, session.display_state());
//!     }
//! }
//! # Ok::<(), turn_store::StoreError>(())
//! ```

mod codec;
pub mod error;
pub mod location;
mod maintenance;
pub mod migrations;
pub mod redact;
pub mod repo;

pub use error::{Result, StoreError};
pub use location::{DATABASE_FILE, DATA_DIR_ENV};
pub use migrations::{Applied, LATEST_VERSION};
pub use redact::{is_sensitive_key, REDACTED};
pub use repo::{
    AttentionRepo, EventRepo, HierarchyRepo, NodeRepo, PruneOutcome, Retention, SessionRepo,
    SettingsRepo, TemplateRepo, WorkspaceRepo,
};

use rusqlite::Connection;
use std::path::{Path, PathBuf};
use turn_core::attention::AttentionQueue;
use turn_core::model::Session;
use turn_core::TurnEvent;

/// How long a write waits for another connection to finish before giving up.
///
/// The daemon is the only writer, but a second Turn process (a CLI invocation, a
/// user's `sqlite3`) can hold the lock briefly. Failing instantly there would turn
/// a moment's contention into a lost session.
const BUSY_TIMEOUT_MS: u64 = 5_000;

/// An open database, migrated and ready.
#[derive(Debug)]
pub struct Store {
    conn: Connection,
    path: Option<PathBuf>,
}

impl Store {
    /// Opens the store in the platform data directory, honouring `TURN_DATA_DIR`.
    pub fn open_default() -> Result<Self> {
        let dir = location::default_data_dir()?;
        Self::open_in(dir)
    }

    /// Opens the store in a specific directory, creating it if needed.
    pub fn open_in(dir: impl AsRef<Path>) -> Result<Self> {
        let dir = dir.as_ref();
        location::ensure_dir(dir)?;
        Self::open_at(location::database_path(dir))
    }

    /// Opens a specific database file, creating it and its parents if needed.
    pub fn open_at(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                location::ensure_dir(parent)?;
            }
        }
        // Legacy files may already contain the very credentials preparation is
        // about to scrub. Narrow the database and existing sidecars before even
        // opening SQLite, so a failed migration/busy WAL cannot return while
        // leaving those bytes world-readable.
        for file in Self::files_for_path(&path) {
            if file.exists() {
                location::restrict_to_owner(&file, 0o600)?;
            }
        }
        let conn = Connection::open(&path)?;
        let store = Self {
            conn,
            path: Some(path.clone()),
        };
        // `Connection::open` may just have created the main file.
        store.restrict_files()?;
        store.prepare()?;
        // After `prepare`, because enabling WAL is what creates the sidecar files.
        // All three carry the same data and so deserve the same permissions: the
        // write-ahead log holds recent rows that have not been checkpointed yet, so
        // leaving it at 0644 would defeat narrowing the database itself.
        store.restrict_files()?;
        Ok(store)
    }

    /// The database and its SQLite sidecars, whether or not they exist yet.
    fn files(&self) -> Vec<PathBuf> {
        let Some(path) = &self.path else {
            return Vec::new();
        };
        Self::files_for_path(path)
    }

    fn files_for_path(path: &Path) -> Vec<PathBuf> {
        let mut out = vec![path.to_path_buf()];
        for suffix in ["-wal", "-shm", "-journal"] {
            let mut sidecar = path.as_os_str().to_os_string();
            sidecar.push(suffix);
            out.push(PathBuf::from(sidecar));
        }
        out
    }

    fn restrict_files(&self) -> Result<()> {
        for file in self.files() {
            if file.exists() {
                location::restrict_to_owner(&file, 0o600)?;
            }
        }
        Ok(())
    }

    /// An anonymous in-memory database. For tests, and for a `--no-persist` run.
    ///
    /// Everything else behaves identically, except that SQLite has no write-ahead
    /// log for memory databases — see [`Self::journal_mode`].
    pub fn open_in_memory() -> Result<Self> {
        let store = Self {
            conn: Connection::open_in_memory()?,
            path: None,
        };
        store.prepare()?;
        Ok(store)
    }

    /// Sets the pragmas, then migrates.
    ///
    /// Pragma order matters: `foreign_keys` cannot be changed inside a
    /// transaction, so it is set before any migration runs, and the constraints
    /// are therefore live for every write from the first one onwards.
    fn prepare(&self) -> Result<()> {
        self.conn
            .busy_timeout(std::time::Duration::from_millis(BUSY_TIMEOUT_MS))?;
        self.conn.execute_batch(
            "PRAGMA foreign_keys = ON;\
             PRAGMA synchronous = NORMAL;\
             PRAGMA temp_store = MEMORY;",
        )?;
        if self.path.is_some() {
            // Returns the resulting mode as a row, so it cannot go through
            // execute_batch. A refusal is not fatal — the store still works with
            // a rollback journal, just with less concurrency — but it is worth
            // knowing about.
            let mode: String = self
                .conn
                .query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
            if !mode.eq_ignore_ascii_case("wal") {
                tracing::warn!(
                    mode = %mode,
                    "the database rejected write-ahead logging; falling back"
                );
            }
            // WAL/SHM may have been created by the mode switch. Protect them
            // before migrations or security maintenance can fail.
            self.restrict_files()?;
        }
        let applied = migrations::apply(&self.conn)?;
        // SQL migrations can mark a physical purge as required, but VACUUM may
        // not run inside their transaction. The marker remains until this
        // retryable pass has redacted every historical free-text column,
        // rebuilt the database and truncated the WAL.
        maintenance::run(&self.conn, self.path.is_some())?;
        // SQL cannot resolve symlinks or prove a checkout still exists. Perform
        // that part of the v6 safety migration against the real filesystem. The
        // pass is idempotent and deliberately never clears a reconciliation flag.
        repo::workspace::canonicalize_persisted_roots(&self.conn)?;
        if applied.changed() {
            tracing::info!(
                from = applied.from,
                to = applied.to,
                migrations = ?applied.names,
                "migrated the store"
            );
        }
        Ok(())
    }

    /// The file backing this store, or `None` for an in-memory one.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// The schema version currently on disk.
    pub fn schema_version(&self) -> Result<i64> {
        migrations::schema_version(&self.conn)
    }

    /// The journal mode in force. `"wal"` for a file, `"memory"` for an in-memory
    /// database, which is why it is reported rather than asserted.
    pub fn journal_mode(&self) -> Result<String> {
        Ok(self
            .conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))?)
    }

    /// Whether foreign keys are being enforced.
    pub fn foreign_keys_enforced(&self) -> Result<bool> {
        Ok(self
            .conn
            .query_row("PRAGMA foreign_keys", [], |row| row.get::<_, i64>(0))?
            == 1)
    }

    /// Compacts the file and truncates the write-ahead log.
    ///
    /// Deleting a workspace can free a lot of pages; SQLite keeps them for reuse
    /// unless asked. Called on demand, never on a timer from in here.
    pub fn compact(&self) -> Result<()> {
        self.conn
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE); VACUUM;")?;
        Ok(())
    }

    pub fn workspaces(&self) -> WorkspaceRepo<'_> {
        WorkspaceRepo::new(&self.conn)
    }

    pub fn sessions(&self) -> SessionRepo<'_> {
        SessionRepo::new(&self.conn)
    }

    pub fn nodes(&self) -> NodeRepo<'_> {
        NodeRepo::new(&self.conn)
    }

    pub fn hierarchy(&self) -> HierarchyRepo<'_> {
        HierarchyRepo::new(&self.conn)
    }

    pub fn templates(&self) -> TemplateRepo<'_> {
        TemplateRepo::new(&self.conn)
    }

    pub fn events(&self) -> EventRepo<'_> {
        EventRepo::new(&self.conn)
    }

    pub fn attention(&self) -> AttentionRepo<'_> {
        AttentionRepo::new(&self.conn)
    }

    pub fn settings(&self) -> SettingsRepo<'_> {
        SettingsRepo::new(&self.conn)
    }

    /// The user's layered preferences. Separate from [`Self::settings`], which holds Turn's
    /// own singletons: a preference's identity includes the level it was set at.
    pub fn setting_layers(&self) -> crate::repo::setting_layer::SettingLayerRepo<'_> {
        crate::repo::setting_layer::SettingLayerRepo::new(&self.conn)
    }

    /// Atomically records everything one accepted runtime event changed.
    ///
    /// Ordering is part of the contract: an out-of-order stop can materialise a
    /// node tombstone, so the complete Session (including its nodes, layout and
    /// activity preview) must exist before the event is inserted; the queue then
    /// records the attention projection produced by that same event. A failure in
    /// any step rolls all three projections back to their previous state.
    pub fn checkpoint_event_session_attention(
        &self,
        session: &Session,
        event: &TurnEvent,
        attention: &AttentionQueue,
    ) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        repo::session::save_in(&tx, session)?;
        repo::event::insert(&tx, event)?;
        repo::attention::replace_all_in(&tx, attention)?;
        tx.commit()?;
        Ok(())
    }

    /// The underlying connection, for tests that need to simulate a database
    /// damaged behind Turn's back.
    #[cfg(test)]
    pub(crate) fn connection(&self) -> &Connection {
        &self.conn
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use turn_core::ids::{SessionId, WorkspaceId};
    use turn_core::model::layout::{Pane, PaneKind};
    use turn_core::model::{Layout, Session, Workspace};
    use turn_core::{Confidence, EventKind, EventSource, TurnEvent};

    pub(crate) const T0: i64 = 1_700_000_000_000;

    pub(crate) fn store() -> Store {
        Store::open_in_memory().expect("an in-memory store always opens")
    }

    /// Every table that still mentions `value` in a column called `column`, with the count.
    ///
    /// Asks the schema which tables have the column rather than being given a list, so a table
    /// added later is covered the day it is added. This is what makes "Turn forgets it" a
    /// testable claim instead of a sentence in a dialog: a delete that leaves a row behind
    /// names the table that kept it.
    pub(crate) fn rows_mentioning(store: &Store, column: &str, value: &str) -> Vec<(String, i64)> {
        let conn = store.connection();
        let tables: Vec<String> = {
            let mut stmt = conn
                .prepare(
                    "SELECT m.name FROM sqlite_master m \
                     JOIN pragma_table_info(m.name) c \
                     WHERE m.type = 'table' AND c.name = ?1 \
                     ORDER BY m.name",
                )
                .expect("the schema can always be queried");
            let mut rows = stmt.query([column]).expect("query");
            let mut out = Vec::new();
            while let Some(row) = rows.next().expect("row") {
                out.push(row.get(0).expect("table name"));
            }
            out
        };
        assert!(
            !tables.is_empty(),
            "no table has a column called {column:?}, so this check would pass vacuously"
        );
        let mut found = Vec::new();
        for table in tables {
            // The table name comes from `sqlite_master`, not from a caller.
            let sql = format!("SELECT COUNT(*) FROM \"{table}\" WHERE \"{column}\" = ?1");
            let count: i64 = conn
                .query_row(&sql, [value], |row| row.get(0))
                .unwrap_or_else(|cause| panic!("counting {table}: {cause}"));
            if count > 0 {
                found.push((table, count));
            }
        }
        found
    }

    pub(crate) fn saved_workspace(store: &Store, name: &str) -> Workspace {
        let id = WorkspaceId::new();
        let root = std::env::temp_dir()
            .join("turn-store-tests")
            .join(id.as_str());
        std::fs::create_dir_all(&root).expect("test Workspace root");
        let root = std::fs::canonicalize(root).expect("canonical test Workspace root");
        let mut workspace = Workspace::new(name, root.to_string_lossy(), T0);
        workspace.id = id;
        store.workspaces().save(&workspace).expect("saved");
        workspace
    }

    pub(crate) fn saved_session(store: &Store, workspace: &WorkspaceId, name: &str) -> Session {
        let layout = Layout::single(Pane::new(PaneKind::Agent).with_command("claude"));
        let session = Session::new(workspace.clone(), name, "/repo", layout, T0);
        store.sessions().save(&session).expect("saved");
        session
    }

    /// A session plus the workspace it needs, for tests that only care about the
    /// session's id.
    pub(crate) fn saved_session_anywhere(store: &Store, name: &str) -> Session {
        let workspace = saved_workspace(store, &format!("ws-for-{name}"));
        saved_session(store, &workspace.id, name)
    }

    pub(crate) fn save_event(store: &Store, session: &SessionId, at_ms: i64) -> TurnEvent {
        let event = TurnEvent::new(
            session.clone(),
            EventKind::AgentIdle,
            EventSource::Supervisor,
            Confidence::Explicit,
            at_ms,
        );
        store.events().append(&event).expect("appended");
        event
    }

    pub(crate) fn save_attention(store: &Store, session: &SessionId, at_ms: i64) {
        use turn_core::attention::{AttentionEntry, EntryState};
        use turn_core::ids::AttentionId;
        use turn_core::state::AwaitingReason;

        let entry = AttentionEntry {
            id: AttentionId::new(),
            session_id: session.clone(),
            node_id: None,
            parent_node_id: None,
            subject_external_id: None,
            reason: AwaitingReason::Permission,
            summary: None,
            confidence: Confidence::Explicit,
            created_ms: at_ms,
            updated_ms: at_ms,
            state: EntryState::Pending,
            priority_boost: 0,
            survives_owner_exit: false,
            demand_kind: Default::default(),
        };
        store.attention().upsert(&entry).expect("stored");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use turn_core::attention::{AttentionDemandKind, AttentionEntry, EntryState};
    use turn_core::ids::{AttentionId, NodeId, SessionId};
    use turn_core::model::{NodeKind, ProcessNode, Workspace};
    use turn_core::state::AwaitingReason;
    use turn_core::{Confidence, EventKind, EventSource};

    fn checkpoint_attention(
        session_id: &SessionId,
        node_id: Option<NodeId>,
        at_ms: i64,
    ) -> AttentionEntry {
        AttentionEntry {
            id: AttentionId::new(),
            session_id: session_id.clone(),
            node_id,
            parent_node_id: None,
            subject_external_id: None,
            reason: AwaitingReason::Permission,
            summary: Some("review the requested command".into()),
            confidence: Confidence::Explicit,
            created_ms: at_ms,
            updated_ms: at_ms,
            state: EntryState::Pending,
            priority_boost: 0,
            survives_owner_exit: false,
            demand_kind: AttentionDemandKind::Interaction,
        }
    }

    #[test]
    fn opening_a_file_enables_write_ahead_logging_and_foreign_keys() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open_at(temp.path().join("turn.db")).unwrap();

        assert_eq!(store.journal_mode().unwrap().to_lowercase(), "wal");
        assert!(store.foreign_keys_enforced().unwrap());
        assert_eq!(store.schema_version().unwrap(), LATEST_VERSION);
    }

    #[test]
    fn opening_a_directory_creates_the_file_at_the_expected_name() {
        let temp = tempfile::tempdir().unwrap();
        let nested = temp.path().join("does/not/exist/yet");
        let store = Store::open_in(&nested).unwrap();

        assert_eq!(store.path(), Some(nested.join("turn.db").as_path()));
        assert!(nested.join("turn.db").is_file());
    }

    #[test]
    fn an_in_memory_store_is_fully_migrated_and_has_no_path() {
        let store = Store::open_in_memory().unwrap();
        assert!(store.path().is_none());
        assert_eq!(store.schema_version().unwrap(), LATEST_VERSION);
        assert!(store.foreign_keys_enforced().unwrap());
        assert_eq!(store.workspaces().count().unwrap(), 0);
    }

    #[test]
    fn an_event_checkpoints_its_new_session_node_and_attention_together() {
        let store = testing::store();
        let workspace = testing::saved_workspace(&store, "atomic checkpoint");
        let layout = turn_core::model::Layout::single(
            turn_core::model::Pane::new(turn_core::model::PaneKind::Agent).with_command("claude"),
        );
        let mut session = Session::new(
            workspace.id.clone(),
            "new session",
            "/repo",
            layout,
            testing::T0,
        );
        let node_id = session.tree.insert(ProcessNode::process(
            session.id.clone(),
            NodeKind::Subagent,
            "Reviewer",
            "/repo",
            testing::T0,
        ));
        let event = TurnEvent::new(
            session.id.clone(),
            EventKind::AgentIdle,
            EventSource::Supervisor,
            Confidence::Explicit,
            testing::T0 + 1,
        )
        .with_workspace(workspace.id.clone())
        .with_node(node_id.clone());
        let mut attention = AttentionQueue::new();
        let demand = checkpoint_attention(&session.id, Some(node_id.clone()), testing::T0 + 1);
        attention.upsert(demand.clone());

        store
            .checkpoint_event_session_attention(&session, &event, &attention)
            .unwrap();

        let saved = store.sessions().get(&session.id).unwrap().unwrap();
        assert!(saved.tree.get(&node_id).is_some());
        assert_eq!(store.events().get(&event.id).unwrap(), Some(event));
        assert_eq!(store.attention().load_queue().unwrap(), attention);
        assert_eq!(store.attention().get(&demand.id).unwrap(), Some(demand));
    }

    #[test]
    fn a_failed_attention_write_rolls_back_session_event_and_queue() {
        let store = testing::store();
        let mut session = testing::saved_session_anywhere(&store, "before checkpoint");
        let prior_event = testing::save_event(&store, &session.id, testing::T0);
        testing::save_attention(&store, &session.id, testing::T0);
        let prior_queue = store.attention().load_queue().unwrap();
        let prior_node_count = store.nodes().count_for_session(&session.id).unwrap();

        session.name = "after checkpoint".into();
        session.touch(testing::T0 + 10);
        let node_id = session.tree.insert(ProcessNode::process(
            session.id.clone(),
            NodeKind::Subagent,
            "Reviewer",
            "/repo",
            testing::T0 + 10,
        ));
        let event = TurnEvent::new(
            session.id.clone(),
            EventKind::AgentIdle,
            EventSource::Supervisor,
            Confidence::Explicit,
            testing::T0 + 10,
        )
        .with_node(node_id.clone());
        let mut replacement_queue = AttentionQueue::new();
        replacement_queue.upsert(checkpoint_attention(
            &session.id,
            Some(node_id.clone()),
            testing::T0 + 10,
        ));

        store
            .connection()
            .execute_batch(
                "CREATE TRIGGER abort_atomic_attention_insert \
                 BEFORE INSERT ON attention_entries \
                 BEGIN \
                   SELECT RAISE(ABORT, 'injected attention failure'); \
                 END;",
            )
            .unwrap();

        assert!(store
            .checkpoint_event_session_attention(&session, &event, &replacement_queue)
            .is_err());

        let durable = store.sessions().get(&session.id).unwrap().unwrap();
        assert_eq!(durable.name, "before checkpoint");
        assert_eq!(durable.last_activity_ms, testing::T0);
        assert_eq!(
            store.nodes().count_for_session(&session.id).unwrap(),
            prior_node_count
        );
        assert!(store.nodes().get(&node_id).unwrap().is_none());
        assert_eq!(store.events().get(&event.id).unwrap(), None);
        assert_eq!(
            store.events().get(&prior_event.id).unwrap(),
            Some(prior_event)
        );
        assert_eq!(store.events().count_for_session(&session.id).unwrap(), 1);
        assert_eq!(store.attention().load_queue().unwrap(), prior_queue);
    }

    #[test]
    fn reopening_a_store_finds_what_the_last_run_wrote() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("turn.db");
        let root = temp.path().join("repo");
        std::fs::create_dir(&root).unwrap();
        let workspace = {
            let store = Store::open_at(&path).unwrap();
            let workspace = Workspace::new("turn", root.to_string_lossy(), testing::T0);
            store.workspaces().save(&workspace).unwrap();
            workspace
        };

        let store = Store::open_at(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), LATEST_VERSION);
        assert_eq!(
            store.workspaces().get(&workspace.id).unwrap().unwrap().name,
            "turn"
        );
    }

    #[test]
    fn opening_v4_canonicalises_a_legacy_root_but_keeps_reconciliation_required() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("turn.db");
        let root = temp.path().join("repo");
        std::fs::create_dir(&root).unwrap();
        let legacy_spelling = root.join(".").to_string_lossy().into_owned();
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
            migrations::apply_to(&conn, 1).unwrap();
            let attention = serde_json::to_string(&turn_core::AttentionPolicy::default()).unwrap();
            conn.execute(
                "INSERT INTO workspaces (id, name, root, env_json, init_commands_json, \
                 attention_json, created_ms, last_used_ms, tmux_enabled, archived) \
                 VALUES ('ws_old', 'legacy', ?1, '[]', '[]', ?2, 1, 1, 0, 0)",
                rusqlite::params![legacy_spelling, attention],
            )
            .unwrap();
            migrations::apply_to(&conn, 4).unwrap();
            conn.execute(
                "INSERT INTO sessions \
                     (id, workspace_id, name, cwd, env_json, attention_json, status, \
                      restore_state, tags_json, favourite, pinned, sort_key, created_ms, \
                      last_activity_ms, tmux, mode, checkout_id, worktree_path, \
                      read_only_enforced) \
                 VALUES ('sess_old', 'ws_old', 'writer', ?1, '[]', ?2, 'active', 'live', \
                         '[]', 0, 0, 0, 1, 1, 0, 'main_checkout', \
                         'checkout_primary_ws_old', NULL, 0)",
                rusqlite::params![legacy_spelling, attention],
            )
            .unwrap();
            conn.execute(
                "UPDATE checkout_write_fences SET generation = 7 WHERE canonical_path = ?1",
                rusqlite::params![legacy_spelling],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO workspace_write_leases \
                     (id, workspace_id, session_id, checkout_id, canonical_path, mode, state, \
                      acquired_ms, heartbeat_ms, released_ms, generation) \
                 VALUES ('lease_old', 'ws_old', 'sess_old', 'checkout_primary_ws_old', ?1, \
                         'exclusive_write', 'active', 1, 1, NULL, 7)",
                rusqlite::params![legacy_spelling],
            )
            .unwrap();
        }

        let store = Store::open_at(&path).unwrap();
        let workspace = store
            .workspaces()
            .get(&turn_core::ids::WorkspaceId::from_stored("ws_old"))
            .unwrap()
            .unwrap();
        let canonical = std::fs::canonicalize(&root)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert_eq!(workspace.root, canonical);
        assert!(workspace.lease_reconciliation_required);
        let checkout = store
            .hierarchy()
            .primary_checkout(&workspace.id)
            .unwrap()
            .unwrap();
        assert_eq!(checkout.path, canonical);
        assert_eq!(checkout.canonical_path, canonical);
        let lease = store
            .hierarchy()
            .active_lease(&workspace.id)
            .unwrap()
            .unwrap();
        assert_eq!(lease.state, turn_core::model::LeaseState::RecoveryRequired);
        assert_eq!(lease.generation, 7);
        let error = store
            .hierarchy()
            .acquire_write_lease(
                &workspace.id,
                &turn_core::ids::SessionId::from_stored("sess_old"),
                &checkout.id,
                2,
            )
            .unwrap_err();
        assert!(matches!(
            error,
            StoreError::WriteLeaseHeld {
                owner_session_id,
                lease_id,
                ..
            } if owner_session_id == "sess_old" && lease_id == "lease_old"
        ));
    }

    #[test]
    fn opening_v5_preserves_a_drifted_claim_and_blocks_its_live_checkout_alias() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("turn.db");
        let root = temp.path().join("workspace-root");
        let drifted = temp.path().join("still-written-checkout");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&drifted).unwrap();
        let root = std::fs::canonicalize(root)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        // Keep a deliberately non-canonical spelling in v5. Text comparison
        // alone cannot see that it aliases `drifted`.
        let drifted_spelling = drifted.join(".").to_string_lossy().into_owned();
        let drifted_canonical = std::fs::canonicalize(&drifted)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
            migrations::apply_to(&conn, 1).unwrap();
            let attention = serde_json::to_string(&turn_core::AttentionPolicy::default()).unwrap();
            conn.execute(
                "INSERT INTO workspaces (id, name, root, env_json, init_commands_json, \
                 attention_json, created_ms, last_used_ms, tmux_enabled, archived) \
                 VALUES ('ws_old', 'legacy', ?1, '[]', '[]', ?2, 1, 1, 0, 0)",
                rusqlite::params![root, attention],
            )
            .unwrap();
            migrations::apply_to(&conn, 5).unwrap();
            conn.execute(
                "INSERT INTO checkout_write_fences (canonical_path, generation) \
                 VALUES (?1, 9)",
                rusqlite::params![drifted_spelling],
            )
            .unwrap();
            conn.execute(
                "UPDATE workspace_checkouts SET path = ?1, canonical_path = ?1 \
                 WHERE workspace_id = 'ws_old' AND is_primary = 1",
                rusqlite::params![drifted_spelling],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO sessions \
                     (id, workspace_id, name, cwd, env_json, attention_json, status, \
                      restore_state, tags_json, favourite, pinned, sort_key, created_ms, \
                      last_activity_ms, tmux, mode, checkout_id, worktree_path, \
                      read_only_enforced) \
                 VALUES ('sess_old', 'ws_old', 'writer', ?1, '[]', ?2, 'active', 'live', \
                         '[]', 0, 0, 0, 1, 1, 0, 'main_checkout', \
                         'checkout_primary_ws_old', NULL, 0)",
                rusqlite::params![drifted_spelling, attention],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO workspace_write_leases \
                     (id, workspace_id, session_id, checkout_id, canonical_path, mode, state, \
                      acquired_ms, heartbeat_ms, released_ms, generation) \
                 VALUES ('lease_old', 'ws_old', 'sess_old', 'checkout_primary_ws_old', ?1, \
                         'exclusive_write', 'active', 1, 1, NULL, 9)",
                rusqlite::params![drifted_spelling],
            )
            .unwrap();
        }

        let store = Store::open_at(&path).unwrap();
        let (required, checkout_path, lease_path, lease_state): (bool, String, String, String) =
            store
                .connection()
                .query_row(
                    "SELECT w.lease_reconciliation_required, c.canonical_path, \
                            l.canonical_path, l.state \
                     FROM workspaces w \
                     JOIN workspace_checkouts c ON c.workspace_id = w.id AND c.is_primary = 1 \
                     JOIN workspace_write_leases l ON l.workspace_id = w.id \
                     WHERE w.id = 'ws_old'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .unwrap();
        assert!(required);
        assert_eq!(checkout_path, drifted_spelling);
        assert_eq!(lease_path, drifted_spelling);
        assert_eq!(lease_state, "recovery_required");

        let alias = Workspace::new("new alias", drifted_canonical, testing::T0);
        assert!(matches!(
            store.workspaces().save(&alias).unwrap_err(),
            StoreError::WorkspaceRootAlias { .. }
        ));
        assert_eq!(store.workspaces().count().unwrap(), 1);
    }

    #[test]
    fn a_database_from_a_newer_build_is_refused_at_open_time() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("turn.db");
        {
            let store = Store::open_at(&path).unwrap();
            store
                .conn
                .execute_batch(&format!("PRAGMA user_version = {}", LATEST_VERSION + 1))
                .unwrap();
        }

        let error = Store::open_at(&path).expect_err("a future schema must stop us");
        assert!(
            matches!(error, StoreError::SchemaTooNew { .. }),
            "got {error:?}"
        );
    }

    #[test]
    fn compacting_keeps_the_data_and_the_schema() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open_at(temp.path().join("turn.db")).unwrap();
        let workspace = testing::saved_workspace(&store, "turn");
        let session = testing::saved_session(&store, &workspace.id, "Fix bug");
        store.workspaces().delete(&workspace.id).unwrap();

        store.compact().unwrap();

        assert_eq!(store.schema_version().unwrap(), LATEST_VERSION);
        assert_eq!(store.workspaces().count().unwrap(), 0);
        assert!(store.sessions().get(&session.id).unwrap().is_none());
    }
}
