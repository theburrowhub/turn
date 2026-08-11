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
    activity_preview_for_persistence, agent_info_for_persistence, agent_ref_for_persistence,
    redact_json, redact_layout, redact_pairs, redact_secrets,
};
use rusqlite::{params, Connection, Transaction, TransactionBehavior};
use std::collections::{BTreeMap, BTreeSet};
use turn_core::attention::AttentionQueue;
use turn_core::event::AgentRef;
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
    AgentRef,
    ActivityPreview,
    PreviewText,
    AttentionDedup,
    NullableOperationalId,
    SettingsKey,
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
    RedactableColumn::new(
        "process_nodes",
        "external_id",
        RedactionKind::NullableOperationalId,
    ),
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
    RedactableColumn::new("events", "agent_json", RedactionKind::AgentRef),
    RedactableColumn::new("events", "source_json", RedactionKind::Json),
    RedactableColumn::new("events", "dedup_key", RedactionKind::Scalar),
    RedactableColumn::new("events", "raw", RedactionKind::Json),
    RedactableColumn::new("settings", "value_json", RedactionKind::Json),
    RedactableColumn::new("settings", "key", RedactionKind::SettingsKey),
    // The layered preferences, classified exactly like the flat table they sit beside. A
    // preference's value is user text and may be an environment variable, so it is scrubbed
    // on the same terms as `workspaces.env_json`: Turn does not hold a credential in the
    // clear on disk, and a settings sheet is not a way around that.
    RedactableColumn::new("setting_layers", "value_json", RedactionKind::Json),
    RedactableColumn::new("setting_layers", "key", RedactionKind::SettingsKey),
    RedactableColumn::new(
        "attention_entries",
        "subject_external_id",
        RedactionKind::NullableOperationalId,
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
    InvariantColumn::new("setting_layers", "scope", "settings level"),
    InvariantColumn::new("setting_layers", "owner_id", "settings level owner id"),
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
    InvariantColumn::new("tree_surface_preferences", "surface_id", "surface id"),
    InvariantColumn::new(
        "tree_surface_preferences",
        "filters_json",
        "tree filter vocabulary",
    ),
    InvariantColumn::new(
        "tree_surface_preferences",
        "visibility_mode",
        "tree visibility mode",
    ),
    InvariantColumn::new(
        "tree_surface_preferences",
        "scroll_node_kind",
        "tree scroll node kind",
    ),
    InvariantColumn::new(
        "tree_surface_preferences",
        "scroll_node_id",
        "tree scroll node id",
    ),
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

    run_pending(conn, physical).map_err(as_maintenance_failure)
}

fn run_pending(conn: &Connection, physical: bool) -> Result<()> {
    if physical {
        require_wal(conn)?;
        set_locking_mode(conn, "EXCLUSIVE")?;
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
    if physical {
        checkpoint_truncate(conn)?;
        set_locking_mode(conn, "NORMAL")?;
    }
    Ok(())
}

fn as_maintenance_failure(error: StoreError) -> StoreError {
    match error {
        StoreError::Sqlite(ref sqlite) if sqlite_is_busy(sqlite) => {
            StoreError::SecurityMaintenanceIncomplete {
                reason: format!("SQLite could not obtain the cleanup lock: {sqlite}"),
            }
        }
        other => other,
    }
}

fn sqlite_is_busy(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(code, _)
            if matches!(
                code.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
            )
    )
}

fn require_wal(conn: &Connection) -> Result<()> {
    let mode: String = conn.query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(StoreError::SecurityMaintenanceIncomplete {
            reason: format!(
                "physical credential erasure requires WAL mode; SQLite reported {mode}"
            ),
        });
    }
    Ok(())
}

/// Retains SQLite's own exclusive lock across the scrub transaction, checkpoints,
/// VACUUM and marker deletion. The daemon lock excludes current Turn processes;
/// this closes the gap for an older/cooperating SQLite writer that does not know
/// that lock file yet.
fn set_locking_mode(conn: &Connection, requested: &str) -> Result<()> {
    let sql = format!("PRAGMA locking_mode = {requested}");
    let actual: String = conn.query_row(&sql, [], |row| row.get(0))?;
    if !actual.eq_ignore_ascii_case(requested) {
        return Err(StoreError::SecurityMaintenanceIncomplete {
            reason: format!(
                "SQLite refused {requested} locking mode during credential cleanup (reported {actual})"
            ),
        });
    }
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

fn redact_rows_transactionally(conn: &Connection) -> Result<bool> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Exclusive)?;
    validate_schema_coverage(&tx)?;
    validate_invariants(&tx)?;
    let mut changed = false;
    for spec in REDACTABLE_COLUMNS {
        changed |= redact_column(&tx, *spec)?;
    }
    changed |= reconcile_attention_entries(&tx)?;
    tx.commit()?;
    Ok(changed)
}

/// Re-derives durable attention correlation after an unsafe legacy external id
/// has been removed.
///
/// Redacting `subject_external_id` to NULL is intentionally lossy: replacing it
/// with a shared marker would alias unrelated workers. The separately stored
/// `dedup_key` must then be rebuilt from the surviving authenticated
/// session/parent/node scope. We fail the whole transaction if two entries would
/// collapse, preserving both rows for explicit reconciliation instead of
/// silently deleting one demand.
fn reconcile_attention_entries(tx: &Transaction<'_>) -> Result<bool> {
    let repo = crate::repo::AttentionRepo::new(tx);
    let entries = repo.list()?;
    if entries.is_empty() {
        return Ok(false);
    }

    let mut safe_queue = AttentionQueue::new();
    let mut derived_keys = BTreeMap::<String, String>::new();
    let mut changed = false;
    for mut entry in entries {
        let original_external = entry.subject_external_id.clone();
        entry.subject_external_id = entry
            .subject_external_id
            .filter(|external| redact_secrets(external) == *external);
        entry.summary = entry.summary.as_deref().map(redact_secrets);
        changed |= entry.subject_external_id != original_external;

        let derived = entry.dedup_key();
        let stored: String = tx.query_row(
            "SELECT dedup_key FROM attention_entries WHERE id = ?1",
            params![entry.id.as_str()],
            |row| row.get(0),
        )?;
        changed |= stored != derived;
        if let Some(previous) = derived_keys.insert(derived.clone(), entry.id.to_string()) {
            return Err(StoreError::SecurityMaintenanceIncomplete {
                reason: format!(
                    "removing unsafe attention identities would alias entries {previous} and {}",
                    entry.id
                ),
            });
        }
        safe_queue.upsert(entry);
    }

    if changed {
        crate::repo::attention::replace_all_in(tx, &safe_queue)?;
    }
    Ok(changed)
}

fn validate_invariants(tx: &Transaction<'_>) -> Result<()> {
    for spec in INVARIANT_COLUMNS {
        let mut after = i64::MIN;
        loop {
            let rows = text_rows_after(tx, spec.table, spec.column, after)?;
            if rows.is_empty() {
                break;
            }
            after = rows.last().unwrap().0;
            for (rowid, value) in rows {
                if redact_secrets(&value) != value {
                    return Err(StoreError::SecretInStructuralField {
                        what: spec.description,
                        owner_id: format!("{}.{} row {rowid}", spec.table, spec.column),
                    });
                }
            }
        }
    }
    Ok(())
}

fn redact_column(tx: &Transaction<'_>, spec: RedactableColumn) -> Result<bool> {
    let mut attention_keys = BTreeMap::new();
    let table = quote_identifier(spec.table);
    let column = quote_identifier(spec.column);
    let sql = match spec.kind {
        RedactionKind::PreviewText => format!(
            "UPDATE {table} SET {column} = ?1, contains_sensitive_data = 1, redacted = 1 WHERE rowid = ?2"
        ),
        RedactionKind::NullableOperationalId => {
            format!("UPDATE {table} SET {column} = NULL WHERE rowid = ?1")
        }
        RedactionKind::SettingsKey => format!("DELETE FROM {table} WHERE rowid = ?1"),
        _ => format!("UPDATE {table} SET {column} = ?1 WHERE rowid = ?2"),
    };
    let mut statement = tx.prepare(&sql)?;
    let mut changed = false;
    let mut after = i64::MIN;
    loop {
        let rows = text_rows_after(tx, spec.table, spec.column, after)?;
        if rows.is_empty() {
            break;
        }
        after = rows.last().unwrap().0;
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
                changed = true;
                if matches!(
                    spec.kind,
                    RedactionKind::NullableOperationalId | RedactionKind::SettingsKey
                ) {
                    statement.execute(params![rowid])?;
                } else {
                    statement.execute(params![safe, rowid])?;
                }
            }
        }
    }
    Ok(changed)
}

fn safe_value(kind: RedactionKind, raw: &str) -> Result<String> {
    match kind {
        RedactionKind::Scalar
        | RedactionKind::PreviewText
        | RedactionKind::AttentionDedup
        | RedactionKind::NullableOperationalId
        | RedactionKind::SettingsKey => Ok(redact_secrets(raw)),
        RedactionKind::Json => Ok(redact_json_preserving_representation(raw)),
        RedactionKind::Environment => redact_environment(raw),
        RedactionKind::Layout => redact_stored_layout(raw),
        RedactionKind::AgentInfo => redact_agent_info(raw),
        RedactionKind::AgentRef => redact_agent_ref(raw),
        RedactionKind::ActivityPreview => redact_activity_preview(raw),
    }
}

fn redact_json_preserving_representation(raw: &str) -> String {
    // Always return the scanned/canonical representation. Semantic equality is
    // not byte safety: JSON permits duplicate member names, while serde keeps
    // only the last. `{"token":"secret","token":"[redacted]"}` would compare
    // equal before/after and preserve the hidden first member verbatim.
    redact_json(raw)
}

fn redact_environment(raw: &str) -> Result<String> {
    let generic = redact_json_preserving_representation(raw);
    let Ok(environment) = serde_json::from_str::<Vec<(String, String)>>(raw) else {
        return Ok(generic);
    };
    let safe = redact_pairs(&environment);
    merge_typed_projection(&generic, &safe, "legacy environment")
}

fn redact_stored_layout(raw: &str) -> Result<String> {
    let generic = redact_json_preserving_representation(raw);
    let Ok(layout) = serde_json::from_str::<Layout>(raw) else {
        return Ok(generic);
    };
    let safe = redact_layout(&layout);
    merge_typed_projection(&generic, &safe, "legacy layout")
}

fn redact_agent_info(raw: &str) -> Result<String> {
    let generic = redact_json_preserving_representation(raw);
    let Ok(agent) = serde_json::from_str::<AgentInfo>(raw) else {
        return Ok(generic);
    };
    let safe = agent_info_for_persistence(&agent);
    merge_typed_projection(&generic, &safe, "legacy agent info")
}

fn redact_agent_ref(raw: &str) -> Result<String> {
    let generic = redact_json_preserving_representation(raw);
    let Ok(agent) = serde_json::from_str::<AgentRef>(raw) else {
        return Ok(generic);
    };
    let safe = agent_ref_for_persistence(&agent);
    merge_typed_projection(&generic, &safe, "legacy agent reference")
}

fn redact_activity_preview(raw: &str) -> Result<String> {
    let generic = redact_json_preserving_representation(raw);
    let Ok(preview) = serde_json::from_str::<ActivityPreview>(raw) else {
        return Ok(generic);
    };
    let safe = activity_preview_for_persistence(&preview);
    merge_typed_projection(&generic, &safe, "legacy activity preview")
}

/// Applies the known typed redaction without throwing away fields written by a
/// newer/older adapter. The generic pass has already scrubbed every unknown key
/// and value; recursive overlay restores the correctly typed known fields (for
/// example `tokens_used`) and applies specialised env/preview rules.
fn merge_typed_projection<T: serde::Serialize>(
    generic: &str,
    safe: &T,
    what: &'static str,
) -> Result<String> {
    let mut target = serde_json::from_str::<serde_json::Value>(generic)
        .map_err(|cause| StoreError::encode(what, cause))?;
    let known = serde_json::to_value(safe).map_err(|cause| StoreError::encode(what, cause))?;
    overlay_known(&mut target, &known);
    serde_json::to_string(&target).map_err(|cause| StoreError::encode(what, cause))
}

fn overlay_known(target: &mut serde_json::Value, known: &serde_json::Value) {
    match (target, known) {
        (serde_json::Value::Object(target), serde_json::Value::Object(known)) => {
            for (key, known_value) in known {
                match target.get_mut(key) {
                    Some(target_value) => overlay_known(target_value, known_value),
                    None => {
                        target.insert(key.clone(), known_value.clone());
                    }
                }
            }
        }
        (serde_json::Value::Array(target), serde_json::Value::Array(known)) => {
            for (index, known_value) in known.iter().enumerate() {
                match target.get_mut(index) {
                    Some(target_value) => overlay_known(target_value, known_value),
                    None => target.push(known_value.clone()),
                }
            }
        }
        (target, known) => *target = known.clone(),
    }
}

const MAINTENANCE_BATCH_ROWS: i64 = 256;

fn text_rows_after(
    conn: &Connection,
    table: &str,
    column: &str,
    after: i64,
) -> Result<Vec<(i64, String)>> {
    let sql = format!(
        "SELECT rowid, {} FROM {} WHERE {} IS NOT NULL AND rowid > ?1 \
         ORDER BY rowid LIMIT ?2",
        quote_identifier(column),
        quote_identifier(table),
        quote_identifier(column),
    );
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map(params![after, MAINTENANCE_BATCH_ROWS], |row| {
        Ok((row.get(0)?, row.get(1)?))
    })?;
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
    fn duplicate_json_keys_cannot_hide_a_secret_from_the_legacy_scrub() {
        let raw = format!(r#"{{"token":"{SECRET}","token":"[redacted]"}}"#);
        let safe = safe_value(RedactionKind::Json, &raw).unwrap();
        assert!(!safe.contains(SECRET), "hidden duplicate survived: {safe}");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&safe).unwrap()["token"],
            "[redacted]"
        );
    }

    #[test]
    fn typed_documents_keep_unknown_fields_but_never_their_credentials() {
        let node = turn_core::ids::NodeId::from_stored("proc_legacy_typed");
        let documents = [
            (
                RedactionKind::Layout,
                serde_json::to_value(Layout::single(turn_core::model::Pane::new(
                    turn_core::model::PaneKind::Agent,
                )))
                .unwrap(),
            ),
            (
                RedactionKind::AgentInfo,
                serde_json::to_value(AgentInfo::default()).unwrap(),
            ),
            (
                RedactionKind::ActivityPreview,
                serde_json::to_value(ActivityPreview {
                    node_id: node,
                    raw_source_sequence: None,
                    normalized_text: "working".into(),
                    source: turn_core::model::PreviewSource::AdapterState,
                    confidence: turn_core::Confidence::Integrated,
                    stable: true,
                    contains_sensitive_data: false,
                    redacted: false,
                    updated_ms: 1,
                })
                .unwrap(),
            ),
        ];
        for (kind, mut value) in documents {
            value.as_object_mut().unwrap().insert(
                "plugin_secret".into(),
                serde_json::Value::String(SECRET.into()),
            );
            let safe = safe_value(kind, &value.to_string()).unwrap();
            assert!(!safe.contains(SECRET), "{kind:?} leaked: {safe}");
            let reparsed: serde_json::Value = serde_json::from_str(&safe).unwrap();
            assert_eq!(
                reparsed["plugin_secret"], "[redacted]",
                "unknown metadata was dropped instead of retained"
            );
        }
    }

    #[test]
    fn a_legacy_credential_used_as_a_setting_name_is_deleted_without_aliasing_preferences() {
        let conn = legacy_connection();
        conn.execute(
            "INSERT INTO settings (key, value_json, updated_ms) VALUES (?1, '\"value\"', 1)",
            [SECRET],
        )
        .unwrap();
        migrations::apply(&conn).unwrap();

        redact_rows_transactionally(&conn).unwrap();
        let present: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM settings WHERE key = ?1)",
                [SECRET],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!present, "an unsafe preference identity cannot be retained");
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

    #[test]
    fn exclusive_locking_survives_the_scrub_commit_until_physical_cleanup_finishes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("maintenance-lock.db");
        let owner = Connection::open(&path).unwrap();
        owner
            .execute_batch("PRAGMA journal_mode = WAL; CREATE TABLE proof (value TEXT NOT NULL);")
            .unwrap();
        let contender = Connection::open(&path).unwrap();
        contender.busy_timeout(std::time::Duration::ZERO).unwrap();

        set_locking_mode(&owner, "EXCLUSIVE").unwrap();
        let transaction =
            Transaction::new_unchecked(&owner, TransactionBehavior::Exclusive).unwrap();
        transaction
            .execute("INSERT INTO proof (value) VALUES ('scrubbed')", [])
            .unwrap();
        transaction.commit().unwrap();

        let error = contender
            .execute("INSERT INTO proof (value) VALUES ('racing writer')", [])
            .expect_err("the physical phase must retain ownership after COMMIT");
        assert!(
            sqlite_is_busy(&error),
            "the competing writer failed for an unexpected reason: {error:?}"
        );
    }
}
