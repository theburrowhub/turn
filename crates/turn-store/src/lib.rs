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
pub mod migrations;
pub mod redact;
pub mod repo;

pub use error::{Result, StoreError};
pub use location::{DATABASE_FILE, DATA_DIR_ENV};
pub use migrations::{Applied, LATEST_VERSION};
pub use redact::{is_sensitive_key, REDACTED};
pub use repo::{
    AttentionRepo, EventRepo, HierarchyRepo, NodeRepo, PruneOutcome, Retention, SessionRepo, SettingsRepo,
    TemplateRepo, WorkspaceRepo,
};

use rusqlite::Connection;
use std::path::{Path, PathBuf};

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
        let conn = Connection::open(&path)?;
        let store = Self {
            conn,
            path: Some(path.clone()),
        };
        store.prepare()?;
        // After `prepare`, because enabling WAL is what creates the sidecar files.
        // All three carry the same data and so deserve the same permissions: the
        // write-ahead log holds recent rows that have not been checkpointed yet, so
        // leaving it at 0644 would defeat narrowing the database itself.
        for file in store.files() {
            if file.exists() {
                location::restrict_to_owner(&file, 0o600)?;
            }
        }
        Ok(store)
    }

    /// The database and its SQLite sidecars, whether or not they exist yet.
    fn files(&self) -> Vec<PathBuf> {
        let Some(path) = &self.path else {
            return Vec::new();
        };
        let mut out = vec![path.clone()];
        for suffix in ["-wal", "-shm"] {
            let mut sidecar = path.clone().into_os_string();
            sidecar.push(suffix);
            out.push(PathBuf::from(sidecar));
        }
        out
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
        }
        let applied = migrations::apply(&self.conn)?;
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

    pub(crate) fn saved_workspace(store: &Store, name: &str) -> Workspace {
        let workspace = Workspace::new(name, format!("/repos/{name}"), T0);
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
            reason: AwaitingReason::Permission,
            summary: None,
            confidence: Confidence::Explicit,
            created_ms: at_ms,
            updated_ms: at_ms,
            state: EntryState::Pending,
            priority_boost: 0,
        };
        store.attention().upsert(&entry).expect("stored");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use turn_core::model::Workspace;

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
    fn reopening_a_store_finds_what_the_last_run_wrote() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("turn.db");
        let workspace = {
            let store = Store::open_at(&path).unwrap();
            let workspace = Workspace::new("turn", "/repo", testing::T0);
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
