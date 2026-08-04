//! ADR-040 persistence: checkout leases, view bindings, previews and per-surface tree state.

use crate::codec::{from_tag, tag};
use crate::error::{Result, StoreError};
use rusqlite::{params, Connection, OptionalExtension, Row, Transaction, TransactionBehavior};
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
        // `IMMEDIATE` serialises the read-check-generation-write sequence across
        // daemon processes. The global partial unique index remains the final
        // arbiter, but no contender can read a generation that another writer is
        // concurrently about to consume.
        let tx = Transaction::new_unchecked(self.conn, TransactionBehavior::Immediate)?;
        let canonical: Option<String> = tx
            .query_row(
                "SELECT c.canonical_path \
                 FROM sessions s \
                 JOIN workspace_checkouts c ON c.workspace_id = s.workspace_id \
                 WHERE s.id = ?1 AND s.workspace_id = ?2 \
                   AND c.id = ?3 AND c.workspace_id = ?2",
                params![session.as_str(), workspace.as_str(), checkout.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        let Some(canonical) = canonical else {
            return Err(StoreError::InvalidLeaseOwnership {
                workspace_id: workspace.to_string(),
                session_id: session.to_string(),
                checkout_id: checkout.to_string(),
            });
        };

        let held: Option<WorkspaceWriteLease> = tx
            .query_row(
                "SELECT l.id, l.workspace_id, l.session_id, l.checkout_id, l.mode, l.state, \
                        l.acquired_ms, l.heartbeat_ms, l.released_ms, l.generation \
                 FROM workspace_write_leases l \
                 WHERE l.canonical_path = ?1 AND l.state != 'released' LIMIT 1",
                params![&canonical],
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

        let changed = tx.execute(
            "UPDATE checkout_write_fences SET generation = generation + 1 \
             WHERE canonical_path = ?1",
            params![&canonical],
        )?;
        if changed != 1 {
            return Err(StoreError::InvalidLeaseOwnership {
                workspace_id: workspace.to_string(),
                session_id: session.to_string(),
                checkout_id: checkout.to_string(),
            });
        }
        let generation: i64 = tx.query_row(
            "SELECT generation FROM checkout_write_fences WHERE canonical_path = ?1",
            params![&canonical],
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
                 (id, workspace_id, session_id, checkout_id, canonical_path, mode, state, \
                  acquired_ms, heartbeat_ms, released_ms, generation) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL, ?10)",
            params![
                lease.id.as_str(),
                lease.workspace_id.as_str(),
                lease.session_id.as_str(),
                lease.checkout_id.as_str(),
                canonical,
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

    /// Releases exactly the lease generation the caller observed.
    ///
    /// Addressing a Session alone is not fencing: after a handoff, a stale caller
    /// could release the new owner's lease. Both the immutable lease id and its
    /// monotonic checkout generation must match.
    pub fn release_write_lease(&self, id: &LeaseId, generation: u64, now_ms: i64) -> Result<bool> {
        let tx = Transaction::new_unchecked(self.conn, TransactionBehavior::Immediate)?;
        let changed = tx.execute(
            "UPDATE workspace_write_leases SET state = 'released', released_ms = ?3, \
                    heartbeat_ms = ?3 \
             WHERE id = ?1 AND generation = ?2 AND state != 'released'",
            params![id.as_str(), generation as i64, now_ms],
        )?;
        tx.commit()?;
        Ok(changed == 1)
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
    use crate::{testing, Store};
    use std::path::Path;
    use std::sync::{Arc, Barrier};
    use std::thread;
    use turn_core::model::{HierarchyNodeKind, ProcessNode, Session, Workspace};

    const T0: i64 = 1_700_000_000_000;

    fn saved_workspace_at(store: &Store, name: &str, root: &Path) -> Workspace {
        let workspace = Workspace::new(name, root.to_string_lossy(), T0);
        store.workspaces().save(&workspace).unwrap();
        workspace
    }

    fn saved_alias_pair(store: &Store, root: &Path) -> [(Workspace, Session, CheckoutId); 2] {
        ["first", "second"].map(|name| {
            let workspace = saved_workspace_at(store, name, root);
            let session = testing::saved_session(store, &workspace.id, name);
            let checkout = CheckoutId::primary_for(&workspace.id);
            (workspace, session, checkout)
        })
    }

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
            .release_write_lease(&lease.id, lease.generation, T0 + 2)
            .unwrap());
        let second_lease = store
            .hierarchy()
            .acquire_write_lease(&workspace.id, &second.id, &checkout, T0 + 3)
            .unwrap();
        assert!(second_lease.generation > lease.generation);
    }

    #[test]
    fn a_stale_generation_cannot_release_a_newer_lease() {
        let store = testing::store();
        let workspace = testing::saved_workspace(&store, "fenced-release");
        let first = testing::saved_session(&store, &workspace.id, "first");
        let second = testing::saved_session(&store, &workspace.id, "second");
        let checkout = CheckoutId::primary_for(&workspace.id);

        let old = store
            .hierarchy()
            .acquire_write_lease(&workspace.id, &first.id, &checkout, T0)
            .unwrap();
        assert!(!store
            .hierarchy()
            .release_write_lease(&old.id, old.generation + 1, T0 + 1)
            .unwrap());
        assert_eq!(
            store
                .hierarchy()
                .active_lease(&workspace.id)
                .unwrap()
                .unwrap()
                .id,
            old.id
        );

        assert!(store
            .hierarchy()
            .release_write_lease(&old.id, old.generation, T0 + 2)
            .unwrap());
        let current = store
            .hierarchy()
            .acquire_write_lease(&workspace.id, &second.id, &checkout, T0 + 3)
            .unwrap();
        assert!(current.generation > old.generation);

        assert!(!store
            .hierarchy()
            .release_write_lease(&current.id, old.generation, T0 + 4)
            .unwrap());
        assert_eq!(
            store
                .hierarchy()
                .active_lease(&workspace.id)
                .unwrap()
                .unwrap()
                .id,
            current.id,
            "a stale fence must leave the current owner's claim intact"
        );
    }

    #[test]
    fn lease_ownership_rejects_cross_workspace_session_and_checkout_ids() {
        let store = testing::store();
        let a = testing::saved_workspace(&store, "ownership-a");
        let b = testing::saved_workspace(&store, "ownership-b");
        let session_a = testing::saved_session(&store, &a.id, "session-a");
        let session_b = testing::saved_session(&store, &b.id, "session-b");
        let checkout_a = CheckoutId::primary_for(&a.id);
        let checkout_b = CheckoutId::primary_for(&b.id);

        for error in [
            store
                .hierarchy()
                .acquire_write_lease(&a.id, &session_b.id, &checkout_a, T0)
                .unwrap_err(),
            store
                .hierarchy()
                .acquire_write_lease(&a.id, &session_a.id, &checkout_b, T0)
                .unwrap_err(),
        ] {
            assert!(matches!(error, StoreError::InvalidLeaseOwnership { .. }));
        }

        let count: i64 = store
            .connection()
            .query_row("SELECT COUNT(*) FROM workspace_write_leases", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(
            count, 0,
            "a rejected ownership tuple writes no partial lease"
        );
    }

    #[test]
    fn concurrent_aliasing_workspaces_have_one_global_winner() {
        #[derive(Debug)]
        enum Outcome {
            Acquired(u64),
            Held,
        }

        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("turn.db");
        let checkout_root = temp.path().join("same-checkout");
        std::fs::create_dir(&checkout_root).unwrap();

        let setup = Store::open_at(&database).unwrap();
        let [first, second] = saved_alias_pair(&setup, &checkout_root);
        drop(setup);

        let stores = [
            Store::open_at(&database).unwrap(),
            Store::open_at(&database).unwrap(),
        ];
        let barrier = Arc::new(Barrier::new(2));
        let contenders = [first, second]
            .into_iter()
            .zip(stores)
            .map(|((workspace, session, checkout), store)| {
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    match store.hierarchy().acquire_write_lease(
                        &workspace.id,
                        &session.id,
                        &checkout,
                        T0,
                    ) {
                        Ok(lease) => Outcome::Acquired(lease.generation),
                        Err(StoreError::WriteLeaseHeld { .. }) => Outcome::Held,
                        Err(error) => panic!("unexpected contender failure: {error}"),
                    }
                })
            })
            .collect::<Vec<_>>();

        let outcomes: Vec<Outcome> = contenders
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(outcome, Outcome::Acquired(_)))
                .count(),
            1,
            "exactly one canonical checkout writer: {outcomes:?}"
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(outcome, Outcome::Held))
                .count(),
            1,
            "the loser receives the current owner, not a SQLite race: {outcomes:?}"
        );
        assert!(outcomes
            .iter()
            .any(|outcome| matches!(outcome, Outcome::Acquired(generation) if *generation == 1)));

        let check = Store::open_at(&database).unwrap();
        let active: i64 = check
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM workspace_write_leases WHERE state != 'released'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(active, 1);
    }

    #[test]
    fn canonical_generation_survives_a_cross_workspace_handoff() {
        let temp = tempfile::tempdir().unwrap();
        let checkout_root = temp.path().join("same-checkout");
        std::fs::create_dir(&checkout_root).unwrap();
        let store = testing::store();
        let [first, second] = saved_alias_pair(&store, &checkout_root);

        let old = store
            .hierarchy()
            .acquire_write_lease(&first.0.id, &first.1.id, &first.2, T0)
            .unwrap();
        assert!(store
            .hierarchy()
            .release_write_lease(&old.id, old.generation, T0 + 1)
            .unwrap());
        let current = store
            .hierarchy()
            .acquire_write_lease(&second.0.id, &second.1.id, &second.2, T0 + 2)
            .unwrap();
        assert_eq!(current.generation, old.generation + 1);
    }

    #[test]
    fn the_global_unique_index_is_the_final_claim_arbiter() {
        let temp = tempfile::tempdir().unwrap();
        let checkout_root = temp.path().join("same-checkout");
        std::fs::create_dir(&checkout_root).unwrap();
        let store = testing::store();
        let [first, second] = saved_alias_pair(&store, &checkout_root);

        store
            .hierarchy()
            .acquire_write_lease(&first.0.id, &first.1.id, &first.2, T0)
            .unwrap();
        let canonical = store
            .hierarchy()
            .primary_checkout(&second.0.id)
            .unwrap()
            .unwrap()
            .canonical_path;
        let competing_id = LeaseId::new();
        let error = store
            .connection()
            .execute(
                "INSERT INTO workspace_write_leases \
                     (id, workspace_id, session_id, checkout_id, canonical_path, mode, state, \
                      acquired_ms, heartbeat_ms, released_ms, generation) \
                 VALUES (?1, ?2, ?3, ?4, ?5, 'exclusive_write', 'active', ?6, ?6, NULL, 2)",
                params![
                    competing_id.as_str(),
                    second.0.id.as_str(),
                    second.1.id.as_str(),
                    second.2.as_str(),
                    canonical,
                    T0 + 1,
                ],
            )
            .unwrap_err();
        assert!(matches!(
            error,
            rusqlite::Error::SqliteFailure(code, _)
                if code.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
        ));
    }

    #[test]
    fn the_schema_allows_only_one_primary_checkout_per_workspace() {
        let store = testing::store();
        let workspace = testing::saved_workspace(&store, "one-primary");
        let other_path = "/repos/one-primary-other";
        store
            .connection()
            .execute(
                "INSERT INTO checkout_write_fences (canonical_path, generation) VALUES (?1, 0)",
                params![other_path],
            )
            .unwrap();
        let error = store
            .connection()
            .execute(
                "INSERT INTO workspace_checkouts \
                     (id, workspace_id, path, canonical_path, branch, is_primary, \
                      shared_resources_json, created_ms) \
                 VALUES (?1, ?2, ?3, ?3, NULL, 1, '[]', ?4)",
                params![
                    CheckoutId::new().as_str(),
                    workspace.id.as_str(),
                    other_path,
                    T0
                ],
            )
            .unwrap_err();
        assert!(matches!(
            error,
            rusqlite::Error::SqliteFailure(code, _)
                if code.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
        ));
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
