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
    Migration {
        version: 3,
        name: "unified_hierarchy_and_leases",
        statements: MIGRATION_003_UNIFIED_HIERARCHY,
    },
    Migration {
        version: 4,
        name: "safe_session_checkout_modes",
        statements: MIGRATION_004_SAFE_SESSION_CHECKOUT_MODES,
    },
    Migration {
        version: 5,
        name: "drop_persisted_hook_payloads",
        statements: MIGRATION_005_DROP_PERSISTED_HOOK_PAYLOADS,
    },
    Migration {
        version: 6,
        name: "require_explicit_legacy_lease_reconciliation",
        statements: MIGRATION_006_REQUIRE_EXPLICIT_LEASE_RECONCILIATION,
    },
    Migration {
        version: 7,
        name: "attention_correlation_scope",
        statements: MIGRATION_007_ATTENTION_CORRELATION_SCOPE,
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

/// ADR-040: checkout exclusivity, AgentNode/view separation, activity previews and
/// persisted tree interaction. The old `process_nodes.pane_id` column is retained as
/// migration input; all new writes use `pane_node_bindings`.
const MIGRATION_003_UNIFIED_HIERARCHY: &str = r#"
ALTER TABLE workspaces ADD COLUMN lease_reconciliation_required INTEGER NOT NULL DEFAULT 1;

ALTER TABLE sessions ADD COLUMN mode TEXT NOT NULL DEFAULT 'read_only';
ALTER TABLE sessions ADD COLUMN checkout_id TEXT;
ALTER TABLE sessions ADD COLUMN worktree_path TEXT;
ALTER TABLE sessions ADD COLUMN read_only_enforced INTEGER NOT NULL DEFAULT 0;

ALTER TABLE process_nodes ADD COLUMN declared_name TEXT;
ALTER TABLE process_nodes ADD COLUMN display_name TEXT;
ALTER TABLE process_nodes ADD COLUMN name_source TEXT NOT NULL DEFAULT 'fallback';
ALTER TABLE process_nodes ADD COLUMN name_confidence TEXT NOT NULL DEFAULT 'unknown';
ALTER TABLE process_nodes ADD COLUMN user_renamed INTEGER NOT NULL DEFAULT 0;
ALTER TABLE process_nodes ADD COLUMN relationship_kind TEXT NOT NULL DEFAULT 'unknown';
ALTER TABLE process_nodes ADD COLUMN relationship_confidence TEXT NOT NULL DEFAULT 'unknown';
ALTER TABLE process_nodes ADD COLUMN preview_visibility TEXT NOT NULL DEFAULT 'inherit';
ALTER TABLE process_nodes ADD COLUMN activity_preview_json TEXT;

-- Preserve what is known without fabricating a parent-declared name.
UPDATE process_nodes
SET display_name = title,
    name_source = CASE WHEN kind IN ('agent', 'subagent') THEN 'process_title' ELSE 'fallback' END,
    name_confidence = CASE WHEN kind IN ('agent', 'subagent') THEN 'integrated' ELSE 'unknown' END,
    relationship_kind = CASE WHEN relation IN ('confirmed', 'inferred') THEN 'spawned_by' ELSE 'unknown' END,
    relationship_confidence = CASE
        WHEN relation = 'confirmed' THEN 'explicit'
        WHEN relation = 'inferred' THEN 'inferred_high'
        ELSE 'unknown'
    END;

-- A canonical checkout keeps its fencing generation even if every Workspace
-- pointing at it is later deleted. Reusing a generation would let a stale helper
-- from the old Workspace look current again.
CREATE TABLE checkout_write_fences (
    canonical_path TEXT PRIMARY KEY,
    generation     INTEGER NOT NULL DEFAULT 0 CHECK(generation >= 0)
) STRICT;

CREATE TABLE workspace_checkouts (
    id                    TEXT PRIMARY KEY,
    workspace_id          TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    path                  TEXT NOT NULL,
    canonical_path        TEXT NOT NULL REFERENCES checkout_write_fences(canonical_path),
    branch                TEXT,
    is_primary            INTEGER NOT NULL CHECK(is_primary IN (0, 1)),
    shared_resources_json TEXT NOT NULL DEFAULT '[]',
    created_ms            INTEGER NOT NULL,
    UNIQUE(workspace_id, id),
    UNIQUE(workspace_id, canonical_path)
) STRICT;

CREATE INDEX idx_checkouts_canonical_path ON workspace_checkouts(canonical_path);
CREATE UNIQUE INDEX idx_one_primary_checkout_per_workspace
ON workspace_checkouts(workspace_id)
WHERE is_primary = 1;

-- Stable primary checkout identities for old rows. No lease is granted here: a
-- migration cannot prove that an old process is not still writing.
INSERT OR IGNORE INTO checkout_write_fences (canonical_path, generation)
SELECT root, 0 FROM workspaces;

INSERT INTO workspace_checkouts
    (id, workspace_id, path, canonical_path, branch, is_primary, shared_resources_json, created_ms)
SELECT 'checkout_primary_' || id, id, root, root, NULL, 1, '[]', created_ms FROM workspaces;

UPDATE sessions
SET checkout_id = 'checkout_primary_' || workspace_id;

-- Composite parent keys let the lease table enforce that its Session and
-- Checkout really belong to the Workspace named on the same row. Independent
-- foreign keys would accept a Session from Workspace B beside a Checkout from A.
CREATE UNIQUE INDEX idx_sessions_workspace_identity
ON sessions(workspace_id, id);

CREATE TABLE workspace_write_leases (
    id             TEXT PRIMARY KEY,
    workspace_id   TEXT NOT NULL,
    session_id     TEXT NOT NULL,
    checkout_id    TEXT NOT NULL,
    canonical_path TEXT NOT NULL REFERENCES checkout_write_fences(canonical_path),
    mode           TEXT NOT NULL,
    state          TEXT NOT NULL,
    acquired_ms    INTEGER NOT NULL,
    heartbeat_ms   INTEGER NOT NULL,
    released_ms    INTEGER,
    generation     INTEGER NOT NULL CHECK(generation > 0),
    FOREIGN KEY(workspace_id, session_id)
        REFERENCES sessions(workspace_id, id) ON DELETE CASCADE,
    FOREIGN KEY(workspace_id, checkout_id)
        REFERENCES workspace_checkouts(workspace_id, id) ON DELETE CASCADE
) STRICT;

-- This is the final race arbiter. It is global by canonical filesystem identity,
-- not scoped to a Workspace id: two Workspace records may point through different
-- symlinks or names to the same checkout.
CREATE UNIQUE INDEX idx_one_unreconciled_canonical_writer
ON workspace_write_leases(canonical_path)
WHERE state != 'released';
CREATE UNIQUE INDEX idx_one_unreconciled_lease_per_session
ON workspace_write_leases(session_id)
WHERE state != 'released';
CREATE INDEX idx_workspace_leases_session ON workspace_write_leases(session_id, state);

-- The repository always copies canonical_path from workspace_checkouts. Keep the
-- same invariant for any future writer that talks to SQLite directly, so it cannot
-- evade the global claim by supplying a different string.
CREATE TRIGGER validate_write_lease_canonical
BEFORE INSERT ON workspace_write_leases
FOR EACH ROW
WHEN NOT EXISTS (
    SELECT 1 FROM workspace_checkouts c
    WHERE c.id = NEW.checkout_id
      AND c.workspace_id = NEW.workspace_id
      AND c.canonical_path = NEW.canonical_path
)
BEGIN
    SELECT RAISE(ABORT, 'write lease checkout identity mismatch');
END;

CREATE TABLE activity_previews (
    id                      INTEGER PRIMARY KEY,
    node_id                 TEXT NOT NULL REFERENCES process_nodes(id) ON DELETE CASCADE,
    raw_source_sequence     INTEGER,
    normalized_text         TEXT NOT NULL,
    source_type             TEXT NOT NULL,
    confidence              TEXT NOT NULL,
    stable                  INTEGER NOT NULL,
    contains_sensitive_data INTEGER NOT NULL,
    redacted                INTEGER NOT NULL,
    created_ms              INTEGER NOT NULL
) STRICT;

CREATE INDEX idx_activity_preview_latest
ON activity_previews(node_id, created_ms DESC, id DESC);

CREATE TABLE pane_node_bindings (
    pane_id    TEXT NOT NULL,
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    node_id    TEXT NOT NULL REFERENCES process_nodes(id) ON DELETE CASCADE,
    temporary  INTEGER NOT NULL DEFAULT 0,
    surface_id TEXT,
    opened_ms  INTEGER NOT NULL,
    PRIMARY KEY(session_id, pane_id)
) STRICT;

CREATE INDEX idx_pane_bindings_node ON pane_node_bindings(node_id, opened_ms);

-- Import the old one-pane projection without changing a process or layout.
INSERT INTO pane_node_bindings (pane_id, session_id, node_id, temporary, surface_id, opened_ms)
SELECT pane_id, session_id, id, 0, NULL, started_ms
FROM process_nodes
WHERE pane_id IS NOT NULL;

CREATE TABLE tree_ui_state (
    surface_id      TEXT NOT NULL,
    node_kind       TEXT NOT NULL,
    node_id         TEXT NOT NULL,
    expanded        INTEGER NOT NULL DEFAULT 0,
    selected        INTEGER NOT NULL DEFAULT 0,
    manual_order    INTEGER,
    visibility_mode TEXT,
    updated_ms      INTEGER NOT NULL,
    PRIMARY KEY(surface_id, node_kind, node_id)
) STRICT;

CREATE UNIQUE INDEX idx_tree_one_selected_per_client
ON tree_ui_state(surface_id)
WHERE selected = 1;

-- Workspace-scoped audit facts that can exist before a Session does. Runtime
-- TurnEvents remain session-scoped and are not weakened for this use case.
CREATE TABLE workspace_audit_events (
    id             TEXT PRIMARY KEY,
    workspace_id   TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    event_name     TEXT NOT NULL,
    timestamp_ms   INTEGER NOT NULL,
    session_id     TEXT,
    payload_json   TEXT NOT NULL,
    confidence     TEXT NOT NULL,
    dedup_key      TEXT NOT NULL
) STRICT;

CREATE INDEX idx_workspace_audit_time
ON workspace_audit_events(workspace_id, timestamp_ms DESC);
"#;

/// Makes the Session -> Checkout relation a database invariant. SQLite cannot
/// add a composite foreign key to the existing `sessions` table without a table
/// rebuild, so guarded inserts/updates provide the same protection while keeping
/// the append-only v3 migration intact.
const MIGRATION_004_SAFE_SESSION_CHECKOUT_MODES: &str = r#"
-- The v3 generic API could persist an isolated mode without registering its
-- checkout. Such a row is not proof that a worktree exists. Downgrade malformed
-- relations to an honest, unenforced reader of the primary checkout before the
-- guards become active; valid registered worktrees are preserved.
UPDATE sessions
SET mode = 'read_only',
    checkout_id = (
        SELECT c.id FROM workspace_checkouts c
        WHERE c.workspace_id = sessions.workspace_id AND c.is_primary = 1
    ),
    worktree_path = NULL,
    read_only_enforced = 0
WHERE NOT EXISTS (
    SELECT 1 FROM workspace_checkouts c
    WHERE c.id = sessions.checkout_id
      AND c.workspace_id = sessions.workspace_id
      AND (
          (sessions.mode IN ('main_checkout', 'read_only')
              AND c.is_primary = 1
              AND sessions.worktree_path IS NULL)
          OR
          (sessions.mode = 'isolated_worktree'
              AND c.is_primary = 0
              AND sessions.worktree_path = c.path)
      )
);

CREATE TRIGGER validate_session_checkout_insert
BEFORE INSERT ON sessions
FOR EACH ROW
WHEN EXISTS (SELECT 1 FROM workspaces w WHERE w.id = NEW.workspace_id)
AND (
    NOT EXISTS (
        SELECT 1 FROM workspace_checkouts c
        WHERE c.id = NEW.checkout_id
          AND c.workspace_id = NEW.workspace_id
          AND (
              (NEW.mode IN ('main_checkout', 'read_only')
                  AND c.is_primary = 1
                  AND NEW.worktree_path IS NULL)
              OR
              (NEW.mode = 'isolated_worktree'
                  AND c.is_primary = 0
                  AND NEW.worktree_path = c.path)
          )
    )
    OR (NEW.read_only_enforced != 0 AND NEW.mode != 'read_only')
)
BEGIN
    SELECT RAISE(ABORT, 'invalid session checkout mode');
END;

CREATE TRIGGER validate_session_checkout_update
BEFORE UPDATE OF workspace_id, checkout_id, mode, worktree_path, read_only_enforced ON sessions
FOR EACH ROW
WHEN EXISTS (SELECT 1 FROM workspaces w WHERE w.id = NEW.workspace_id)
AND (
    NOT EXISTS (
        SELECT 1 FROM workspace_checkouts c
        WHERE c.id = NEW.checkout_id
          AND c.workspace_id = NEW.workspace_id
          AND (
              (NEW.mode IN ('main_checkout', 'read_only')
                  AND c.is_primary = 1
                  AND NEW.worktree_path IS NULL)
              OR
              (NEW.mode = 'isolated_worktree'
                  AND c.is_primary = 0
                  AND NEW.worktree_path = c.path)
          )
    )
    OR (NEW.read_only_enforced != 0 AND NEW.mode != 'read_only')
)
BEGIN
    SELECT RAISE(ABORT, 'invalid session checkout mode');
END;

-- Primary checkout aliases are intentional. An isolated worktree, however,
-- must never share a canonical directory with any checkout, primary or isolated.
CREATE TRIGGER isolate_checkout_path_insert
BEFORE INSERT ON workspace_checkouts
FOR EACH ROW
WHEN EXISTS (
    SELECT 1 FROM workspace_checkouts c
    WHERE c.canonical_path = NEW.canonical_path
      AND (NEW.is_primary = 0 OR c.is_primary = 0)
)
BEGIN
    SELECT RAISE(ABORT, 'isolated checkout aliases an existing checkout');
END;

CREATE TRIGGER isolate_checkout_path_update
BEFORE UPDATE OF canonical_path, is_primary ON workspace_checkouts
FOR EACH ROW
WHEN EXISTS (
    SELECT 1 FROM workspace_checkouts c
    WHERE c.canonical_path = NEW.canonical_path
      AND c.id != NEW.id
      AND (NEW.is_primary = 0 OR c.is_primary = 0)
)
BEGIN
    SELECT RAISE(ABORT, 'isolated checkout aliases an existing checkout');
END;
"#;

/// Removes callback bodies written by builds predating ADR-040's durable
/// boundary. The typed event, provenance and confidence remain intact; only the
/// opaque hook body is destroyed.
///
/// `EventSource` uses serde's externally tagged representation, so every hook
/// source starts with `{"hook":`. Keeping this independent of JSON1 makes the
/// migration work with every SQLite build Turn supports.
const MIGRATION_005_DROP_PERSISTED_HOOK_PAYLOADS: &str = r#"
UPDATE events
SET raw = NULL
WHERE raw IS NOT NULL
  AND source_json LIKE '{"hook":%';

-- An UPDATE removes the value logically but can leave its old bytes in free
-- pages or the WAL. The Store sees this durable marker after the migration
-- commits, rebuilds/checkpoints the file, and deletes the marker only after that
-- succeeds. A failed cleanup is therefore retried on the next open.
INSERT INTO settings (key, value_json, updated_ms)
SELECT 'security.hook_raw_purge_pending', 'true', 0
WHERE changes() > 0
ON CONFLICT(key) DO UPDATE SET value_json = 'true', updated_ms = 0;
"#;

/// v3 used the caller's path spelling as the checkout fence during migration,
/// and v4 could clear the legacy guard merely by acquiring a lease. A migration
/// cannot consult the filesystem safely, so it does the only conservative thing:
/// every pre-existing Workspace must pass an explicit reconciliation flow and
/// every allegedly live writer becomes recovery-required. `Store::prepare`
/// canonicalises resolvable roots afterwards, but never clears this flag.
const MIGRATION_006_REQUIRE_EXPLICIT_LEASE_RECONCILIATION: &str = r#"
UPDATE workspaces
SET lease_reconciliation_required = 1;

UPDATE workspace_write_leases
SET state = 'recovery_required'
WHERE state = 'active';

-- Historical aliases stay queryable while reconciliation is pending, but a
-- newly safe Workspace (flag = 0) may never add another name for an existing
-- primary checkout. The repository performs the same check to return a typed
-- error; these triggers remain the final arbiter for direct SQLite writers.
CREATE TRIGGER prevent_safe_primary_checkout_alias_insert
BEFORE INSERT ON workspace_checkouts
FOR EACH ROW
WHEN NEW.is_primary = 1
AND COALESCE((
    SELECT w.lease_reconciliation_required FROM workspaces w
    WHERE w.id = NEW.workspace_id
), 1) = 0
AND EXISTS (
    SELECT 1 FROM workspace_checkouts c
    WHERE c.canonical_path = NEW.canonical_path
      AND c.workspace_id != NEW.workspace_id
    UNION
    SELECT 1 FROM workspaces other
    WHERE other.root = NEW.canonical_path
      AND other.id != NEW.workspace_id
)
BEGIN
    SELECT RAISE(ABORT, 'primary checkout aliases an existing workspace');
END;

CREATE TRIGGER validate_safe_primary_checkout_identity_insert
BEFORE INSERT ON workspace_checkouts
FOR EACH ROW
WHEN NEW.is_primary = 1
AND COALESCE((
    SELECT w.lease_reconciliation_required FROM workspaces w
    WHERE w.id = NEW.workspace_id
), 1) = 0
AND EXISTS (
    SELECT 1 FROM workspaces w
    WHERE w.id = NEW.workspace_id
      AND (NEW.path != w.root OR NEW.canonical_path != w.root)
)
BEGIN
    SELECT RAISE(ABORT, 'primary checkout identity differs from workspace root');
END;

CREATE TRIGGER prevent_safe_primary_checkout_alias_update
BEFORE UPDATE OF canonical_path, is_primary ON workspace_checkouts
FOR EACH ROW
WHEN NEW.is_primary = 1
AND COALESCE((
    SELECT w.lease_reconciliation_required FROM workspaces w
    WHERE w.id = NEW.workspace_id
), 1) = 0
AND EXISTS (
    SELECT 1 FROM workspace_checkouts c
    WHERE c.canonical_path = NEW.canonical_path
      AND c.workspace_id != NEW.workspace_id
    UNION
    SELECT 1 FROM workspaces other
    WHERE other.root = NEW.canonical_path
      AND other.id != NEW.workspace_id
)
BEGIN
    SELECT RAISE(ABORT, 'primary checkout aliases an existing workspace');
END;

CREATE TRIGGER validate_safe_primary_checkout_identity_update
BEFORE UPDATE OF path, canonical_path, is_primary ON workspace_checkouts
FOR EACH ROW
WHEN NEW.is_primary = 1
AND COALESCE((
    SELECT w.lease_reconciliation_required FROM workspaces w
    WHERE w.id = NEW.workspace_id
), 1) = 0
AND EXISTS (
    SELECT 1 FROM workspaces w
    WHERE w.id = NEW.workspace_id
      AND (NEW.path != w.root OR NEW.canonical_path != w.root)
)
BEGIN
    SELECT RAISE(ABORT, 'primary checkout identity differs from workspace root');
END;

CREATE TRIGGER prevent_safe_workspace_root_retarget
BEFORE UPDATE OF root ON workspaces
FOR EACH ROW
WHEN OLD.lease_reconciliation_required = 0
AND NEW.lease_reconciliation_required = 0
AND NEW.root != OLD.root
BEGIN
    SELECT RAISE(ABORT, 'workspace root change requires reconciliation');
END;

-- Builds predating this migration cleared the guard as a side effect of lease
-- acquisition. Fail closed even if one of those daemons is still connected while
-- a newer daemon upgrades the database. A future explicit reconciliation API
-- must replace this trigger with its own auditable, fenced transition.
CREATE TRIGGER prevent_implicit_workspace_reconciliation
BEFORE UPDATE OF lease_reconciliation_required ON workspaces
FOR EACH ROW
WHEN OLD.lease_reconciliation_required = 1
AND NEW.lease_reconciliation_required = 0
BEGIN
    SELECT RAISE(ABORT, 'workspace reconciliation requires an explicit fenced flow');
END;

CREATE TRIGGER prevent_alias_reconciliation_without_unique_root
BEFORE UPDATE OF lease_reconciliation_required ON workspaces
FOR EACH ROW
WHEN NEW.lease_reconciliation_required = 0
AND EXISTS (
    SELECT 1
    FROM workspace_checkouts own
    JOIN workspace_checkouts other
      ON other.canonical_path = own.canonical_path
     AND other.workspace_id != own.workspace_id
    WHERE own.workspace_id = NEW.id AND own.is_primary = 1
    UNION
    SELECT 1 FROM workspaces other
    WHERE other.root = NEW.root AND other.id != NEW.id
    UNION
    SELECT 1 FROM workspace_checkouts own
    WHERE own.workspace_id = NEW.id AND own.is_primary = 1
      AND (own.path != NEW.root OR own.canonical_path != NEW.root)
    UNION
    SELECT 1 WHERE NOT EXISTS (
        SELECT 1 FROM workspace_checkouts own
        WHERE own.workspace_id = NEW.id AND own.is_primary = 1
    )
)
BEGIN
    SELECT RAISE(ABORT, 'aliased workspace cannot be marked reconciled');
END;
"#;

/// A node-less worker callback still has an authenticated hook parent and may
/// carry the tool's own worker id before the matching AgentNode is declared.
/// Persist both so restart, deduplication and resolution keep the same narrow
/// boundary instead of falling back to every demand in the Session.
const MIGRATION_007_ATTENTION_CORRELATION_SCOPE: &str = r#"
ALTER TABLE attention_entries ADD COLUMN parent_node_id TEXT;
ALTER TABLE attention_entries ADD COLUMN subject_external_id TEXT;

-- The in-memory key now names the kind of subject explicitly. Re-key legacy
-- rows in place so the first post-upgrade callback deduplicates even before a
-- daemon has had an opportunity to load and checkpoint the queue.
UPDATE attention_entries
SET dedup_key = session_id || '|' ||
    CASE
        WHEN node_id IS NOT NULL THEN 'node:' || node_id
        ELSE 'unassigned'
    END || '|' ||
    CASE reason
        WHEN 'question' THEN 'Question'
        WHEN 'permission' THEN 'Permission'
        WHEN 'input' THEN 'Input'
        WHEN 'credentials' THEN 'Credentials'
        ELSE reason
    END;

CREATE INDEX idx_attention_correlation_scope
ON attention_entries(session_id, parent_node_id, subject_external_id)
WHERE node_id IS NULL;
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

    fn column_names(conn: &Connection, table: &str) -> Vec<String> {
        let mut stmt = conn
            .prepare(&format!("PRAGMA table_info({table})"))
            .unwrap();
        stmt.query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .map(|row| row.unwrap())
            .collect()
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
        assert_eq!(
            applied.names,
            vec![
                "core_entities",
                "attention_queue",
                "unified_hierarchy_and_leases",
                "safe_session_checkout_modes",
                "drop_persisted_hook_payloads",
                "require_explicit_legacy_lease_reconciliation",
                "attention_correlation_scope"
            ]
        );

        let tables = table_names(&conn);
        for expected in [
            "attention_entries",
            "activity_previews",
            "checkout_write_fences",
            "events",
            "pane_node_bindings",
            "process_nodes",
            "session_layouts",
            "sessions",
            "settings",
            "templates",
            "tree_ui_state",
            "workspace_audit_events",
            "workspace_checkouts",
            "workspace_write_leases",
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
    fn v7_adds_durable_attention_correlation_scope_append_only() {
        let conn = fresh();
        apply_to(&conn, 6).unwrap();
        let before = column_names(&conn, "attention_entries");
        assert!(!before.iter().any(|column| column == "parent_node_id"));
        assert!(!before.iter().any(|column| column == "subject_external_id"));
        conn.execute(
            "INSERT INTO workspaces (id, name, root, env_json, init_commands_json, \
             attention_json, created_ms, last_used_ms, tmux_enabled, archived) \
             VALUES ('ws_scope', 'scope', '/scope', '[]', '[]', '{}', 1, 1, 0, 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO checkout_write_fences (canonical_path, generation) \
             VALUES ('/scope', 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO workspace_checkouts (id, workspace_id, path, canonical_path, branch, \
             is_primary, shared_resources_json, created_ms) VALUES ('checkout_scope', \
             'ws_scope', '/scope', '/scope', NULL, 1, '[]', 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sessions (id, workspace_id, name, cwd, env_json, attention_json, \
             status, restore_state, tags_json, favourite, pinned, sort_key, created_ms, \
             last_activity_ms, tmux, mode, checkout_id, read_only_enforced) VALUES \
             ('sess_scope', 'ws_scope', 'scope', '/scope', '[]', '{}', 'active', 'live', \
             '[]', 0, 0, 0, 1, 1, 0, 'read_only', 'checkout_scope', 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO attention_entries (id, session_id, node_id, reason, summary, \
             confidence, created_ms, updated_ms, state_json, priority_boost, dedup_key) \
             VALUES ('att_scope', 'sess_scope', NULL, 'permission', NULL, 'explicit', \
             1, 1, '{\"kind\":\"pending\"}', 0, 'sess_scope|-|Permission')",
            [],
        )
        .unwrap();

        let applied = apply(&conn).unwrap();
        assert_eq!(applied.names, vec!["attention_correlation_scope"]);
        let after = column_names(&conn, "attention_entries");
        assert!(after.iter().any(|column| column == "parent_node_id"));
        assert!(after.iter().any(|column| column == "subject_external_id"));
        for column in before {
            assert!(after.contains(&column), "v7 removed legacy column {column}");
        }
        let rekeyed: String = conn
            .query_row(
                "SELECT dedup_key FROM attention_entries WHERE id = 'att_scope'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(rekeyed, "sess_scope|unassigned|Permission");
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
        assert_eq!(
            applied.names,
            vec![
                "attention_queue",
                "unified_hierarchy_and_leases",
                "safe_session_checkout_modes",
                "drop_persisted_hook_payloads",
                "require_explicit_legacy_lease_reconciliation",
                "attention_correlation_scope"
            ]
        );

        let name: String = conn
            .query_row("SELECT name FROM workspaces WHERE id = 'ws_old'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(name, "legacy");
        assert!(table_names(&conn).iter().any(|t| t == "attention_entries"));
    }

    #[test]
    fn v3_orphaned_worktree_claims_become_honest_primary_readers() {
        let conn = fresh();
        apply_to(&conn, 1).unwrap();
        conn.execute(
            "INSERT INTO workspaces (id, name, root, env_json, init_commands_json, \
             attention_json, created_ms, last_used_ms, tmux_enabled, archived) \
             VALUES ('ws_old', 'legacy', '/repo', '[]', '[]', '{}', 1, 1, 0, 0)",
            [],
        )
        .unwrap();
        apply_to(&conn, 3).unwrap();
        conn.execute(
            "INSERT INTO sessions \
                 (id, workspace_id, name, cwd, env_json, attention_json, status, \
                  restore_state, tags_json, favourite, pinned, sort_key, created_ms, \
                  last_activity_ms, tmux, mode, checkout_id, worktree_path, \
                  read_only_enforced) \
             VALUES ('sess_old', 'ws_old', 'unsafe worktree', '/missing', '[]', '{}', \
                     'active', 'live', '[]', 0, 0, 0, 1, 1, 0, 'isolated_worktree', \
                     'checkout_missing', '/missing', 1)",
            [],
        )
        .unwrap();

        let applied = apply(&conn).unwrap();
        assert_eq!(
            applied.names,
            vec![
                "safe_session_checkout_modes",
                "drop_persisted_hook_payloads",
                "require_explicit_legacy_lease_reconciliation",
                "attention_correlation_scope"
            ]
        );
        let repaired: (String, String, Option<String>, bool) = conn
            .query_row(
                "SELECT mode, checkout_id, worktree_path, read_only_enforced \
                 FROM sessions WHERE id = 'sess_old'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            repaired,
            (
                "read_only".into(),
                "checkout_primary_ws_old".into(),
                None,
                false
            )
        );
    }

    #[test]
    fn v5_deletes_historical_hook_bodies_without_losing_typed_events_or_other_notes() {
        let conn = fresh();
        apply_to(&conn, 1).unwrap();
        conn.execute(
            "INSERT INTO workspaces (id, name, root, env_json, init_commands_json, \
             attention_json, created_ms, last_used_ms, tmux_enabled, archived) \
             VALUES ('ws_old', 'legacy', '/repo', '[]', '[]', '{}', 1, 1, 0, 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sessions \
                 (id, workspace_id, name, cwd, env_json, attention_json, status, \
                  restore_state, tags_json, favourite, pinned, sort_key, created_ms, \
                  last_activity_ms, tmux) \
             VALUES ('sess_old', 'ws_old', 'work', '/repo', '[]', '{}', 'active', \
                     'live', '[]', 0, 0, 0, 1, 1, 0)",
            [],
        )
        .unwrap();
        let secret = "historical-free-text-secret-8675309";
        conn.execute(
            "INSERT INTO events \
                 (id, timestamp_ms, session_id, kind_slug, kind_json, agent_json, \
                  confidence, source_json, severity, dedup_key, raw) \
             VALUES ('evt_hook', 1, 'sess_old', 'agent.idle', '{}', '{}', 'explicit', \
                     '{\"hook\":{\"tool\":\"claude-code\",\"event_name\":\"Stop\"}}', \
                     'debug', 'hook', ?1)",
            [secret],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO events \
                 (id, timestamp_ms, session_id, kind_slug, kind_json, agent_json, \
                  confidence, source_json, severity, dedup_key, raw) \
             VALUES ('evt_supervisor', 2, 'sess_old', 'agent.idle', '{}', '{}', \
                     'explicit', '\"supervisor\"', 'debug', 'supervisor', \
                     'process disappeared')",
            [],
        )
        .unwrap();

        let applied = apply_to(&conn, 5).unwrap();
        assert_eq!(applied.names.last(), Some(&"drop_persisted_hook_payloads"));
        let hook: (String, Option<String>) = conn
            .query_row(
                "SELECT kind_slug, raw FROM events WHERE id = 'evt_hook'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(hook, ("agent.idle".into(), None));
        let supervisor: Option<String> = conn
            .query_row(
                "SELECT raw FROM events WHERE id = 'evt_supervisor'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(supervisor.as_deref(), Some("process disappeared"));
        let pending: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM settings \
                 WHERE key = 'security.hook_raw_purge_pending')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(pending, "the physical purge must be retried by Store::open");
    }

    #[test]
    fn v6_from_v5_never_trusts_a_legacy_active_writer_or_clears_reconciliation() {
        let conn = fresh();
        apply_to(&conn, 1).unwrap();
        conn.execute(
            "INSERT INTO workspaces (id, name, root, env_json, init_commands_json, \
             attention_json, created_ms, last_used_ms, tmux_enabled, archived) \
             VALUES ('ws_old', 'legacy', '/repo', '[]', '[]', '{}', 1, 1, 0, 0)",
            [],
        )
        .unwrap();
        apply_to(&conn, 5).unwrap();
        conn.execute(
            "INSERT INTO sessions \
                 (id, workspace_id, name, cwd, env_json, attention_json, status, \
                  restore_state, tags_json, favourite, pinned, sort_key, created_ms, \
                  last_activity_ms, tmux, mode, checkout_id, worktree_path, \
                  read_only_enforced) \
             VALUES ('sess_old', 'ws_old', 'writer', '/repo', '[]', '{}', 'active', \
                     'live', '[]', 0, 0, 0, 1, 1, 0, 'main_checkout', \
                     'checkout_primary_ws_old', NULL, 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE checkout_write_fences SET generation = 1 WHERE canonical_path = '/repo'",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO workspace_write_leases \
                 (id, workspace_id, session_id, checkout_id, canonical_path, mode, state, \
                  acquired_ms, heartbeat_ms, released_ms, generation) \
             VALUES ('lease_old', 'ws_old', 'sess_old', 'checkout_primary_ws_old', \
                     '/repo', 'exclusive_write', 'active', 1, 1, NULL, 1)",
            [],
        )
        .unwrap();

        let applied = apply(&conn).unwrap();
        assert_eq!(
            applied.names,
            vec![
                "require_explicit_legacy_lease_reconciliation",
                "attention_correlation_scope"
            ]
        );
        let (required, state): (bool, String) = conn
            .query_row(
                "SELECT w.lease_reconciliation_required, l.state \
                 FROM workspaces w JOIN workspace_write_leases l ON l.workspace_id = w.id \
                 WHERE w.id = 'ws_old'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert!(required);
        assert_eq!(state, "recovery_required");

        let implicit_clear = conn
            .execute(
                "UPDATE workspaces SET lease_reconciliation_required = 0 WHERE id = 'ws_old'",
                [],
            )
            .expect_err("an old daemon must not clear the v6 gate as a lease side effect");
        assert!(matches!(
            implicit_clear,
            rusqlite::Error::SqliteFailure(code, _)
                if code.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_TRIGGER
        ));
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
