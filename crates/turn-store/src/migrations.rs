//! Schema and migrations.
//!
//! The schema version lives in SQLite's own `user_version` header field rather
//! than a table Turn has to bootstrap: it is written inside the same transaction
//! as the DDL, so a migration either lands completely or not at all — there is no
//! window where the tables are new and the recorded version is old.
//!
//! Migrations are append-only. Once a version has shipped, its statements are
//! frozen; changing them would leave every machine that already ran it with a
//! schema no later migration accounts for.
//!
//! Downgrades are refused, loudly. Opening a database from a newer build and
//! writing to it would either fail on columns this build does not know about or,
//! worse, succeed and drop the fields the newer build depends on.

use crate::error::{Result, StoreError};
use rusqlite::Connection;

/// One ordered, transactional schema change.
pub struct Migration {
    /// The `user_version` this migration produces. Strictly increasing.
    pub version: i64,
    /// Short slug, for logs and for naming a failure.
    pub name: &'static str,
    /// SQL applied as one batch inside one transaction.
    pub statements: &'static str,
}

/// Everything Turn's schema has ever been, in order.
pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "core_entities",
        statements: MIGRATION_001_CORE_ENTITIES,
    },
    Migration {
        version: 2,
        name: "attention_queue",
        statements: MIGRATION_002_ATTENTION_QUEUE,
    },
];

/// The schema version this build produces and understands.
pub const LATEST_VERSION: i64 = match MIGRATIONS.last() {
    Some(last) => last.version,
    None => 0,
};

/// What an [`apply`] call actually did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    /// The version found on disk.
    pub from: i64,
    /// The version now on disk.
    pub to: i64,
    /// Names of the migrations that ran, in order. Empty when already current,
    /// which is what makes "opening an up-to-date database changed nothing"
    /// something a test can assert rather than infer.
    pub names: Vec<&'static str>,
}

impl Applied {
    /// Whether anything was written.
    pub fn changed(&self) -> bool {
        !self.names.is_empty()
    }
}

/// Reads the schema version recorded in the database header.
pub fn schema_version(conn: &Connection) -> Result<i64> {
    Ok(conn.pragma_query_value(None, "user_version", |row| row.get(0))?)
}

/// Brings a database up to [`LATEST_VERSION`].
pub fn apply(conn: &Connection) -> Result<Applied> {
    apply_to(conn, LATEST_VERSION)
}

/// Brings a database up to a specific version.
///
/// Public because it is the only honest way to construct an older database — for
/// a test that proves the upgrade path works, or for a future staged migration
/// that wants to stop halfway.
pub fn apply_to(conn: &Connection, target: i64) -> Result<Applied> {
    let current = schema_version(conn)?;
    if current > LATEST_VERSION {
        return Err(StoreError::SchemaTooNew {
            found: current,
            supported: LATEST_VERSION,
        });
    }

    let mut names = Vec::new();
    for migration in MIGRATIONS
        .iter()
        .filter(|m| m.version > current && m.version <= target)
    {
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(migration.statements)?;
        // The version is an integer from the const table above, never user
        // input, so formatting it is safe; PRAGMA does not accept a bound
        // parameter here.
        tx.execute_batch(&format!("PRAGMA user_version = {}", migration.version))?;
        tx.commit()?;
        names.push(migration.name);
    }

    Ok(Applied {
        from: current,
        to: schema_version(conn)?,
        names,
    })
}

/// Workspaces, sessions, layouts, process nodes, templates, events, settings.
///
/// Every table is `STRICT`, so a column typed `INTEGER` rejects a string instead
/// of quietly storing one and failing to decode months later.
const MIGRATION_001_CORE_ENTITIES: &str = r#"
CREATE TABLE workspaces (
    id                 TEXT PRIMARY KEY,
    name               TEXT NOT NULL,
    root               TEXT NOT NULL,
    git_remote         TEXT,
    env_json           TEXT NOT NULL,
    default_shell      TEXT,
    default_agent      TEXT,
    init_commands_json TEXT NOT NULL,
    -- Not a foreign key: a workspace may name a template that has not been
    -- imported on this machine yet, and losing the reference would be worse
    -- than carrying a dangling one the UI can report.
    default_template   TEXT,
    attention_json     TEXT NOT NULL,
    colour             TEXT,
    icon               TEXT,
    created_ms         INTEGER NOT NULL,
    last_used_ms       INTEGER NOT NULL,
    tmux_enabled       INTEGER NOT NULL,
    archived           INTEGER NOT NULL
) STRICT;

CREATE INDEX idx_workspaces_recent ON workspaces(archived, last_used_ms DESC);

CREATE TABLE sessions (
    id               TEXT PRIMARY KEY,
    workspace_id     TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    name             TEXT NOT NULL,
    note             TEXT,
    cwd              TEXT NOT NULL,
    env_json         TEXT NOT NULL,
    attention_json   TEXT NOT NULL,
    -- Deliberately not a foreign key: deleting a template must not delete the
    -- sessions made from it, nor erase where they came from.
    template_id      TEXT,
    status           TEXT NOT NULL,
    restore_state    TEXT NOT NULL,
    tags_json        TEXT NOT NULL,
    git_branch       TEXT,
    linked_ref       TEXT,
    favourite        INTEGER NOT NULL,
    pinned           INTEGER NOT NULL,
    sort_key         INTEGER NOT NULL,
    -- A duplicated session outlives its origin, so the link is cleared rather
    -- than cascading the delete into unrelated work.
    parent_session   TEXT REFERENCES sessions(id) ON DELETE SET NULL,
    created_ms       INTEGER NOT NULL,
    last_activity_ms INTEGER NOT NULL,
    tmux             INTEGER NOT NULL
) STRICT;

CREATE INDEX idx_sessions_workspace ON sessions(workspace_id, last_activity_ms DESC);
CREATE INDEX idx_sessions_status ON sessions(status, last_activity_ms DESC);

-- The layout is a tree that Turn only ever reads and writes whole, so it is one
-- JSON document rather than a pane table plus an edge table. It lives apart from
-- `sessions` because a drag of a divider rewrites the geometry many times a
-- second and has no business touching the row the sidebar queries.
CREATE TABLE session_layouts (
    session_id  TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
    layout_json TEXT NOT NULL,
    updated_ms  INTEGER NOT NULL
) STRICT;

CREATE TABLE process_nodes (
    id                  TEXT PRIMARY KEY,
    session_id          TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    -- Position in the session's insertion order, so a restored tree renders in
    -- the order the user watched it grow instead of hash order.
    seq                 INTEGER NOT NULL,
    kind                TEXT NOT NULL,
    title               TEXT NOT NULL,
    command             TEXT NOT NULL,
    args_json           TEXT NOT NULL,
    cwd                 TEXT NOT NULL,
    pid                 INTEGER,
    ppid                INTEGER,
    lifecycle_json      TEXT NOT NULL,
    turn_json           TEXT,
    agent_json          TEXT,
    -- Lifted out of agent_json so a hook callback carrying only the tool's own
    -- session id can find its node in one indexed lookup.
    external_id         TEXT,
    -- Not a foreign key: a child's hook can arrive before its parent's spawn
    -- notification, so the column tolerates a parent that is not stored yet.
    -- Loading repairs anything still dangling.
    parent              TEXT,
    relation            TEXT NOT NULL,
    pane_id             TEXT,
    started_ms          INTEGER NOT NULL,
    ended_ms            INTEGER,
    exit_code           INTEGER,
    env_highlights_json TEXT NOT NULL,
    interaction_pending INTEGER NOT NULL
) STRICT;

CREATE INDEX idx_nodes_session ON process_nodes(session_id, seq);
CREATE INDEX idx_nodes_external ON process_nodes(external_id);
CREATE INDEX idx_nodes_pid ON process_nodes(pid);

CREATE TABLE templates (
    id                 TEXT PRIMARY KEY,
    name               TEXT NOT NULL,
    description        TEXT,
    icon               TEXT,
    layout_json        TEXT NOT NULL,
    attention_json     TEXT,
    init_commands_json TEXT NOT NULL,
    name_pattern       TEXT,
    hotkey             TEXT,
    env_json           TEXT NOT NULL,
    tmux               INTEGER NOT NULL,
    built_in           INTEGER NOT NULL,
    created_ms         INTEGER NOT NULL
) STRICT;

CREATE INDEX idx_templates_name ON templates(built_in, name);

CREATE TABLE events (
    id             TEXT PRIMARY KEY,
    timestamp_ms   INTEGER NOT NULL,
    workspace_id   TEXT,
    session_id     TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    node_id        TEXT,
    parent_node_id TEXT,
    -- The stable slug, so "every permission request today" is an indexable
    -- query and not a JSON scan.
    kind_slug      TEXT NOT NULL,
    kind_json      TEXT NOT NULL,
    agent_json     TEXT NOT NULL,
    -- Kept for every single event: Turn must be able to tell the user months
    -- later that a state it showed them was a guess, and which rule guessed it.
    confidence     TEXT NOT NULL,
    source_json    TEXT NOT NULL,
    severity       TEXT NOT NULL,
    dedup_key      TEXT NOT NULL,
    raw            TEXT
) STRICT;

CREATE INDEX idx_events_session_time ON events(session_id, timestamp_ms DESC);
CREATE INDEX idx_events_time ON events(timestamp_ms);
CREATE INDEX idx_events_dedup ON events(dedup_key, timestamp_ms DESC);

CREATE TABLE settings (
    key        TEXT PRIMARY KEY,
    value_json TEXT NOT NULL,
    updated_ms INTEGER NOT NULL
) STRICT;
"#;

/// The attention queue, so a demand the user never got to survives a restart.
const MIGRATION_002_ATTENTION_QUEUE: &str = r#"
CREATE TABLE attention_entries (
    id             TEXT PRIMARY KEY,
    session_id     TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    node_id        TEXT,
    reason         TEXT NOT NULL,
    summary        TEXT,
    confidence     TEXT NOT NULL,
    created_ms     INTEGER NOT NULL,
    updated_ms     INTEGER NOT NULL,
    state_json     TEXT NOT NULL,
    priority_boost INTEGER NOT NULL,
    -- The queue's deduplication rule, enforced by the storage layer as well as
    -- in memory: one blocked agent is one demand however often it says so.
    dedup_key      TEXT NOT NULL
) STRICT;

CREATE UNIQUE INDEX idx_attention_dedup ON attention_entries(dedup_key);
CREATE INDEX idx_attention_session ON attention_entries(session_id);
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        conn
    }

    fn table_names(conn: &Connection) -> Vec<String> {
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
            .unwrap();
        let names = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        names
    }

    #[test]
    fn versions_are_unique_and_strictly_increasing() {
        let mut previous = 0;
        for migration in MIGRATIONS {
            assert!(
                migration.version > previous,
                "{} is out of order",
                migration.name
            );
            previous = migration.version;
        }
        assert_eq!(LATEST_VERSION, previous);
    }

    #[test]
    fn a_fresh_database_gets_every_table_and_the_latest_version() {
        let conn = fresh();
        let applied = apply(&conn).unwrap();

        assert_eq!(applied.from, 0);
        assert_eq!(applied.to, LATEST_VERSION);
        assert_eq!(applied.names, vec!["core_entities", "attention_queue"]);

        let tables = table_names(&conn);
        for expected in [
            "attention_entries",
            "events",
            "process_nodes",
            "session_layouts",
            "sessions",
            "settings",
            "templates",
            "workspaces",
        ] {
            assert!(
                tables.iter().any(|t| t == expected),
                "missing table {expected}; got {tables:?}"
            );
        }
    }

    #[test]
    fn applying_twice_changes_nothing_the_second_time() {
        let conn = fresh();
        apply(&conn).unwrap();
        let before = table_names(&conn);

        let again = apply(&conn).unwrap();
        assert_eq!(again.from, LATEST_VERSION);
        assert_eq!(again.to, LATEST_VERSION);
        assert!(!again.changed(), "a current database must not be rewritten");
        assert_eq!(table_names(&conn), before);
    }

    #[test]
    fn an_older_database_is_upgraded_in_place_without_losing_its_rows() {
        let conn = fresh();
        apply_to(&conn, 1).unwrap();
        assert_eq!(schema_version(&conn).unwrap(), 1);
        assert!(
            !table_names(&conn).iter().any(|t| t == "attention_entries"),
            "v1 predates the queue table"
        );

        // A row written by the old build must survive the upgrade.
        conn.execute(
            "INSERT INTO workspaces (id, name, root, env_json, init_commands_json, \
             attention_json, created_ms, last_used_ms, tmux_enabled, archived) \
             VALUES ('ws_old', 'legacy', '/repo', '[]', '[]', '{}', 1, 1, 0, 0)",
            [],
        )
        .unwrap();

        let applied = apply(&conn).unwrap();
        assert_eq!(applied.from, 1);
        assert_eq!(applied.to, LATEST_VERSION);
        assert_eq!(applied.names, vec!["attention_queue"]);

        let name: String = conn
            .query_row("SELECT name FROM workspaces WHERE id = 'ws_old'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(name, "legacy");
        assert!(table_names(&conn).iter().any(|t| t == "attention_entries"));
    }

    #[test]
    fn a_newer_database_is_refused_instead_of_being_written_to() {
        let conn = fresh();
        apply(&conn).unwrap();
        conn.execute_batch(&format!("PRAGMA user_version = {}", LATEST_VERSION + 7))
            .unwrap();

        let error = apply(&conn).expect_err("a future schema must not be touched");
        match error {
            StoreError::SchemaTooNew { found, supported } => {
                assert_eq!(found, LATEST_VERSION + 7);
                assert_eq!(supported, LATEST_VERSION);
            }
            other => panic!("expected SchemaTooNew, got {other:?}"),
        }
        assert_eq!(
            schema_version(&conn).unwrap(),
            LATEST_VERSION + 7,
            "the refusal must leave the header exactly as it was"
        );
    }

    #[test]
    fn a_failing_migration_leaves_no_half_built_schema() {
        // Stand in for a migration that breaks halfway: the first statement is
        // valid, the second is not. Nothing from either may survive.
        let conn = fresh();
        let broken = Migration {
            version: 1,
            name: "broken",
            statements: "CREATE TABLE half_built (id TEXT) STRICT; \
                         CREATE TABLE oops (id TEXT) STRICT NOT VALID SQL;",
        };
        let tx = conn.unchecked_transaction().unwrap();
        let outcome = tx.execute_batch(broken.statements);
        assert!(outcome.is_err(), "the batch was supposed to fail");
        drop(tx);

        assert!(
            !table_names(&conn).iter().any(|t| t == "half_built"),
            "the transaction must have rolled the first statement back"
        );
        assert_eq!(schema_version(&conn).unwrap(), 0);
    }

    #[test]
    fn foreign_keys_are_enforced_so_an_event_cannot_outlive_its_session() {
        let conn = fresh();
        apply(&conn).unwrap();

        let orphan = conn.execute(
            "INSERT INTO events (id, timestamp_ms, session_id, kind_slug, kind_json, \
             agent_json, confidence, source_json, severity, dedup_key) \
             VALUES ('evt_1', 1, 'sess_ghost', 'agent.idle', '{}', '{}', 'explicit', \
             '\"supervisor\"', 'debug', 'k')",
            [],
        );
        assert!(
            orphan.is_err(),
            "an event for an unknown session must be refused"
        );
    }

    /// An index that the planner ignores is dead weight on every write, so the
    /// three queries the UI runs constantly are checked against the plan rather
    /// than assumed to be fast.
    #[test]
    fn the_hot_queries_use_their_indexes_instead_of_scanning() {
        let conn = fresh();
        apply(&conn).unwrap();

        let plan = |sql: &str| -> String {
            let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
            let details: Vec<String> = stmt
                .query_map([], |row| row.get::<_, String>(3))
                .unwrap()
                .map(|r| r.unwrap())
                .collect();
            details.join(" | ")
        };

        let events =
            plan("SELECT id FROM events WHERE session_id = 'sess_a' ORDER BY timestamp_ms DESC");
        assert!(events.contains("idx_events_session_time"), "got {events}");
        assert!(
            !events.contains("SCAN events"),
            "the event panel must not scan the whole log: {events}"
        );

        let sessions = plan(
            "SELECT id FROM sessions WHERE workspace_id = 'ws_a' ORDER BY last_activity_ms DESC",
        );
        assert!(
            sessions.contains("idx_sessions_workspace"),
            "got {sessions}"
        );

        let nodes = plan("SELECT id FROM process_nodes WHERE session_id = 'sess_a' ORDER BY seq");
        assert!(nodes.contains("idx_nodes_session"), "got {nodes}");

        let external = plan("SELECT id FROM process_nodes WHERE external_id = 'claude-abc'");
        assert!(
            external.contains("idx_nodes_external"),
            "a hook callback must find its node without a scan: {external}"
        );
    }

    #[test]
    fn strict_tables_reject_a_value_of_the_wrong_type() {
        let conn = fresh();
        apply(&conn).unwrap();
        let bad = conn.execute(
            "INSERT INTO workspaces (id, name, root, env_json, init_commands_json, \
             attention_json, created_ms, last_used_ms, tmux_enabled, archived) \
             VALUES ('ws_a', 'a', '/', '[]', '[]', '{}', 'yesterday', 1, 0, 0)",
            [],
        );
        assert!(bad.is_err(), "created_ms is an integer column");
    }
}
