//! Complete, redacted inventory of SQLite persistence.
//!
//! The table catalogue is intentionally closed. Adding a migration table without
//! adding it here makes export fail loudly instead of producing a reassuring but
//! incomplete privacy report.

use crate::error::{Result, StoreError};
use crate::redact::redact_json;
use crate::{Retention, Store};
use rusqlite::types::ValueRef;
use rusqlite::{params, params_from_iter};
use serde_json::{Map, Value};
use std::collections::BTreeSet;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use turn_core::privacy::{
    PrivacyDatum, PrivacyExportDocument, PrivacyPolicy, PrivacyScope, PRIVACY_POLICY_KEYS,
};
use turn_core::settings::{Catalogue, Sensitivity};

#[derive(Debug, Clone, Copy)]
struct TableSpec {
    name: &'static str,
    timestamp: Option<&'static str>,
}

const TABLES: &[TableSpec] = &[
    TableSpec {
        name: "attention_entries",
        timestamp: Some("created_ms"),
    },
    TableSpec {
        name: "activity_previews",
        timestamp: Some("created_ms"),
    },
    TableSpec {
        name: "checkout_write_fences",
        timestamp: None,
    },
    TableSpec {
        name: "events",
        timestamp: Some("timestamp_ms"),
    },
    TableSpec {
        name: "pane_node_bindings",
        timestamp: Some("opened_ms"),
    },
    TableSpec {
        name: "process_nodes",
        timestamp: Some("started_ms"),
    },
    TableSpec {
        name: "session_layouts",
        timestamp: Some("updated_ms"),
    },
    TableSpec {
        name: "sessions",
        timestamp: Some("created_ms"),
    },
    TableSpec {
        name: "setting_layers",
        timestamp: Some("updated_ms"),
    },
    TableSpec {
        name: "settings",
        timestamp: Some("updated_ms"),
    },
    TableSpec {
        name: "templates",
        timestamp: Some("created_ms"),
    },
    TableSpec {
        name: "tree_surface_preferences",
        timestamp: Some("updated_ms"),
    },
    TableSpec {
        name: "tree_ui_state",
        timestamp: Some("updated_ms"),
    },
    TableSpec {
        name: "workspace_audit_events",
        timestamp: Some("timestamp_ms"),
    },
    TableSpec {
        name: "workspace_checkouts",
        timestamp: Some("created_ms"),
    },
    TableSpec {
        name: "workspace_write_leases",
        timestamp: Some("acquired_ms"),
    },
    TableSpec {
        name: "workspaces",
        timestamp: Some("created_ms"),
    },
];

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PrivacyPruneOutcome {
    pub events: usize,
    pub previews: usize,
}

impl Store {
    /// Every SQLite row owned by `scope`, with JSON fields expanded and every
    /// textual value run through the durable secret scrubber again.
    pub fn privacy_rows(&self, scope: &PrivacyScope) -> Result<Vec<PrivacyDatum>> {
        self.ensure_privacy_catalogue_complete()?;
        let mut data = Vec::new();
        for table in TABLES {
            let Some((predicate, argument)) = filter_for(table.name, scope) else {
                continue;
            };
            let sql = format!(
                "SELECT rowid AS __turn_rowid, * FROM \"{}\" WHERE {predicate} ORDER BY rowid",
                table.name
            );
            let mut statement = self.conn.prepare(&sql)?;
            let columns: Vec<String> = statement
                .column_names()
                .iter()
                .map(|name| (*name).to_string())
                .collect();
            let arguments: Vec<&str> = argument.iter().map(String::as_str).collect();
            let mut rows = statement.query(params_from_iter(arguments))?;
            while let Some(row) = rows.next()? {
                let rowid: i64 = row.get(0)?;
                let mut content = Map::new();
                for (index, column) in columns.iter().enumerate().skip(1) {
                    content.insert(column.clone(), export_value(row.get_ref(index)?));
                }
                redact_setting_value(table.name, &mut content);
                let timestamp_ms = table
                    .timestamp
                    .and_then(|column| content.get(column))
                    .and_then(Value::as_i64);
                let content = Value::Object(content);
                let bytes = serde_json::to_vec(&content)
                    .map_err(|cause| StoreError::encode("privacy datum", cause))?
                    .len() as u64;
                data.push(PrivacyDatum {
                    origin: format!("sqlite/{}/{}", table.name, rowid),
                    data_type: table.name.to_string(),
                    timestamp_ms,
                    bytes,
                    content,
                });
            }
        }
        Ok(data)
    }

    /// Global retention settings as stored by the existing settings hierarchy.
    pub fn global_privacy_policy(&self) -> Result<PrivacyPolicy> {
        global_privacy_policy_in(&self.conn)
    }

    /// Applies event age/count and preview age/per-Agent/global retention now.
    pub fn prune_privacy(
        &self,
        policy: &PrivacyPolicy,
        now_ms: i64,
    ) -> Result<PrivacyPruneOutcome> {
        const DAY_MS: i64 = 24 * 60 * 60 * 1_000;
        let retention = Retention::unlimited()
            .with_max_age_ms(i64::from(policy.event_max_age_days).saturating_mul(DAY_MS))
            .with_max_events(policy.event_max_records as usize)
            .keeping_per_session(policy.event_keep_per_session as usize);
        let events = self.events().prune(&retention, now_ms)?.total();

        let tx = self.conn.unchecked_transaction()?;
        let previews = prune_previews_in(&tx, policy, now_ms)?;
        tx.commit()?;
        Ok(PrivacyPruneOutcome { events, previews })
    }

    /// Removes every durable row attributable to one Agent identity. The daemon
    /// has already removed the runtime and its in-memory tree node.
    pub fn delete_agent_records(
        &self,
        session: &turn_core::ids::SessionId,
        node: &turn_core::ids::NodeId,
    ) -> Result<bool> {
        let belongs: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM process_nodes WHERE id = ?1 AND session_id = ?2)",
            params![node.as_str(), session.as_str()],
            |row| row.get(0),
        )?;
        if !belongs {
            return Ok(false);
        }
        let unknown = crate::codec::tag("relation", &turn_core::model::Relation::Unknown)?;
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM events WHERE session_id = ?1 AND (node_id = ?2 OR parent_node_id = ?2)",
            params![session.as_str(), node.as_str()],
        )?;
        tx.execute(
            "DELETE FROM attention_entries WHERE session_id = ?1 AND \
             (node_id = ?2 OR parent_node_id = ?2)",
            params![session.as_str(), node.as_str()],
        )?;
        tx.execute(
            "DELETE FROM tree_ui_state WHERE node_id = ?1 AND \
             node_kind IN ('agent', 'process')",
            [node.as_str()],
        )?;
        tx.execute(
            "UPDATE tree_surface_preferences \
             SET scroll_node_kind = NULL, scroll_node_id = NULL \
             WHERE scroll_node_id = ?1 AND scroll_node_kind IN ('agent', 'process')",
            [node.as_str()],
        )?;
        tx.execute(
            "UPDATE process_nodes SET parent = NULL, relation = ?2 WHERE parent = ?1",
            params![node.as_str(), unknown],
        )?;
        tx.execute("DELETE FROM process_nodes WHERE id = ?1", [node.as_str()])?;
        tx.commit()?;
        Ok(true)
    }

    /// Clears all logical Turn records while keeping the open database usable
    /// until the daemon exits. The offline purge removes the physical files.
    pub fn delete_installation_records(&self) -> Result<u64> {
        let before = self.privacy_rows(&PrivacyScope::Installation)?.len() as u64;
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM workspaces", [])?;
        tx.execute("DELETE FROM templates", [])?;
        tx.execute("DELETE FROM settings", [])?;
        tx.execute("DELETE FROM setting_layers", [])?;
        tx.execute("DELETE FROM tree_ui_state", [])?;
        tx.execute("DELETE FROM tree_surface_preferences", [])?;
        tx.execute("DELETE FROM checkout_write_fences", [])?;
        tx.commit()?;
        Ok(before)
    }

    fn ensure_privacy_catalogue_complete(&self) -> Result<()> {
        let declared: BTreeSet<&str> = TABLES.iter().map(|table| table.name).collect();
        let mut statement = self.conn.prepare(
            "SELECT name FROM sqlite_master \
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )?;
        let actual: Vec<String> = statement
            .query_map([], |row| row.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        let missing: Vec<String> = actual
            .into_iter()
            .filter(|table| !declared.contains(table.as_str()))
            .collect();
        if missing.is_empty() {
            Ok(())
        } else {
            Err(StoreError::PrivacyInventoryIncomplete { tables: missing })
        }
    }
}

pub(crate) fn global_privacy_policy_in(conn: &rusqlite::Connection) -> Result<PrivacyPolicy> {
    let mut policy = PrivacyPolicy::default();
    for key in PRIVACY_POLICY_KEYS {
        let value: Option<String> = conn
            .query_row(
                "SELECT value_json FROM setting_layers \
                 WHERE scope = 'global' AND owner_id = '' AND key = ?1",
                [key],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(value) = value {
            let value =
                serde_json::from_str::<Value>(&value).map_err(|cause| StoreError::Decode {
                    what: "privacy setting",
                    id: key.to_string(),
                    cause,
                })?;
            policy.apply(key, &value);
        }
    }
    Ok(policy)
}

pub(crate) fn prune_previews_for_write(conn: &rusqlite::Connection, now_ms: i64) -> Result<usize> {
    let policy = global_privacy_policy_in(conn)?;
    prune_previews_in(conn, &policy, now_ms)
}

fn prune_previews_in(
    conn: &rusqlite::Connection,
    policy: &PrivacyPolicy,
    now_ms: i64,
) -> Result<usize> {
    const DAY_MS: i64 = 24 * 60 * 60 * 1_000;
    let cutoff =
        now_ms.saturating_sub(i64::from(policy.preview_max_age_days).saturating_mul(DAY_MS));
    let mut previews = conn.execute(
        "DELETE FROM activity_previews WHERE created_ms < ?1",
        [cutoff],
    )?;
    previews += conn.execute(
        "DELETE FROM activity_previews WHERE id IN ( \
             SELECT id FROM ( \
                 SELECT id, ROW_NUMBER() OVER ( \
                     PARTITION BY node_id ORDER BY created_ms DESC, id DESC \
                 ) AS in_node FROM activity_previews \
             ) WHERE in_node > ?1)",
        [i64::from(policy.preview_keep_per_agent)],
    )?;
    previews += conn.execute(
        "DELETE FROM activity_previews WHERE id IN ( \
             SELECT id FROM activity_previews \
             ORDER BY created_ms DESC, id DESC LIMIT -1 OFFSET ?1)",
        [i64::from(policy.preview_max_records)],
    )?;
    Ok(previews)
}

/// Writes a complete export without replacing any existing path.
pub fn write_export_create_new(path: &Path, document: &PrivacyExportDocument) -> Result<u64> {
    let mut encoded = serde_json::to_vec_pretty(document)
        .map_err(|cause| StoreError::encode("privacy export", cause))?;
    encoded.push(b'\n');
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let mut file = options
        .open(path)
        .map_err(|cause| StoreError::PrivacyExport {
            path: path.display().to_string(),
            cause,
        })?;
    let written = (|| {
        file.write_all(&encoded)?;
        file.sync_all()?;
        Ok::<_, std::io::Error>(encoded.len() as u64)
    })();
    if let Err(cause) = written {
        drop(file);
        let _ = std::fs::remove_file(path);
        return Err(StoreError::PrivacyExport {
            path: path.display().to_string(),
            cause,
        });
    }
    written.map_err(|cause| StoreError::PrivacyExport {
        path: path.display().to_string(),
        cause,
    })
}

fn export_value(value: ValueRef<'_>) -> Value {
    match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(value) => Value::from(value),
        ValueRef::Real(value) => serde_json::Number::from_f64(value)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        ValueRef::Text(bytes) => {
            let raw = String::from_utf8_lossy(bytes);
            let safe = redact_json(&raw);
            serde_json::from_str(&safe).unwrap_or(Value::String(safe))
        }
        ValueRef::Blob(bytes) => serde_json::json!({
            "binary_omitted": true,
            "bytes": bytes.len(),
        }),
    }
}

fn redact_setting_value(table: &str, content: &mut Map<String, Value>) {
    if !matches!(table, "settings" | "setting_layers") {
        return;
    }
    let Some(key) = content.get("key").and_then(Value::as_str) else {
        return;
    };
    let sensitivity = Catalogue::built_in()
        .get(key)
        .map(|definition| definition.sensitivity)
        .unwrap_or(Sensitivity::Unknown);
    if sensitivity != Sensitivity::Plain {
        content.insert(
            "value_json".to_string(),
            Value::String(crate::REDACTED.to_string()),
        );
    }
}

fn filter_for(table: &str, scope: &PrivacyScope) -> Option<(String, Vec<String>)> {
    let direct = |sql: &str, value: String| Some((sql.to_string(), vec![value]));
    match scope {
        PrivacyScope::Installation => Some(("1 = 1".to_string(), Vec::new())),
        PrivacyScope::Workspace { workspace_id } => match table {
            "workspaces" => direct("id = ?1", workspace_id.to_string()),
            "sessions"
            | "workspace_checkouts"
            | "workspace_write_leases"
            | "workspace_audit_events" => direct("workspace_id = ?1", workspace_id.to_string()),
            "session_layouts" | "process_nodes" | "events" | "attention_entries"
            | "pane_node_bindings" => direct(
                "session_id IN (SELECT id FROM sessions WHERE workspace_id = ?1)",
                workspace_id.to_string(),
            ),
            "activity_previews" => direct(
                "node_id IN (SELECT id FROM process_nodes WHERE session_id IN (\
                 SELECT id FROM sessions WHERE workspace_id = ?1))",
                workspace_id.to_string(),
            ),
            "tree_ui_state" => direct(
                "(node_kind = 'workspace' AND node_id = ?1) OR \
                 (node_kind = 'session' AND node_id IN (\
                    SELECT id FROM sessions WHERE workspace_id = ?1)) OR \
                 node_id IN (SELECT id FROM process_nodes WHERE session_id IN (\
                    SELECT id FROM sessions WHERE workspace_id = ?1))",
                workspace_id.to_string(),
            ),
            "tree_surface_preferences" => direct(
                "(scroll_node_kind = 'workspace' AND scroll_node_id = ?1) OR \
                 (scroll_node_kind = 'session' AND scroll_node_id IN (\
                    SELECT id FROM sessions WHERE workspace_id = ?1)) OR \
                 scroll_node_id IN (SELECT id FROM process_nodes WHERE session_id IN (\
                    SELECT id FROM sessions WHERE workspace_id = ?1))",
                workspace_id.to_string(),
            ),
            "setting_layers" => direct(
                "(scope = 'workspace' AND owner_id = ?1) OR \
                 (scope = 'session' AND owner_id IN (\
                    SELECT id FROM sessions WHERE workspace_id = ?1))",
                workspace_id.to_string(),
            ),
            _ => None,
        },
        PrivacyScope::Session { session_id } => match table {
            "sessions" => direct("id = ?1", session_id.to_string()),
            "session_layouts" | "process_nodes" | "events" | "attention_entries"
            | "pane_node_bindings" => direct("session_id = ?1", session_id.to_string()),
            "workspace_write_leases" => direct("session_id = ?1", session_id.to_string()),
            "activity_previews" => direct(
                "node_id IN (SELECT id FROM process_nodes WHERE session_id = ?1)",
                session_id.to_string(),
            ),
            "tree_ui_state" => direct(
                "(node_kind = 'session' AND node_id = ?1) OR \
                 node_id IN (SELECT id FROM process_nodes WHERE session_id = ?1)",
                session_id.to_string(),
            ),
            "tree_surface_preferences" => direct(
                "(scroll_node_kind = 'session' AND scroll_node_id = ?1) OR \
                 scroll_node_id IN (SELECT id FROM process_nodes WHERE session_id = ?1)",
                session_id.to_string(),
            ),
            "setting_layers" => direct(
                "scope = 'session' AND owner_id = ?1",
                session_id.to_string(),
            ),
            _ => None,
        },
        PrivacyScope::Agent {
            session_id,
            node_id,
        } => match table {
            "process_nodes" => direct(
                "id = ?1 AND session_id = ?2",
                // Two parameters are handled below because this is the sole filter
                // that must prove both identities.
                node_id.to_string(),
            )
            .map(|(sql, mut args)| {
                args.push(session_id.to_string());
                (sql, args)
            }),
            "events" | "attention_entries" => direct(
                "session_id = ?2 AND (node_id = ?1 OR parent_node_id = ?1)",
                node_id.to_string(),
            )
            .map(|(sql, mut args)| {
                args.push(session_id.to_string());
                (sql, args)
            }),
            "activity_previews" => direct("node_id = ?1", node_id.to_string()),
            "pane_node_bindings" => direct("node_id = ?1 AND session_id = ?2", node_id.to_string())
                .map(|(sql, mut args)| {
                    args.push(session_id.to_string());
                    (sql, args)
                }),
            "tree_ui_state" => direct("node_id = ?1", node_id.to_string()),
            "tree_surface_preferences" => direct(
                "scroll_node_id = ?1 AND scroll_node_kind IN ('agent', 'process')",
                node_id.to_string(),
            ),
            _ => None,
        },
    }
}

use rusqlite::OptionalExtension;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{saved_session, saved_workspace, T0};
    use turn_core::ids::NodeId;
    use turn_core::model::ProcessNode;

    #[test]
    fn installation_inventory_accounts_for_every_schema_table_and_redacts_secrets() {
        let store = Store::open_in_memory().unwrap();
        store
            .connection()
            .execute(
                "INSERT INTO settings (key, value_json, updated_ms) VALUES (?1, ?2, ?3)",
                params![
                    "future.credential",
                    "\"ghp_abcdefghijklmnopqrstuvwxyz0123456789\"",
                    T0
                ],
            )
            .unwrap();
        let rows = store.privacy_rows(&PrivacyScope::Installation).unwrap();
        let serialized = serde_json::to_string(&rows).unwrap();
        assert!(
            !serialized.contains("ghp_"),
            "credential leaked: {serialized}"
        );

        let represented: BTreeSet<&str> = rows.iter().map(|row| row.data_type.as_str()).collect();
        assert!(represented.contains("settings"));
        assert_eq!(
            TABLES.len(),
            17,
            "a table addition must update the catalogue"
        );
    }

    #[test]
    fn agent_scope_and_delete_take_every_row_that_names_the_agent() {
        let store = Store::open_in_memory().unwrap();
        let workspace = saved_workspace(&store, "privacy");
        let mut session = saved_session(&store, &workspace.id, "privacy");
        let mut node = ProcessNode::agent(session.id.clone(), "claude", "/repo", T0);
        node.id = NodeId::from_stored("proc_privacy");
        session.tree.insert(node.clone());
        store.sessions().save(&session).unwrap();

        let scope = PrivacyScope::Agent {
            session_id: session.id.clone(),
            node_id: node.id.clone(),
        };
        store
            .connection()
            .execute(
                "INSERT INTO tree_ui_state \
                 (surface_id, node_kind, node_id, updated_ms) VALUES ('window', 'process', ?1, ?2)",
                params![node.id.as_str(), T0],
            )
            .unwrap();
        store
            .connection()
            .execute(
                "INSERT INTO tree_surface_preferences \
                 (surface_id, scroll_node_kind, scroll_node_id, updated_ms) \
                 VALUES ('window', 'process', ?1, ?2)",
                params![node.id.as_str(), T0],
            )
            .unwrap();
        let rows = store.privacy_rows(&scope).unwrap();
        assert!(rows.iter().any(|row| row.data_type == "process_nodes"));
        assert!(rows
            .iter()
            .any(|row| row.data_type == "tree_surface_preferences"));
        assert!(store.delete_agent_records(&session.id, &node.id).unwrap());
        assert!(store.privacy_rows(&scope).unwrap().is_empty());
        let anchor: Option<String> = store
            .connection()
            .query_row(
                "SELECT scroll_node_id FROM tree_surface_preferences WHERE surface_id = 'window'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(anchor.is_none());
    }

    #[test]
    fn retention_uses_the_configured_event_and_preview_limits() {
        let store = Store::open_in_memory().unwrap();
        let workspace = saved_workspace(&store, "retention");
        let mut session = saved_session(&store, &workspace.id, "retention");
        let node = ProcessNode::agent(session.id.clone(), "agent", "/repo", T0);
        let node_id = node.id.clone();
        session.tree.insert(node);
        store.sessions().save(&session).unwrap();
        for index in 0..5 {
            store
                .connection()
                .execute(
                    "INSERT INTO activity_previews (node_id, normalized_text, source_type, \
                     confidence, stable, contains_sensitive_data, redacted, created_ms) \
                     VALUES (?1, ?2, 'hook', 'explicit', 1, 0, 0, ?3)",
                    params![node_id.as_str(), format!("preview {index}"), T0 + index],
                )
                .unwrap();
        }
        let policy = PrivacyPolicy {
            preview_keep_per_agent: 2,
            preview_max_records: 2,
            ..PrivacyPolicy::default()
        };
        let outcome = store.prune_privacy(&policy, T0 + 10).unwrap();
        assert_eq!(outcome.previews, 3);
        let count: i64 = store
            .connection()
            .query_row("SELECT COUNT(*) FROM activity_previews", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 2);
    }

    #[cfg(unix)]
    #[test]
    fn export_refuses_an_existing_symlink_without_touching_its_target() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("outside.json");
        std::fs::write(&target, b"keep").unwrap();
        let link = temp.path().join("export.json");
        symlink(&target, &link).unwrap();
        let document = PrivacyExportDocument {
            schema: 1,
            generated_ms: T0,
            scope: PrivacyScope::Installation,
            policy: PrivacyPolicy::default(),
            telemetry_enabled: false,
            telemetry_endpoints: 0,
            data: Vec::new(),
        };
        assert!(write_export_create_new(&link, &document).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"keep");
    }
}
