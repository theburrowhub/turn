//! One-shot, retryable maintenance for durable text written by older builds.
//!
//! Current repositories redact before every write. That does not erase a token
//! which an older binary already committed: SQLite can retain the previous cell
//! in a free page or in the WAL even after an `UPDATE`. Migration v9 therefore
//! leaves a durable marker and this module owns the second, non-transactional
//! half of the upgrade: redact logically, rebuild the database, truncate the WAL,
//! and only then clear the marker.

use crate::error::{Result, StoreError};
use crate::redact::{
    activity_preview_for_persistence, agent_info_for_persistence, redact_json, redact_layout,
    redact_pairs, redact_secrets,
};
use rusqlite::{params, Connection, Transaction, TransactionBehavior};
use std::collections::{BTreeMap, BTreeSet};
use turn_core::model::{ActivityPreview, AgentInfo, Layout};

pub(crate) const LEGACY_FREE_TEXT_PURGE_MARKER: &str = "security.legacy_free_text_purge_pending";
const LEGACY_HOOK_PURGE_MARKER: &str = "security.hook_raw_purge_pending";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RedactionKind {
    Scalar,
    Json,
    Environment,
    Layout,
    AgentInfo,
    ActivityPreview,
    PreviewText,
    AttentionDedup,
}

#[derive(Clone, Copy, Debug)]
struct RedactableColumn {
    table: &'static str,
    column: &'static str,
    kind: RedactionKind,
}

impl RedactableColumn {
    const fn new(table: &'static str, column: &'static str, kind: RedactionKind) -> Self {
        Self {
            table,
            column,
            kind,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct InvariantColumn {
    table: &'static str,
    column: &'static str,
    description: &'static str,
}

impl InvariantColumn {
    const fn new(table: &'static str, column: &'static str, description: &'static str) -> Self {
        Self {
            table,
            column,
            description,
        }
    }
}

/// Every current TEXT column whose value may contain user/tool supplied prose.
///
/// This is deliberately explicit. [`validate_schema_coverage`] makes an added
/// TEXT column fail closed until somebody classifies it here or below.
const REDACTABLE_COLUMNS: &[RedactableColumn] = &[
    RedactableColumn::new("workspaces", "name", RedactionKind::Scalar),
    RedactableColumn::new("workspaces", "git_remote", RedactionKind::Scalar),
    RedactableColumn::new("workspaces", "env_json", RedactionKind::Environment),
    RedactableColumn::new("workspaces", "default_shell", RedactionKind::Scalar),
    RedactableColumn::new("workspaces", "default_agent", RedactionKind::Scalar),
    RedactableColumn::new("workspaces", "init_commands_json", RedactionKind::Json),
    RedactableColumn::new("workspaces", "attention_json", RedactionKind::Json),
    RedactableColumn::new("workspaces", "colour", RedactionKind::Scalar),
    RedactableColumn::new("workspaces", "icon", RedactionKind::Scalar),
    RedactableColumn::new("sessions", "name", RedactionKind::Scalar),
    RedactableColumn::new("sessions", "note", RedactionKind::Scalar),
    RedactableColumn::new("sessions", "cwd", RedactionKind::Scalar),
    RedactableColumn::new("sessions", "env_json", RedactionKind::Environment),
    RedactableColumn::new("sessions", "attention_json", RedactionKind::Json),
    RedactableColumn::new("sessions", "tags_json", RedactionKind::Json),
    RedactableColumn::new("sessions", "git_branch", RedactionKind::Scalar),
    RedactableColumn::new("sessions", "linked_ref", RedactionKind::Scalar),
    RedactableColumn::new("session_layouts", "layout_json", RedactionKind::Layout),
    RedactableColumn::new("process_nodes", "title", RedactionKind::Scalar),
    RedactableColumn::new("process_nodes", "command", RedactionKind::Scalar),
    RedactableColumn::new("process_nodes", "args_json", RedactionKind::Json),
    RedactableColumn::new("process_nodes", "cwd", RedactionKind::Scalar),
    RedactableColumn::new("process_nodes", "lifecycle_json", RedactionKind::Json),
    RedactableColumn::new("process_nodes", "turn_json", RedactionKind::Json),
    RedactableColumn::new("process_nodes", "agent_json", RedactionKind::AgentInfo),
    RedactableColumn::new("process_nodes", "external_id", RedactionKind::Scalar),
    RedactableColumn::new("process_nodes", "env_highlights_json", RedactionKind::Json),
    RedactableColumn::new("process_nodes", "declared_name", RedactionKind::Scalar),
    RedactableColumn::new("process_nodes", "display_name", RedactionKind::Scalar),
    RedactableColumn::new(
        "process_nodes",
        "activity_preview_json",
        RedactionKind::ActivityPreview,
    ),
    RedactableColumn::new("templates", "name", RedactionKind::Scalar),
    RedactableColumn::new("templates", "description", RedactionKind::Scalar),
    RedactableColumn::new("templates", "icon", RedactionKind::Scalar),
    RedactableColumn::new("templates", "layout_json", RedactionKind::Layout),
    RedactableColumn::new("templates", "attention_json", RedactionKind::Json),
    RedactableColumn::new("templates", "init_commands_json", RedactionKind::Json),
    RedactableColumn::new("templates", "name_pattern", RedactionKind::Scalar),
    RedactableColumn::new("templates", "hotkey", RedactionKind::Scalar),
    RedactableColumn::new("templates", "env_json", RedactionKind::Environment),
    RedactableColumn::new("events", "kind_json", RedactionKind::Json),
    RedactableColumn::new("events", "agent_json", RedactionKind::Json),
    RedactableColumn::new("events", "source_json", RedactionKind::Json),
    RedactableColumn::new("events", "dedup_key", RedactionKind::Scalar),
    RedactableColumn::new("events", "raw", RedactionKind::Json),
    RedactableColumn::new("settings", "value_json", RedactionKind::Json),
    RedactableColumn::new(
        "attention_entries",
        "subject_external_id",
        RedactionKind::Scalar,
    ),
    RedactableColumn::new("attention_entries", "summary", RedactionKind::Scalar),
    RedactableColumn::new("attention_entries", "state_json", RedactionKind::Json),
    RedactableColumn::new(
        "attention_entries",
        "dedup_key",
        RedactionKind::AttentionDedup,
    ),
    RedactableColumn::new("workspace_checkouts", "branch", RedactionKind::Scalar),
    RedactableColumn::new(
        "workspace_checkouts",
        "shared_resources_json",
        RedactionKind::Json,
    ),
    RedactableColumn::new(
        "activity_previews",
        "normalized_text",
        RedactionKind::PreviewText,
    ),
    RedactableColumn::new(
        "workspace_audit_events",
        "payload_json",
        RedactionKind::Json,
    ),
    RedactableColumn::new("workspace_audit_events", "dedup_key", RedactionKind::Scalar),
];

/// Every remaining TEXT column is identity, a relationship, or a closed tag.
/// Those values are scanned, but never rewritten: changing one could break a
/// foreign key, a checkout fence, a process relationship, or decoding semantics.
const INVARIANT_COLUMNS: &[InvariantColumn] = &[
    InvariantColumn::new("workspaces", "id", "Workspace id"),
    InvariantColumn::new("workspaces", "root", "workspace root"),
    InvariantColumn::new("workspaces", "default_template", "Template id"),
    InvariantColumn::new("sessions", "id", "Session id"),
    InvariantColumn::new("sessions", "workspace_id", "Workspace id"),
    InvariantColumn::new("sessions", "template_id", "Template id"),
    InvariantColumn::new("sessions", "status", "Session state"),
    InvariantColumn::new("sessions", "restore_state", "restore state"),
    InvariantColumn::new("sessions", "parent_session", "parent Session id"),
    InvariantColumn::new("sessions", "mode", "Session mode"),
    InvariantColumn::new("sessions", "checkout_id", "Checkout id"),
    InvariantColumn::new("sessions", "worktree_path", "worktree path"),
    InvariantColumn::new("session_layouts", "session_id", "Session id"),
    InvariantColumn::new("process_nodes", "id", "Process Node id"),
    InvariantColumn::new("process_nodes", "session_id", "Session id"),
    InvariantColumn::new("process_nodes", "kind", "Process Node kind"),
    InvariantColumn::new("process_nodes", "parent", "parent Process Node id"),
    InvariantColumn::new("process_nodes", "relation", "process relation"),
    InvariantColumn::new("process_nodes", "pane_id", "Pane id"),
    InvariantColumn::new("process_nodes", "name_source", "agent name source"),
    InvariantColumn::new("process_nodes", "name_confidence", "agent name confidence"),
    InvariantColumn::new(
        "process_nodes",
        "relationship_kind",
        "agent relationship kind",
    ),
    InvariantColumn::new(
        "process_nodes",
        "relationship_confidence",
        "agent relationship confidence",
    ),
    InvariantColumn::new("process_nodes", "preview_visibility", "preview visibility"),
    InvariantColumn::new("templates", "id", "Template id"),
    InvariantColumn::new("events", "id", "Event id"),
    InvariantColumn::new("events", "workspace_id", "Workspace id"),
    InvariantColumn::new("events", "session_id", "Session id"),
    InvariantColumn::new("events", "node_id", "Process Node id"),
    InvariantColumn::new("events", "parent_node_id", "parent Process Node id"),
    InvariantColumn::new("events", "kind_slug", "event kind"),
    InvariantColumn::new("events", "confidence", "event confidence"),
    InvariantColumn::new("events", "severity", "event severity"),
    InvariantColumn::new("settings", "key", "setting key"),
    InvariantColumn::new("attention_entries", "id", "Attention id"),
    InvariantColumn::new("attention_entries", "session_id", "Session id"),
    InvariantColumn::new("attention_entries", "node_id", "Process Node id"),
    InvariantColumn::new(
        "attention_entries",
        "parent_node_id",
        "parent Process Node id",
    ),
    InvariantColumn::new("attention_entries", "reason", "attention reason"),
    InvariantColumn::new("attention_entries", "confidence", "attention confidence"),
    InvariantColumn::new("attention_entries", "demand_kind", "attention demand kind"),
    InvariantColumn::new(
        "checkout_write_fences",
        "canonical_path",
        "checkout fence path",
    ),
    InvariantColumn::new("workspace_checkouts", "id", "Checkout id"),
    InvariantColumn::new("workspace_checkouts", "workspace_id", "Workspace id"),
    InvariantColumn::new("workspace_checkouts", "path", "checkout path"),
    InvariantColumn::new(
        "workspace_checkouts",
        "canonical_path",
        "canonical checkout path",
    ),
    InvariantColumn::new("workspace_write_leases", "id", "Workspace Lease id"),
    InvariantColumn::new("workspace_write_leases", "workspace_id", "Workspace id"),
    InvariantColumn::new("workspace_write_leases", "session_id", "Session id"),
    InvariantColumn::new("workspace_write_leases", "checkout_id", "Checkout id"),
    InvariantColumn::new(
        "workspace_write_leases",
        "canonical_path",
        "leased checkout path",
    ),
    InvariantColumn::new("workspace_write_leases", "mode", "Workspace Lease mode"),
    InvariantColumn::new("workspace_write_leases", "state", "Workspace Lease state"),
    InvariantColumn::new("activity_previews", "node_id", "Process Node id"),
    InvariantColumn::new("activity_previews", "source_type", "preview source"),
    InvariantColumn::new("activity_previews", "confidence", "preview confidence"),
    InvariantColumn::new("pane_node_bindings", "pane_id", "Pane id"),
    InvariantColumn::new("pane_node_bindings", "session_id", "Session id"),
    InvariantColumn::new("pane_node_bindings", "node_id", "Process Node id"),
    InvariantColumn::new("pane_node_bindings", "surface_id", "surface id"),
    InvariantColumn::new("tree_ui_state", "surface_id", "surface id"),
    InvariantColumn::new("tree_ui_state", "node_kind", "tree node kind"),
    InvariantColumn::new("tree_ui_state", "node_id", "tree node id"),
    InvariantColumn::new("tree_ui_state", "visibility_mode", "tree visibility mode"),
    InvariantColumn::new("workspace_audit_events", "id", "Workspace audit Event id"),
    InvariantColumn::new("workspace_audit_events", "workspace_id", "Workspace id"),
    InvariantColumn::new(
        "workspace_audit_events",
        "event_name",
        "Workspace audit event kind",
    ),
    InvariantColumn::new("workspace_audit_events", "session_id", "Session id"),
    InvariantColumn::new(
        "workspace_audit_events",
        "confidence",
        "Workspace audit confidence",
    ),
];

/// Runs the pending v5/v9 maintenance. A marker is only removed after the
/// logical transaction and the physical rebuild have both completed.
pub(crate) fn run(conn: &Connection, physical: bool) -> Result<()> {
    if !purge_pending(conn)? {
        return Ok(());
    }

    // Makes subsequent UPDATE/DELETE operations overwrite freed cells where the
    // current SQLite backend supports it. VACUUM remains necessary for bytes
    // already stranded by an older process.
    conn.execute_batch("PRAGMA secure_delete = ON;")?;
    redact_rows_transactionally(conn)?;

    if physical {
        checkpoint_truncate(conn)?;
        conn.execute_batch("VACUUM;")?;
        checkpoint_truncate(conn)?;
    }

    conn.execute(
        "DELETE FROM settings WHERE key IN (?1, ?2)",
        params![LEGACY_HOOK_PURGE_MARKER, LEGACY_FREE_TEXT_PURGE_MARKER],
    )?;
    Ok(())
}

fn purge_pending(conn: &Connection) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM settings WHERE key IN (?1, ?2))",
        params![LEGACY_HOOK_PURGE_MARKER, LEGACY_FREE_TEXT_PURGE_MARKER],
        |row| row.get(0),
    )?)
}

/// Returns an error rather than accepting a partial TRUNCATE checkpoint. SQLite
/// reports a busy reader as a result row, not necessarily as `SQLITE_BUSY`.
fn checkpoint_truncate(conn: &Connection) -> Result<()> {
    let (busy, log_frames, checkpointed_frames): (i64, i64, i64) =
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
    if busy != 0 {
        return Err(StoreError::SecurityMaintenanceIncomplete {
            reason: format!(
                "a SQLite reader kept the WAL busy ({checkpointed_frames}/{log_frames} frames checkpointed)"
            ),
        });
    }
    Ok(())
}

fn redact_rows_transactionally(conn: &Connection) -> Result<()> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    validate_schema_coverage(&tx)?;
    validate_invariants(&tx)?;
    for spec in REDACTABLE_COLUMNS {
        redact_column(&tx, *spec)?;
    }
    tx.commit()?;
    Ok(())
}

fn validate_invariants(tx: &Transaction<'_>) -> Result<()> {
    for spec in INVARIANT_COLUMNS {
        for (rowid, value) in text_rows(tx, spec.table, spec.column)? {
            if redact_secrets(&value) != value {
                return Err(StoreError::SecretInStructuralField {
                    what: spec.description,
                    owner_id: format!("{}.{} row {rowid}", spec.table, spec.column),
                });
            }
        }
    }
    Ok(())
}

fn redact_column(tx: &Transaction<'_>, spec: RedactableColumn) -> Result<()> {
    let rows = text_rows(tx, spec.table, spec.column)?;
    let mut updates = Vec::new();
    let mut attention_keys = BTreeMap::new();
    for (rowid, raw) in rows {
        let safe = safe_value(spec.kind, &raw)?;
        if spec.kind == RedactionKind::AttentionDedup {
            if let Some(previous) = attention_keys.insert(safe.clone(), rowid) {
                if previous != rowid {
                    return Err(StoreError::SecurityMaintenanceIncomplete {
                        reason: format!(
                            "redacting attention correlation keys would alias rows {previous} and {rowid}"
                        ),
                    });
                }
            }
        }
        if safe != raw {
            updates.push((rowid, safe));
        }
    }

    if updates.is_empty() {
        return Ok(());
    }

    let table = quote_identifier(spec.table);
    let column = quote_identifier(spec.column);
    let sql = if spec.kind == RedactionKind::PreviewText {
        format!(
            "UPDATE {table} SET {column} = ?1, contains_sensitive_data = 1, redacted = 1 WHERE rowid = ?2"
        )
    } else {
        format!("UPDATE {table} SET {column} = ?1 WHERE rowid = ?2")
    };
    let mut statement = tx.prepare(&sql)?;
    for (rowid, safe) in updates {
        statement.execute(params![safe, rowid])?;
    }
    Ok(())
}

fn safe_value(kind: RedactionKind, raw: &str) -> Result<String> {
    match kind {
        RedactionKind::Scalar | RedactionKind::PreviewText | RedactionKind::AttentionDedup => {
            Ok(redact_secrets(raw))
        }
        RedactionKind::Json => Ok(redact_json_preserving_representation(raw)),
        RedactionKind::Environment => redact_environment(raw),
        RedactionKind::Layout => redact_stored_layout(raw),
        RedactionKind::AgentInfo => redact_agent_info(raw),
        RedactionKind::ActivityPreview => redact_activity_preview(raw),
    }
}

fn redact_json_preserving_representation(raw: &str) -> String {
    let safe = redact_json(raw);
    match (
        serde_json::from_str::<serde_json::Value>(raw),
        serde_json::from_str::<serde_json::Value>(&safe),
    ) {
        (Ok(before), Ok(after)) if before == after => raw.to_string(),
        _ => safe,
    }
}

fn redact_environment(raw: &str) -> Result<String> {
    let Ok(environment) = serde_json::from_str::<Vec<(String, String)>>(raw) else {
        return Ok(redact_json_preserving_representation(raw));
    };
    let safe = redact_pairs(&environment);
    if safe == environment {
        return Ok(raw.to_string());
    }
    serde_json::to_string(&safe).map_err(|cause| StoreError::encode("legacy environment", cause))
}

fn redact_stored_layout(raw: &str) -> Result<String> {
    let Ok(layout) = serde_json::from_str::<Layout>(raw) else {
        return Ok(redact_json_preserving_representation(raw));
    };
    let safe = redact_layout(&layout);
    if safe == layout {
        return Ok(raw.to_string());
    }
    serde_json::to_string(&safe).map_err(|cause| StoreError::encode("legacy layout", cause))
}

fn redact_agent_info(raw: &str) -> Result<String> {
    let Ok(agent) = serde_json::from_str::<AgentInfo>(raw) else {
        return Ok(redact_json_preserving_representation(raw));
    };
    let safe = agent_info_for_persistence(&agent);
    if safe == agent {
        return Ok(raw.to_string());
    }
    serde_json::to_string(&safe).map_err(|cause| StoreError::encode("legacy agent info", cause))
}

fn redact_activity_preview(raw: &str) -> Result<String> {
    let Ok(preview) = serde_json::from_str::<ActivityPreview>(raw) else {
        return Ok(redact_json_preserving_representation(raw));
    };
    let safe = activity_preview_for_persistence(&preview);
    if safe == preview {
        return Ok(raw.to_string());
    }
    serde_json::to_string(&safe)
        .map_err(|cause| StoreError::encode("legacy activity preview", cause))
}

fn text_rows(conn: &Connection, table: &str, column: &str) -> Result<Vec<(i64, String)>> {
    let sql = format!(
        "SELECT rowid, {} FROM {} WHERE {} IS NOT NULL",
        quote_identifier(column),
        quote_identifier(table),
        quote_identifier(column),
    );
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

fn quote_identifier(identifier: &str) -> String {
    debug_assert!(
        identifier
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'),
        "maintenance identifiers are compile-time schema names"
    );
    format!("\"{identifier}\"")
}

fn validate_schema_coverage(conn: &Connection) -> Result<()> {
    let expected: BTreeSet<(String, String)> = REDACTABLE_COLUMNS
        .iter()
        .map(|spec| (spec.table.to_string(), spec.column.to_string()))
        .chain(
            INVARIANT_COLUMNS
                .iter()
                .map(|spec| (spec.table.to_string(), spec.column.to_string())),
        )
        .collect();
    let actual = schema_text_columns(conn)?;
    if expected != actual {
        let missing = actual.difference(&expected).cloned().collect::<Vec<_>>();
        let stale = expected.difference(&actual).cloned().collect::<Vec<_>>();
        return Err(StoreError::SecurityMaintenanceIncomplete {
            reason: format!(
                "TEXT-column classification drift (unclassified: {missing:?}; absent: {stale:?})"
            ),
        });
    }
    Ok(())
}

fn schema_text_columns(conn: &Connection) -> Result<BTreeSet<(String, String)>> {
    let tables = {
        let mut statement = conn.prepare(
            "SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    let mut columns = BTreeSet::new();
    for table in tables {
        let mut statement = conn.prepare(
            "SELECT name FROM pragma_table_info(?1) WHERE upper(type) = 'TEXT' ORDER BY cid",
        )?;
        let rows = statement.query_map(params![&table], |row| row.get::<_, String>(0))?;
        for column in rows {
            columns.insert((table.clone(), column?));
        }
    }
    Ok(columns)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrations;

    const SECRET: &str = "ghp_0123456789abcdefghijklmnopqrstuvwxyz";

    fn legacy_connection() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        migrations::apply_to(&conn, 8).unwrap();
        conn
    }

    #[test]
    fn every_text_column_is_explicitly_classified() {
        let conn = legacy_connection();
        migrations::apply(&conn).unwrap();
        validate_schema_coverage(&conn).unwrap();
    }

    #[test]
    fn a_failed_logical_pass_rolls_back_every_change_and_keeps_the_marker() {
        let conn = legacy_connection();
        conn.execute(
            "INSERT INTO workspaces (id, name, root, env_json, init_commands_json, \
             attention_json, created_ms, last_used_ms, tmux_enabled, archived) \
             VALUES ('ws_old', ?1, '/repo', '[]', '[]', '{}', 1, 1, 0, 0)",
            [format!("legacy {SECRET}")],
        )
        .unwrap();
        migrations::apply(&conn).unwrap();
        conn.execute_batch(
            "CREATE TRIGGER fail_legacy_scrub BEFORE UPDATE OF name ON workspaces \
             BEGIN SELECT RAISE(ABORT, 'disk full stand-in'); END;",
        )
        .unwrap();

        assert!(redact_rows_transactionally(&conn).is_err());
        let name: String = conn
            .query_row(
                "SELECT name FROM workspaces WHERE id = 'ws_old'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(name.contains(SECRET));
        assert!(purge_pending(&conn).unwrap());

        conn.execute_batch("DROP TRIGGER fail_legacy_scrub;")
            .unwrap();
        run(&conn, false).unwrap();
        let name: String = conn
            .query_row(
                "SELECT name FROM workspaces WHERE id = 'ws_old'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(name, "legacy [redacted]");
        assert!(!purge_pending(&conn).unwrap());
    }

    #[test]
    fn a_structural_identity_is_never_rewritten() {
        let conn = legacy_connection();
        let root = format!("/repo/{SECRET}");
        conn.execute(
            "INSERT INTO workspaces (id, name, root, env_json, init_commands_json, \
             attention_json, created_ms, last_used_ms, tmux_enabled, archived) \
             VALUES ('ws_old', 'legacy', ?1, '[]', '[]', '{}', 1, 1, 0, 0)",
            [&root],
        )
        .unwrap();
        migrations::apply(&conn).unwrap();

        let error = redact_rows_transactionally(&conn).unwrap_err();
        assert!(matches!(
            error,
            StoreError::SecretInStructuralField {
                what: "workspace root",
                ..
            }
        ));
        let stored: String = conn
            .query_row(
                "SELECT root FROM workspaces WHERE id = 'ws_old'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, root);
        assert!(purge_pending(&conn).unwrap());
    }

    #[test]
    fn attention_keys_that_would_alias_fail_without_deleting_either_identity() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        migrations::apply_to(&conn, 1).unwrap();
        conn.execute(
            "INSERT INTO workspaces (id, name, root, env_json, init_commands_json, \
             attention_json, created_ms, last_used_ms, tmux_enabled, archived) \
             VALUES ('ws_old', 'legacy', '/repo', '[]', '[]', '{}', 1, 1, 0, 0)",
            [],
        )
        .unwrap();
        migrations::apply_to(&conn, 8).unwrap();
        conn.execute(
            "INSERT INTO sessions (id, workspace_id, name, cwd, env_json, attention_json, \
             status, restore_state, tags_json, favourite, pinned, sort_key, created_ms, \
             last_activity_ms, tmux, mode, checkout_id, read_only_enforced) VALUES \
             ('sess_old', 'ws_old', 'legacy', '/repo', '[]', '{}', 'active', 'live', '[]', \
              0, 0, 0, 1, 1, 0, 'read_only', 'checkout_primary_ws_old', 0)",
            [],
        )
        .unwrap();
        let first = "ghp_0123456789abcdefghijklmnopqrstuv";
        let second = "ghp_vwxyz0123456789abcdefghijklmnopq";
        assert_ne!(first, second);
        assert_eq!(redact_secrets(first), "[redacted]");
        assert_eq!(redact_secrets(second), "[redacted]");
        for (id, secret) in [("att_first", first), ("att_second", second)] {
            conn.execute(
                "INSERT INTO attention_entries (id, session_id, reason, confidence, \
                 created_ms, updated_ms, state_json, priority_boost, dedup_key, \
                 survives_owner_exit, demand_kind) VALUES (?1, 'sess_old', 'question', \
                 'explicit', 1, 1, '{\"pending\":{}}', 0, ?2, 0, 'interaction')",
                params![id, format!("sess_old|question|{secret}")],
            )
            .unwrap();
        }
        migrations::apply(&conn).unwrap();

        let error = redact_rows_transactionally(&conn).unwrap_err();
        assert!(matches!(
            error,
            StoreError::SecurityMaintenanceIncomplete { reason }
                if reason.contains("would alias")
        ));
        let ids: Vec<String> = {
            let mut statement = conn
                .prepare("SELECT id FROM attention_entries ORDER BY id")
                .unwrap();
            statement
                .query_map([], |row| row.get(0))
                .unwrap()
                .collect::<std::result::Result<Vec<_>, _>>()
                .unwrap()
        };
        assert_eq!(ids, vec!["att_first", "att_second"]);
        assert!(purge_pending(&conn).unwrap());
    }
}
