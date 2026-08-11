//! Authenticated local-data inventory, export, deletion and compaction.

use super::Answer;
use crate::core::Core;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use turn_core::ids::SessionId;
use turn_core::privacy::{
    PrivacyCategory, PrivacyDatum, PrivacyDeletionReport, PrivacyExportDocument,
    PrivacyExportResult, PrivacyPolicy, PrivacyReport, PrivacyScope, PRIVACY_POLICY_KEYS,
};
use turn_proto::{CloseDisposition, ErrorCode, ProtoError, Response};

impl Core {
    /// Applies every bounded policy without blocking normal use on a maintenance
    /// failure. Explicit report/export/delete requests still return those failures.
    pub(crate) fn maintain_privacy(&mut self, now_ms: i64) {
        self.last_privacy_maintenance_ms = now_ms;
        let policy = self.privacy_policy();
        match self.store.prune_privacy(&policy, now_ms) {
            Ok(outcome) if outcome.events > 0 || outcome.previews > 0 => tracing::info!(
                events = outcome.events,
                previews = outcome.previews,
                "applied local-data retention"
            ),
            Ok(_) => {}
            Err(error) => tracing::warn!(%error, "could not apply local-data retention"),
        }
        if let Err(error) =
            crate::privacy::enforce_log_privacy(&self.data_dir, policy.diagnostic_log_bytes)
        {
            tracing::warn!(%error, "could not enforce the diagnostic-log bound");
        }
        self.enforce_terminal_history_policy();
    }

    pub(crate) fn privacy_setting_changed(&mut self, key: &str, now_ms: i64) {
        if PRIVACY_POLICY_KEYS.contains(&key) {
            self.maintain_privacy(now_ms);
        }
    }

    pub(super) fn get_privacy_report(&self, scope: PrivacyScope, now_ms: i64) -> Answer {
        self.validate_privacy_scope(&scope)?;
        let rows = self.privacy_rows(&scope)?;
        Ok(Response::PrivacyReport {
            report: Box::new(self.summarise_privacy(scope, rows, now_ms)),
        })
    }

    pub(super) fn export_privacy_data(
        &self,
        scope: PrivacyScope,
        path: String,
        now_ms: i64,
    ) -> Answer {
        self.validate_privacy_scope(&scope)?;
        let destination = validated_export_path(&path)?;
        let rows = self.privacy_rows(&scope)?;
        let items = rows.len() as u64;
        let document = PrivacyExportDocument {
            schema: 1,
            generated_ms: now_ms,
            scope,
            policy: self.privacy_policy(),
            telemetry_enabled: false,
            telemetry_endpoints: 0,
            data: rows,
        };
        let bytes = turn_store::privacy::write_export_create_new(&destination, &document)
            .map_err(privacy_store)?;
        Ok(Response::PrivacyExported {
            export: PrivacyExportResult {
                path: destination.display().to_string(),
                items,
                bytes,
            },
        })
    }

    pub(super) fn delete_privacy_data(
        &mut self,
        scope: PrivacyScope,
        disposition: CloseDisposition,
        now_ms: i64,
    ) -> Answer {
        if matches!(scope, PrivacyScope::Installation) {
            return Err(ProtoError::refused(
                "Installation-wide deletion requires Turn's daemon to be stopped",
            )
            .with_detail(
                "Run `turnd --delete-installation-data`; it acquires the installation lock and refuses a live daemon",
            ));
        }
        if disposition == CloseDisposition::KeepProcesses {
            return Err(ProtoError::refused(
                "Deleting local data cannot keep its processes running",
            ));
        }
        self.validate_privacy_scope(&scope)?;
        let workspace_sessions = self.privacy_workspace_sessions(&scope);
        let before_database = self.store.privacy_rows(&scope).map_err(privacy_store)?;
        let before_files = crate::privacy::file_rows(&self.data_dir, &scope, &workspace_sessions)
            .map_err(privacy_daemon)?;

        let escaped_processes = match &scope {
            PrivacyScope::Workspace { workspace_id } => {
                let response = self.delete_workspace(workspace_id, disposition, now_ms)?;
                workspace_sessions.iter().for_each(|id| {
                    crate::paths::remove_session_terminal_history(&self.data_dir, id)
                });
                closed_node_ids(response)
            }
            PrivacyScope::Session { session_id } => {
                let response = self.delete_session(session_id, disposition, now_ms)?;
                crate::paths::remove_session_terminal_history(&self.data_dir, session_id);
                closed_node_ids(response)
            }
            PrivacyScope::Agent {
                session_id,
                node_id,
            } => self.delete_agent_privacy(session_id, node_id, disposition, now_ms)?,
            PrivacyScope::Installation => unreachable!("handled above"),
        };

        let after_database = self.store.privacy_rows(&scope).map_err(privacy_store)?;
        let after_files = crate::privacy::file_rows(&self.data_dir, &scope, &workspace_sessions)
            .map_err(privacy_daemon)?;
        self.store.compact().map_err(privacy_store)?;

        let before_bytes = bytes(&before_database).saturating_add(bytes(&before_files));
        let after_bytes = bytes(&after_database).saturating_add(bytes(&after_files));
        Ok(Response::PrivacyDeleted {
            report: PrivacyDeletionReport {
                scope,
                records_deleted: (before_database.len() as u64)
                    .saturating_sub(after_database.len() as u64),
                files_deleted: (before_files.len() as u64).saturating_sub(after_files.len() as u64),
                bytes_freed: before_bytes.saturating_sub(after_bytes),
                database_compacted: true,
                escaped_processes,
            },
        })
    }

    pub(super) fn compact_privacy_data(&self, now_ms: i64) -> Answer {
        let before_bytes =
            crate::privacy::persistent_bytes(&self.data_dir).map_err(privacy_daemon)?;
        let policy = self.privacy_policy();
        self.store
            .prune_privacy(&policy, now_ms)
            .map_err(privacy_store)?;
        crate::privacy::enforce_log_privacy(&self.data_dir, policy.diagnostic_log_bytes)
            .map_err(privacy_daemon)?;
        self.store.compact().map_err(privacy_store)?;
        let after_bytes =
            crate::privacy::persistent_bytes(&self.data_dir).map_err(privacy_daemon)?;
        Ok(Response::PrivacyCompacted {
            before_bytes,
            after_bytes,
        })
    }

    pub(crate) fn privacy_policy(&self) -> PrivacyPolicy {
        let mut policy = PrivacyPolicy::default();
        for key in PRIVACY_POLICY_KEYS {
            policy.apply(key, &self.setting_for(None, key));
        }
        policy
    }

    fn enforce_terminal_history_policy(&mut self) {
        let disabled: Vec<(SessionId, Vec<turn_core::ids::NodeId>)> = self
            .sessions
            .iter()
            .filter(|(session_id, _)| !self.terminal_history_enabled(session_id))
            .map(|(session_id, session)| {
                (
                    session_id.clone(),
                    session.tree.iter().map(|node| node.id.clone()).collect(),
                )
            })
            .collect();
        for (session_id, nodes) in disabled {
            for node in &nodes {
                if let Some(process) = self.processes.get(node) {
                    process.pty.disable_journal();
                }
                self.recovered_terminals.remove(node);
            }
            crate::paths::remove_session_terminal_history(&self.data_dir, &session_id);
        }
    }

    fn validate_privacy_scope(&self, scope: &PrivacyScope) -> Result<(), ProtoError> {
        match scope {
            PrivacyScope::Installation => Ok(()),
            PrivacyScope::Workspace { workspace_id } => self.workspace(workspace_id).map(|_| ()),
            PrivacyScope::Session { session_id } => self.session(session_id).map(|_| ()),
            PrivacyScope::Agent {
                session_id,
                node_id,
            } => self.node_of(session_id, node_id).map(|_| ()),
        }
    }

    fn privacy_workspace_sessions(&self, scope: &PrivacyScope) -> Vec<SessionId> {
        match scope {
            PrivacyScope::Workspace { workspace_id } => self
                .sessions
                .values()
                .filter(|session| &session.workspace_id == workspace_id)
                .map(|session| session.id.clone())
                .collect(),
            _ => Vec::new(),
        }
    }

    fn privacy_rows(&self, scope: &PrivacyScope) -> Result<Vec<PrivacyDatum>, ProtoError> {
        let mut rows = self.store.privacy_rows(scope).map_err(privacy_store)?;
        rows.extend(
            crate::privacy::file_rows(
                &self.data_dir,
                scope,
                &self.privacy_workspace_sessions(scope),
            )
            .map_err(privacy_daemon)?,
        );
        rows.sort_by(|left, right| left.origin.cmp(&right.origin));
        Ok(rows)
    }

    fn summarise_privacy(
        &self,
        scope: PrivacyScope,
        rows: Vec<PrivacyDatum>,
        now_ms: i64,
    ) -> PrivacyReport {
        let total_bytes = bytes(&rows);
        let total_items = rows.len() as u64;
        let mut aggregate: BTreeMap<String, (u64, u64)> = BTreeMap::new();
        for row in rows {
            let category = aggregate.entry(row.data_type).or_default();
            category.0 = category.0.saturating_add(1);
            category.1 = category.1.saturating_add(row.bytes);
        }
        PrivacyReport {
            generated_ms: now_ms,
            scope,
            policy: self.privacy_policy(),
            telemetry_enabled: false,
            telemetry_endpoints: 0,
            total_items,
            total_bytes,
            categories: aggregate
                .into_iter()
                .map(|(data_type, (items, bytes))| PrivacyCategory {
                    data_type,
                    items,
                    bytes,
                })
                .collect(),
        }
    }

    fn delete_agent_privacy(
        &mut self,
        session_id: &turn_core::ids::SessionId,
        node_id: &turn_core::ids::NodeId,
        disposition: CloseDisposition,
        now_ms: i64,
    ) -> Result<Vec<turn_core::ids::NodeId>, ProtoError> {
        let descendants: Vec<_> = self
            .session(session_id)?
            .tree
            .descendants(node_id)
            .into_iter()
            .map(|node| node.id.clone())
            .collect();
        let retired: Vec<_> = std::iter::once(node_id.clone())
            .chain(descendants.iter().cloned())
            .collect();
        let escaped: Vec<_> = retired
            .iter()
            .filter(|id| {
                self.session(session_id)
                    .ok()
                    .and_then(|session| session.tree.get(id))
                    .is_some_and(|node| node.is_running())
                    && !self.processes.contains_key(*id)
                    && !self.is_hosted(id)
            })
            .cloned()
            .collect();
        let owned: Vec<_> = retired
            .iter()
            .filter(|id| self.processes.contains_key(*id))
            .cloned()
            .collect();
        for id in owned {
            self.stop_and_release(
                session_id,
                &id,
                disposition == CloseDisposition::Kill,
                now_ms,
            );
        }
        self.retire_replaced_node(session_id, node_id, &descendants, now_ms)?;
        for id in &retired {
            self.store
                .delete_agent_records(session_id, id)
                .map_err(privacy_store)?;
        }
        self.persist_session(session_id)?;
        self.bump_hierarchy();
        self.push_hierarchy_all(now_ms);
        Ok(escaped)
    }
}

fn validated_export_path(path: &str) -> Result<PathBuf, ProtoError> {
    let path = PathBuf::from(path.trim());
    if !path.is_absolute() {
        return Err(ProtoError::invalid(
            "A privacy export path must be absolute",
        ));
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("/"));
    if !parent.is_dir() {
        return Err(ProtoError::invalid(
            "The privacy export directory must already exist",
        ));
    }
    Ok(path)
}

fn closed_node_ids(response: Response) -> Vec<turn_core::ids::NodeId> {
    match response {
        Response::Closed { escaped } => escaped.into_iter().map(|item| item.node_id).collect(),
        _ => Vec::new(),
    }
}

fn bytes(rows: &[PrivacyDatum]) -> u64 {
    rows.iter()
        .fold(0u64, |total, row| total.saturating_add(row.bytes))
}

fn privacy_store(error: turn_store::StoreError) -> ProtoError {
    tracing::error!(%error, "local privacy operation failed in the store");
    ProtoError::new(
        ErrorCode::Unavailable,
        "Turn could not complete the local-data operation",
    )
    .with_detail(error.to_string())
}

fn privacy_daemon(error: crate::error::DaemonError) -> ProtoError {
    tracing::error!(%error, "local privacy filesystem operation failed");
    ProtoError::new(
        ErrorCode::Unavailable,
        "Turn could not complete the local-data operation",
    )
    .with_detail(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::testing::Harness;
    use turn_core::ids::{PaneId, SessionId};

    const NOW: i64 = 1_786_000_000_000;
    const TOKEN: &str = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";

    #[tokio::test]
    async fn report_and_export_are_complete_redacted_and_explicitly_telemetry_free() {
        let mut harness = Harness::new().await;
        let session_id = SessionId::from_stored("sess_privacy_export");
        harness.add_session(
            session_id.clone(),
            PaneId::from_stored("pane_privacy_export"),
            NOW,
        );
        harness.core.sessions.get_mut(&session_id).unwrap().name = TOKEN.into();
        harness.core.persist_session(&session_id).unwrap();

        let report = harness
            .core
            .get_privacy_report(PrivacyScope::Installation, NOW)
            .unwrap();
        match report {
            Response::PrivacyReport { report } => {
                assert!(!report.telemetry_enabled);
                assert_eq!(report.telemetry_endpoints, 0);
                assert!(report.total_items > 0);
                assert!(report
                    .categories
                    .iter()
                    .any(|row| row.data_type == "sessions"));
            }
            other => panic!("expected privacy report, got {other:?}"),
        }

        let destination = harness._dir.path().join("reviewable-export.json");
        let exported = harness
            .core
            .export_privacy_data(
                PrivacyScope::Installation,
                destination.display().to_string(),
                NOW,
            )
            .unwrap();
        assert!(matches!(exported, Response::PrivacyExported { .. }));
        let text = std::fs::read_to_string(&destination).unwrap();
        assert!(!text.contains(TOKEN), "credential leaked: {text}");
        let document: PrivacyExportDocument = serde_json::from_str(&text).unwrap();
        assert!(document
            .data
            .iter()
            .all(|datum| !datum.origin.is_empty() && !datum.data_type.is_empty()));
        assert_eq!(document.telemetry_endpoints, 0);
        assert!(
            harness
                .core
                .export_privacy_data(
                    PrivacyScope::Installation,
                    destination.display().to_string(),
                    NOW + 1,
                )
                .is_err(),
            "an export must never overwrite an existing file"
        );
    }

    #[tokio::test]
    async fn deleting_a_session_removes_sqlite_scratch_and_terminal_history() {
        let mut harness = Harness::new().await;
        let session_id = SessionId::from_stored("sess_privacy_delete");
        harness.add_session(
            session_id.clone(),
            PaneId::from_stored("pane_privacy_delete"),
            NOW,
        );
        let scratch = crate::paths::session_scratch(harness._dir.path(), &session_id);
        let history = crate::paths::session_terminal_history(harness._dir.path(), &session_id);
        std::fs::create_dir_all(&scratch).unwrap();
        std::fs::create_dir_all(&history).unwrap();
        std::fs::write(scratch.join("config"), b"secret").unwrap();
        std::fs::write(history.join("journal.bin"), b"terminal").unwrap();

        let response = harness
            .core
            .delete_privacy_data(
                PrivacyScope::Session {
                    session_id: session_id.clone(),
                },
                CloseDisposition::Terminate,
                NOW + 1,
            )
            .unwrap();
        match response {
            Response::PrivacyDeleted { report } => {
                assert!(report.records_deleted > 0);
                assert!(report.files_deleted >= 2);
                assert!(report.database_compacted);
            }
            other => panic!("expected deletion report, got {other:?}"),
        }
        assert!(!harness.core.sessions.contains_key(&session_id));
        assert!(!scratch.exists());
        assert!(!history.exists());
        assert!(harness
            .core
            .store
            .privacy_rows(&PrivacyScope::Session { session_id })
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn turning_off_terminal_history_removes_it_without_stopping_the_session() {
        let mut harness = Harness::new().await;
        let session_id = SessionId::from_stored("sess_privacy_history");
        harness.add_session(
            session_id.clone(),
            PaneId::from_stored("pane_privacy_history"),
            NOW,
        );
        let history = crate::paths::session_terminal_history(harness._dir.path(), &session_id);
        std::fs::create_dir_all(&history).unwrap();
        std::fs::write(history.join("journal.bin"), b"terminal").unwrap();

        harness
            .core
            .set_setting(
                turn_core::settings::Scope::Session,
                Some(session_id.to_string()),
                turn_core::privacy::TERMINAL_HISTORY_KEY.into(),
                serde_json::json!(false),
                NOW + 1,
            )
            .unwrap();
        assert!(!harness.core.terminal_history_enabled(&session_id));
        assert!(!history.exists());
        assert!(harness.core.sessions.contains_key(&session_id));
    }
}
