//! ADR-040 persistence: checkout leases, view bindings, previews and per-surface tree state.

use crate::codec::{from_tag, tag};
use crate::error::{Result, StoreError};
use rusqlite::{params, Connection, OptionalExtension, Row};
use turn_core::ids::{CheckoutId, LeaseId, NodeId, PaneId, SessionId, WorkspaceId};
use turn_core::model::{
    ActivityPreview, HierarchyNodeKind, LeaseState, PaneNodeBinding, TreeUiState,
    WorkspaceCheckout, WorkspaceWriteLease,
};

pub struct HierarchyRepo<'a> {
    conn: &'a Connection,
}

impl<'a> HierarchyRepo<'a> {
    pub(crate) fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    pub fn primary_checkout(&self, workspace: &WorkspaceId) -> Result<Option<WorkspaceCheckout>> {
        self.conn
            .query_row(
                "SELECT id, workspace_id, path, canonical_path, branch, is_primary, \
                        shared_resources_json, created_ms \
                 FROM workspace_checkouts WHERE workspace_id = ?1 AND is_primary = 1",
                params![workspace.as_str()],
                checkout_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    /// Acquires checkout exclusivity. The partial unique indexes are the final
    /// arbiter; the preceding read exists only to return the current owner cleanly.
    pub fn acquire_write_lease(
        &self,
        workspace: &WorkspaceId,
        session: &SessionId,
        checkout: &CheckoutId,
        now_ms: i64,
    ) -> Result<WorkspaceWriteLease> {
        let tx = self.conn.unchecked_transaction()?;
        let canonical: String = tx.query_row(
            "SELECT canonical_path FROM workspace_checkouts \
             WHERE id = ?1 AND workspace_id = ?2",
            params![checkout.as_str(), workspace.as_str()],
            |row| row.get(0),
        )?;

        let held: Option<WorkspaceWriteLease> = tx
            .query_row(
                "SELECT l.id, l.workspace_id, l.session_id, l.checkout_id, l.mode, l.state, \
                        l.acquired_ms, l.heartbeat_ms, l.released_ms, l.generation \
                 FROM workspace_write_leases l \
                 JOIN workspace_checkouts c ON c.id = l.checkout_id \
                 WHERE c.canonical_path = ?1 AND l.state != 'released' LIMIT 1",
                params![canonical],
                lease_from_row,
            )
            .optional()?;
        if let Some(held) = held {
            if held.session_id == *session && held.state == LeaseState::Active {
                return Ok(held);
            }
            return Err(StoreError::WriteLeaseHeld {
                checkout_id: held.checkout_id.to_string(),
                owner_session_id: held.session_id.to_string(),
                lease_id: held.id.to_string(),
            });
        }

        let generation: i64 = tx.query_row(
            "SELECT COALESCE(MAX(l.generation), 0) + 1 \
             FROM workspace_write_leases l \
             JOIN workspace_checkouts c ON c.id = l.checkout_id \
             WHERE c.canonical_path = ?1",
            params![canonical],
            |row| row.get(0),
        )?;
        let mut lease = WorkspaceWriteLease::active(
            workspace.clone(),
            session.clone(),
            checkout.clone(),
            now_ms,
        );
        lease.generation = generation as u64;
        tx.execute(
            "INSERT INTO workspace_write_leases \
                 (id, workspace_id, session_id, checkout_id, mode, state, acquired_ms, \
                  heartbeat_ms, released_ms, generation) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, ?9)",
            params![
                lease.id.as_str(),
                lease.workspace_id.as_str(),
                lease.session_id.as_str(),
                lease.checkout_id.as_str(),
                tag("lease mode", &lease.mode)?,
                tag("lease state", &lease.state)?,
                lease.acquired_ms,
                lease.heartbeat_ms,
                lease.generation as i64,
            ],
        )?;
        tx.execute(
            "UPDATE sessions SET mode = 'main_checkout', checkout_id = ?2, \
                    worktree_path = NULL, read_only_enforced = 0 WHERE id = ?1",
            params![session.as_str(), checkout.as_str()],
        )?;
        tx.execute(
            "UPDATE workspaces SET lease_reconciliation_required = 0 WHERE id = ?1",
            params![workspace.as_str()],
        )?;
        tx.commit()?;
        Ok(lease)
    }

    pub fn active_lease(&self, workspace: &WorkspaceId) -> Result<Option<WorkspaceWriteLease>> {
        self.conn
            .query_row(
                "SELECT id, workspace_id, session_id, checkout_id, mode, state, acquired_ms, \
                        heartbeat_ms, released_ms, generation \
                 FROM workspace_write_leases \
                 WHERE workspace_id = ?1 AND state != 'released' \
                 ORDER BY generation DESC LIMIT 1",
                params![workspace.as_str()],
                lease_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn heartbeat_lease(&self, id: &LeaseId, generation: u64, now_ms: i64) -> Result<bool> {
        Ok(self.conn.execute(
            "UPDATE workspace_write_leases SET heartbeat_ms = ?3 \
             WHERE id = ?1 AND generation = ?2 AND state = 'active'",
            params![id.as_str(), generation as i64, now_ms],
        )? > 0)
    }

    pub fn require_recovery(&self, id: &LeaseId, now_ms: i64) -> Result<bool> {
        Ok(self.conn.execute(
            "UPDATE workspace_write_leases SET state = 'recovery_required', heartbeat_ms = ?2 \
             WHERE id = ?1 AND state != 'released'",
            params![id.as_str(), now_ms],
        )? > 0)
    }

    pub fn release_lease_for_session(&self, session: &SessionId, now_ms: i64) -> Result<bool> {
        Ok(self.conn.execute(
            "UPDATE workspace_write_leases SET state = 'released', released_ms = ?2, \
                    heartbeat_ms = ?2 WHERE session_id = ?1 AND state != 'released'",
            params![session.as_str(), now_ms],
        )? > 0)
    }

    pub fn assign_read_only(&self, session: &SessionId, enforced: bool) -> Result<bool> {
        Ok(self.conn.execute(
            "UPDATE sessions SET mode = 'read_only', worktree_path = NULL, \
                    read_only_enforced = ?2 WHERE id = ?1",
            params![session.as_str(), enforced],
        )? > 0)
    }

    pub fn bind_pane(&self, binding: &PaneNodeBinding) -> Result<()> {
        self.conn.execute(
            "INSERT INTO pane_node_bindings \
                 (pane_id, session_id, node_id, temporary, surface_id, opened_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
             ON CONFLICT(session_id, pane_id) DO UPDATE SET \
                 node_id = excluded.node_id, temporary = excluded.temporary, \
                 surface_id = excluded.surface_id, opened_ms = excluded.opened_ms",
            params![
                binding.pane_id.as_str(),
                binding.session_id.as_str(),
                binding.node_id.as_str(),
                binding.temporary,
                binding.surface_id,
                binding.opened_ms,
            ],
        )?;
        Ok(())
    }

    pub fn unbind_pane(&self, session: &SessionId, pane: &PaneId) -> Result<bool> {
        Ok(self.conn.execute(
            "DELETE FROM pane_node_bindings WHERE session_id = ?1 AND pane_id = ?2",
            params![session.as_str(), pane.as_str()],
        )? > 0)
    }

    pub fn bindings_for_session(&self, session: &SessionId) -> Result<Vec<PaneNodeBinding>> {
        let mut stmt = self.conn.prepare(
            "SELECT pane_id, session_id, node_id, temporary, surface_id, opened_ms \
             FROM pane_node_bindings WHERE session_id = ?1 ORDER BY opened_ms, pane_id",
        )?;
        let rows = stmt.query_map(params![session.as_str()], binding_from_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn preview_history(&self, node: &NodeId, limit: usize) -> Result<Vec<ActivityPreview>> {
        let limit = limit.clamp(1, 20) as i64;
        let mut stmt = self.conn.prepare(
            "SELECT node_id, raw_source_sequence, normalized_text, source_type, confidence, \
                    stable, contains_sensitive_data, redacted, created_ms \
             FROM activity_previews WHERE node_id = ?1 \
             ORDER BY created_ms DESC, id DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![node.as_str(), limit], preview_from_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn save_tree_state(&self, state: &TreeUiState) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        if state.selected {
            tx.execute(
                "UPDATE tree_ui_state SET selected = 0, updated_ms = ?2 \
                 WHERE surface_id = ?1 AND selected = 1",
                params![state.surface_id, state.updated_ms],
            )?;
        }
        tx.execute(
            "INSERT INTO tree_ui_state \
                 (surface_id, node_kind, node_id, expanded, selected, manual_order, \
                  visibility_mode, updated_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
             ON CONFLICT(surface_id, node_kind, node_id) DO UPDATE SET \
                 expanded = excluded.expanded, selected = excluded.selected, \
                 manual_order = excluded.manual_order, \
                 visibility_mode = excluded.visibility_mode, updated_ms = excluded.updated_ms",
            params![
                state.surface_id,
                tag("hierarchy node kind", &state.node_kind)?,
                state.node_id,
                state.expanded,
                state.selected,
                state.manual_order,
                state.visibility_mode,
                state.updated_ms,
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn tree_state(&self, surface_id: &str) -> Result<Vec<TreeUiState>> {
        let mut stmt = self.conn.prepare(
            "SELECT surface_id, node_kind, node_id, expanded, selected, manual_order, \
                    visibility_mode, updated_ms \
             FROM tree_ui_state WHERE surface_id = ?1 \
             ORDER BY COALESCE(manual_order, 2147483647), node_id",
        )?;
        let rows = stmt.query_map(params![surface_id], |row| {
            let raw: String = row.get("node_kind")?;
            let kind = from_tag::<HierarchyNodeKind>("hierarchy node kind", surface_id, &raw)
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
            Ok(TreeUiState {
                surface_id: row.get("surface_id")?,
                node_kind: kind,
                node_id: row.get("node_id")?,
                expanded: row.get("expanded")?,
                selected: row.get("selected")?,
                manual_order: row.get("manual_order")?,
                visibility_mode: row.get("visibility_mode")?,
                updated_ms: row.get("updated_ms")?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }
}

fn checkout_from_row(row: &Row<'_>) -> rusqlite::Result<WorkspaceCheckout> {
    Ok(WorkspaceCheckout {
        id: CheckoutId::from_stored(row.get::<_, String>("id")?),
        workspace_id: WorkspaceId::from_stored(row.get::<_, String>("workspace_id")?),
        path: row.get("path")?,
        canonical_path: row.get("canonical_path")?,
        branch: row.get("branch")?,
        primary: row.get("is_primary")?,
        shared_resources: serde_json::from_str(&row.get::<_, String>("shared_resources_json")?)
            .unwrap_or_default(),
        created_ms: row.get("created_ms")?,
    })
}

fn lease_from_row(row: &Row<'_>) -> rusqlite::Result<WorkspaceWriteLease> {
    let id: String = row.get("id")?;
    let mode_raw: String = row.get("mode")?;
    let state_raw: String = row.get("state")?;
    let mode = from_tag("lease mode", &id, &mode_raw)
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
    let state = from_tag("lease state", &id, &state_raw)
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
    Ok(WorkspaceWriteLease {
        id: LeaseId::from_stored(id),
        workspace_id: WorkspaceId::from_stored(row.get::<_, String>("workspace_id")?),
        session_id: SessionId::from_stored(row.get::<_, String>("session_id")?),
        checkout_id: CheckoutId::from_stored(row.get::<_, String>("checkout_id")?),
        mode,
        state,
        acquired_ms: row.get("acquired_ms")?,
        heartbeat_ms: row.get("heartbeat_ms")?,
        released_ms: row.get("released_ms")?,
        generation: row.get::<_, i64>("generation")? as u64,
    })
}

fn binding_from_row(row: &Row<'_>) -> rusqlite::Result<PaneNodeBinding> {
    Ok(PaneNodeBinding {
        pane_id: PaneId::from_stored(row.get::<_, String>("pane_id")?),
        session_id: SessionId::from_stored(row.get::<_, String>("session_id")?),
        node_id: NodeId::from_stored(row.get::<_, String>("node_id")?),
        temporary: row.get("temporary")?,
        surface_id: row.get("surface_id")?,
        opened_ms: row.get("opened_ms")?,
    })
}

fn preview_from_row(row: &Row<'_>) -> rusqlite::Result<ActivityPreview> {
    let node = row.get::<_, String>("node_id")?;
    let source_raw: String = row.get("source_type")?;
    let confidence_raw: String = row.get("confidence")?;
    let source = from_tag("preview source", &node, &source_raw)
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
    let confidence = from_tag("preview confidence", &node, &confidence_raw)
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
    Ok(ActivityPreview {
        node_id: NodeId::from_stored(node),
        raw_source_sequence: row
            .get::<_, Option<i64>>("raw_source_sequence")?
            .map(|value| value as u64),
        normalized_text: row.get("normalized_text")?,
        source,
        confidence,
        stable: row.get("stable")?,
        contains_sensitive_data: row.get("contains_sensitive_data")?,
        redacted: row.get("redacted")?,
        updated_ms: row.get("created_ms")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing;
    use turn_core::model::{HierarchyNodeKind, ProcessNode};

    const T0: i64 = 1_700_000_000_000;

    #[test]
    fn only_one_session_can_hold_a_checkout_and_release_is_explicit() {
        let store = testing::store();
        let workspace = testing::saved_workspace(&store, "lease");
        let first = testing::saved_session(&store, &workspace.id, "first");
        let second = testing::saved_session(&store, &workspace.id, "second");
        let checkout = CheckoutId::primary_for(&workspace.id);

        let lease = store
            .hierarchy()
            .acquire_write_lease(&workspace.id, &first.id, &checkout, T0)
            .unwrap();
        let error = store
            .hierarchy()
            .acquire_write_lease(&workspace.id, &second.id, &checkout, T0 + 1)
            .unwrap_err();
        assert!(matches!(error, StoreError::WriteLeaseHeld { .. }));
        assert!(store
            .hierarchy()
            .release_lease_for_session(&first.id, T0 + 2)
            .unwrap());
        let second_lease = store
            .hierarchy()
            .acquire_write_lease(&workspace.id, &second.id, &checkout, T0 + 3)
            .unwrap();
        assert!(second_lease.generation > lease.generation);
    }

    #[test]
    fn one_node_can_have_many_views_and_closing_them_never_deletes_it() {
        let store = testing::store();
        let session = testing::saved_session_anywhere(&store, "bindings");
        let node = ProcessNode::agent(session.id.clone(), "claude", "/repo", T0);
        store.nodes().upsert(&node).unwrap();
        for (name, temporary) in [("pane_a", false), ("pane_b", true)] {
            store
                .hierarchy()
                .bind_pane(&PaneNodeBinding {
                    pane_id: PaneId::from_stored(name),
                    session_id: session.id.clone(),
                    node_id: node.id.clone(),
                    temporary,
                    surface_id: temporary.then(|| "window-a".to_string()),
                    opened_ms: T0,
                })
                .unwrap();
        }
        assert_eq!(
            store
                .hierarchy()
                .bindings_for_session(&session.id)
                .unwrap()
                .len(),
            2
        );
        store
            .hierarchy()
            .unbind_pane(&session.id, &PaneId::from_stored("pane_b"))
            .unwrap();
        assert!(store.nodes().get(&node.id).unwrap().is_some());
    }

    #[test]
    fn tree_selection_is_private_to_a_surface() {
        let store = testing::store();
        for surface in ["window-a", "window-b"] {
            store
                .hierarchy()
                .save_tree_state(&TreeUiState {
                    surface_id: surface.into(),
                    node_kind: HierarchyNodeKind::Workspace,
                    node_id: "ws_a".into(),
                    expanded: true,
                    selected: true,
                    manual_order: None,
                    visibility_mode: Some("normal".into()),
                    updated_ms: T0,
                })
                .unwrap();
        }
        assert_eq!(store.hierarchy().tree_state("window-a").unwrap().len(), 1);
        assert_eq!(store.hierarchy().tree_state("window-b").unwrap().len(), 1);
    }
}
