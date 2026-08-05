//! ADR-040 persistence: checkout leases, view bindings, previews and per-surface tree state.

use crate::codec::{from_tag, json, tag};
use crate::error::{Result, StoreError};
use crate::repo::session::SessionRepo;
use rusqlite::{params, Connection, OptionalExtension, Row, Transaction, TransactionBehavior};
use std::path::{Path, PathBuf};
use turn_core::ids::{CheckoutId, LeaseId, NodeId, PaneId, SessionId, WorkspaceId};
use turn_core::model::{
    ActivityPreview, HierarchyNodeKind, LeaseState, PaneNodeBinding, Session, SessionMode,
    TreeUiState, WorkspaceCheckout, WorkspaceWriteLease,
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

    /// Returns one checkout only when it belongs to the requested Workspace.
    pub fn checkout(
        &self,
        workspace: &WorkspaceId,
        checkout: &CheckoutId,
    ) -> Result<Option<WorkspaceCheckout>> {
        self.conn
            .query_row(
                "SELECT id, workspace_id, path, canonical_path, branch, is_primary, \
                        shared_resources_json, created_ms \
                 FROM workspace_checkouts WHERE workspace_id = ?1 AND id = ?2",
                params![workspace.as_str(), checkout.as_str()],
                checkout_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    /// All roots known to a Workspace, with the primary checkout first.
    pub fn checkouts_for_workspace(
        &self,
        workspace: &WorkspaceId,
    ) -> Result<Vec<WorkspaceCheckout>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, workspace_id, path, canonical_path, branch, is_primary, \
                    shared_resources_json, created_ms \
             FROM workspace_checkouts WHERE workspace_id = ?1 \
             ORDER BY is_primary DESC, created_ms, id",
        )?;
        let rows = stmt.query_map(params![workspace.as_str()], checkout_from_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
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
        let lease = Self::acquire_in(&tx, workspace, session, checkout, now_ms)?;
        tx.commit()?;
        Ok(lease)
    }

    /// Proves that a Main Checkout Session still owns an active lease and that
    /// its checkout still resolves to the filesystem identity that was fenced.
    ///
    /// This is the final process-launch check. It deliberately cannot acquire a
    /// missing lease: adding a pane or relaunching an agent is not an ownership
    /// transition and must fail closed if authority was released or recovered.
    pub fn verify_active_write_lease(
        &self,
        workspace: &WorkspaceId,
        session: &SessionId,
        checkout: &CheckoutId,
    ) -> Result<WorkspaceWriteLease> {
        let tx = Transaction::new_unchecked(self.conn, TransactionBehavior::Immediate)?;
        let (canonical, reconciliation_required) =
            Self::verified_checkout_identity_in(&tx, workspace, session, checkout)?;
        if reconciliation_required {
            return Err(StoreError::LeaseReconciliationRequired {
                workspace_id: workspace.to_string(),
                checkout_id: checkout.to_string(),
            });
        }

        let held = Self::unreleased_lease_for_canonical_in(&tx, &canonical)?;
        let Some(held) = held else {
            return Err(StoreError::WriteLeaseNotActive {
                workspace_id: workspace.to_string(),
                session_id: session.to_string(),
                checkout_id: checkout.to_string(),
            });
        };
        if held.workspace_id != *workspace
            || held.session_id != *session
            || held.checkout_id != *checkout
        {
            return Err(StoreError::WriteLeaseHeld {
                checkout_id: held.checkout_id.to_string(),
                owner_session_id: held.session_id.to_string(),
                lease_id: held.id.to_string(),
            });
        }
        if held.state != LeaseState::Active {
            return Err(StoreError::LeaseReconciliationRequired {
                workspace_id: workspace.to_string(),
                checkout_id: checkout.to_string(),
            });
        }
        tx.commit()?;
        Ok(held)
    }

    /// Persists a new Session and arbitrates its checkout in one `BEGIN
    /// IMMEDIATE` transaction. No init command or PTY may be started until this
    /// succeeds; a lease conflict therefore leaves no half-created Session row.
    pub fn create_session(
        &self,
        session: &Session,
        now_ms: i64,
    ) -> Result<Option<WorkspaceWriteLease>> {
        let tx = Transaction::new_unchecked(self.conn, TransactionBehavior::Immediate)?;
        let lease = match session.mode {
            SessionMode::MainCheckout => {
                SessionRepo::save_in_transaction(&tx, session)?;
                Some(Self::acquire_in(
                    &tx,
                    &session.workspace_id,
                    &session.id,
                    &session.checkout_id,
                    now_ms,
                )?)
            }
            // Compatibility for callers that do not need to report technical
            // enforcement. The explicit API below is required to persist `true`.
            SessionMode::ReadOnly => {
                Self::create_read_only_in(&tx, session, false)?;
                None
            }
            SessionMode::IsolatedWorktree => {
                return Err(StoreError::InvalidSessionCreation {
                    session_id: session.id.to_string(),
                    reason: "an isolated worktree must be registered atomically with create_worktree_session"
                        .into(),
                });
            }
        };
        tx.commit()?;
        Ok(lease)
    }

    /// Persists a new review/research Session against this Workspace's primary
    /// checkout without acquiring a write lease.
    ///
    /// `read_only_enforced` is an explicit fact supplied by the launcher after it
    /// installs a technical guard. The field on `session` is deliberately ignored
    /// so this method defaults to honest `false` unless its caller opts in here.
    pub fn create_read_only_session(
        &self,
        session: &Session,
        read_only_enforced: bool,
    ) -> Result<()> {
        let tx = Transaction::new_unchecked(self.conn, TransactionBehavior::Immediate)?;
        Self::create_read_only_in(&tx, session, read_only_enforced)?;
        tx.commit()?;
        Ok(())
    }

    fn create_read_only_in(
        tx: &Transaction<'_>,
        session: &Session,
        read_only_enforced: bool,
    ) -> Result<()> {
        Self::validate_id("Session", session.id.as_str(), SessionId::PREFIX)?;
        Self::validate_id(
            "Workspace",
            session.workspace_id.as_str(),
            WorkspaceId::PREFIX,
        )?;
        if session.mode != SessionMode::ReadOnly {
            return Err(Self::invalid_session(
                session,
                "create_read_only_session requires mode read_only",
            ));
        }
        if session.worktree_path.is_some() {
            return Err(Self::invalid_session(
                session,
                "a read-only Session cannot carry a worktree path",
            ));
        }
        Self::ensure_new_session(tx, session)?;

        let primary: Option<String> = tx
            .query_row(
                "SELECT id FROM workspace_checkouts \
                 WHERE workspace_id = ?1 AND is_primary = 1",
                params![session.workspace_id.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        let Some(primary) = primary else {
            return Err(StoreError::InvalidCheckout {
                checkout_id: session.checkout_id.to_string(),
                reason: format!(
                    "workspace {} has no registered primary checkout",
                    session.workspace_id
                ),
            });
        };
        if primary != session.checkout_id.as_str() {
            return Err(StoreError::InvalidCheckout {
                checkout_id: session.checkout_id.to_string(),
                reason: format!(
                    "read-only Session {} must reference Workspace {} primary checkout {primary}",
                    session.id, session.workspace_id
                ),
            });
        }

        let mut stored = session.clone();
        stored.read_only_enforced = read_only_enforced;
        SessionRepo::save_in_transaction(tx, &stored)
    }

    /// Registers an existing, independent Git worktree and its Session as one
    /// transaction. This never acquires the primary checkout lease.
    ///
    /// The filesystem is resolved before SQLite is touched. Once the transaction
    /// begins, the checkout fence, checkout metadata, Session, Layout, and nodes
    /// either all commit or all roll back.
    pub fn create_worktree_session(
        &self,
        session: &Session,
        checkout: &WorkspaceCheckout,
    ) -> Result<()> {
        let canonical = Self::validate_worktree_shape(session, checkout)?;
        let tx = Transaction::new_unchecked(self.conn, TransactionBehavior::Immediate)?;
        Self::ensure_new_session(&tx, session)?;

        let primary: Option<(String, String)> = tx
            .query_row(
                "SELECT id, canonical_path FROM workspace_checkouts \
                 WHERE workspace_id = ?1 AND is_primary = 1",
                params![session.workspace_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((_primary_id, primary_canonical)) = primary else {
            return Err(StoreError::InvalidCheckout {
                checkout_id: checkout.id.to_string(),
                reason: format!(
                    "workspace {} has no registered primary checkout",
                    session.workspace_id
                ),
            });
        };
        if canonical == primary_canonical {
            return Err(StoreError::InvalidCheckout {
                checkout_id: checkout.id.to_string(),
                reason: "isolated worktree resolves to the primary checkout".into(),
            });
        }

        let existing: Option<String> = tx
            .query_row(
                "SELECT id FROM workspace_checkouts WHERE canonical_path = ?1 LIMIT 1",
                params![&canonical],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(existing_checkout_id) = existing {
            return Err(StoreError::CheckoutPathConflict {
                canonical_path: canonical,
                existing_checkout_id,
            });
        }
        let reused_id: Option<String> = tx
            .query_row(
                "SELECT canonical_path FROM workspace_checkouts WHERE id = ?1",
                params![checkout.id.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(existing_path) = reused_id {
            return Err(StoreError::InvalidCheckout {
                checkout_id: checkout.id.to_string(),
                reason: format!("checkout id is already registered for {existing_path}"),
            });
        }

        tx.execute(
            "INSERT INTO checkout_write_fences (canonical_path, generation) \
             VALUES (?1, 0) ON CONFLICT(canonical_path) DO NOTHING",
            params![&canonical],
        )?;
        tx.execute(
            "INSERT INTO workspace_checkouts \
                 (id, workspace_id, path, canonical_path, branch, is_primary, \
                  shared_resources_json, created_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6, ?7)",
            params![
                checkout.id.as_str(),
                checkout.workspace_id.as_str(),
                checkout.path,
                canonical,
                checkout.branch,
                json("checkout shared resources", &checkout.shared_resources)?,
                checkout.created_ms,
            ],
        )
        .map_err(|error| {
            StoreError::from_write("worktree checkout", checkout.workspace_id.as_str(), error)
        })?;

        let mut stored = session.clone();
        stored.read_only_enforced = false;
        SessionRepo::save_in_transaction(&tx, &stored)?;
        tx.commit()?;
        Ok(())
    }

    fn validate_worktree_shape(session: &Session, checkout: &WorkspaceCheckout) -> Result<String> {
        Self::validate_id("Session", session.id.as_str(), SessionId::PREFIX)?;
        Self::validate_id(
            "Workspace",
            session.workspace_id.as_str(),
            WorkspaceId::PREFIX,
        )?;
        Self::validate_id("Checkout", checkout.id.as_str(), CheckoutId::PREFIX)?;
        if session.mode != SessionMode::IsolatedWorktree {
            return Err(Self::invalid_session(
                session,
                "create_worktree_session requires mode isolated_worktree",
            ));
        }
        if session.read_only_enforced {
            return Err(Self::invalid_session(
                session,
                "read_only_enforced is only valid for a read-only Session",
            ));
        }
        if checkout.primary {
            return Err(StoreError::InvalidCheckout {
                checkout_id: checkout.id.to_string(),
                reason: "an isolated worktree cannot be marked primary".into(),
            });
        }
        if checkout.id == CheckoutId::primary_for(&session.workspace_id) {
            return Err(StoreError::InvalidCheckout {
                checkout_id: checkout.id.to_string(),
                reason: "an isolated worktree cannot reuse the primary checkout id".into(),
            });
        }
        if checkout.workspace_id != session.workspace_id {
            return Err(StoreError::InvalidCheckout {
                checkout_id: checkout.id.to_string(),
                reason: format!(
                    "checkout belongs to Workspace {}, not {}",
                    checkout.workspace_id, session.workspace_id
                ),
            });
        }
        if checkout.id != session.checkout_id {
            return Err(StoreError::InvalidCheckout {
                checkout_id: checkout.id.to_string(),
                reason: format!("Session {} references {}", session.id, session.checkout_id),
            });
        }
        if session.worktree_path.as_deref() != Some(checkout.path.as_str()) {
            return Err(Self::invalid_session(
                session,
                "worktree_path must exactly match the registered checkout path",
            ));
        }
        let branch = checkout
            .branch
            .as_deref()
            .filter(|branch| !branch.trim().is_empty())
            .ok_or_else(|| StoreError::InvalidCheckout {
                checkout_id: checkout.id.to_string(),
                reason: "an isolated worktree requires a branch".into(),
            })?;
        if session.git_branch.as_deref() != Some(branch) {
            return Err(Self::invalid_session(
                session,
                "Session branch must match its worktree checkout branch",
            ));
        }

        let canonical_path = Self::canonicalize_checkout(&checkout.path)?;
        let supplied_canonical = PathBuf::from(&checkout.canonical_path);
        if supplied_canonical != canonical_path {
            return Err(StoreError::InvalidCheckout {
                checkout_id: checkout.id.to_string(),
                reason: format!(
                    "canonical_path {} does not match resolved path {}",
                    checkout.canonical_path,
                    canonical_path.display()
                ),
            });
        }
        let cwd = Self::canonicalize_checkout(&session.cwd)?;
        if !cwd.starts_with(&canonical_path) {
            return Err(Self::invalid_session(
                session,
                "cwd must be inside the isolated worktree",
            ));
        }
        Ok(canonical_path.to_string_lossy().into_owned())
    }

    fn canonicalize_checkout(path: &str) -> Result<PathBuf> {
        let resolved =
            std::fs::canonicalize(Path::new(path)).map_err(|cause| StoreError::CheckoutPath {
                path: path.to_owned(),
                cause,
            })?;
        let metadata = std::fs::metadata(&resolved).map_err(|cause| StoreError::CheckoutPath {
            path: path.to_owned(),
            cause,
        })?;
        if !metadata.is_dir() {
            return Err(StoreError::InvalidCheckout {
                checkout_id: "<unregistered>".into(),
                reason: format!("{} is not a directory", resolved.display()),
            });
        }
        Ok(resolved)
    }

    fn ensure_new_session(tx: &Transaction<'_>, session: &Session) -> Result<()> {
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM sessions WHERE id = ?1)",
            params![session.id.as_str()],
            |row| row.get(0),
        )?;
        if exists {
            return Err(Self::invalid_session(
                session,
                "a Session with this id already exists",
            ));
        }
        Ok(())
    }

    fn validate_id(what: &'static str, value: &str, prefix: &str) -> Result<()> {
        let expected = format!("{prefix}_");
        if !value.starts_with(&expected) || value.len() == expected.len() {
            return Err(StoreError::InvalidSessionCreation {
                session_id: value.to_owned(),
                reason: format!("{what} id must start with {expected} and have a suffix"),
            });
        }
        Ok(())
    }

    fn invalid_session(session: &Session, reason: impl Into<String>) -> StoreError {
        StoreError::InvalidSessionCreation {
            session_id: session.id.to_string(),
            reason: reason.into(),
        }
    }

    fn acquire_in(
        tx: &Transaction<'_>,
        workspace: &WorkspaceId,
        session: &SessionId,
        checkout: &CheckoutId,
        now_ms: i64,
    ) -> Result<WorkspaceWriteLease> {
        let (canonical, reconciliation_required) =
            Self::verified_checkout_identity_in(tx, workspace, session, checkout)?;
        if reconciliation_required {
            // Preserve the typed owner context for a migrated live/recovery
            // claim. The UI can still offer "Focus existing session" while a
            // Workspace with no recoverable owner gets a typed safety refusal.
            let held = tx
                .query_row(
                    "SELECT id, workspace_id, session_id, checkout_id, mode, state, \
                            acquired_ms, heartbeat_ms, released_ms, generation \
                     FROM workspace_write_leases \
                     WHERE workspace_id = ?1 AND checkout_id = ?2 \
                       AND state != 'released' LIMIT 1",
                    params![workspace.as_str(), checkout.as_str()],
                    lease_from_row,
                )
                .optional()?;
            if let Some(held) = held {
                return Err(StoreError::WriteLeaseHeld {
                    checkout_id: held.checkout_id.to_string(),
                    owner_session_id: held.session_id.to_string(),
                    lease_id: held.id.to_string(),
                });
            }
            return Err(StoreError::LeaseReconciliationRequired {
                workspace_id: workspace.to_string(),
                checkout_id: checkout.to_string(),
            });
        }

        let held = Self::unreleased_lease_for_canonical_in(tx, &canonical)?;
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
        Ok(lease)
    }

    fn verified_checkout_identity_in(
        tx: &Transaction<'_>,
        workspace: &WorkspaceId,
        session: &SessionId,
        checkout: &CheckoutId,
    ) -> Result<(String, bool)> {
        let identity: Option<(String, bool, String, String)> = tx
            .query_row(
                "SELECT w.root, w.lease_reconciliation_required, c.path, c.canonical_path \
                 FROM sessions s \
                 JOIN workspaces w ON w.id = s.workspace_id \
                 JOIN workspace_checkouts c ON c.workspace_id = s.workspace_id \
                 WHERE s.id = ?1 AND s.workspace_id = ?2 \
                   AND c.id = ?3 AND c.workspace_id = ?2 \
                   AND s.checkout_id = c.id AND c.is_primary = 1",
                params![session.as_str(), workspace.as_str(), checkout.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        let Some((root, reconciliation_required, checkout_path, stored_canonical)) = identity
        else {
            return Err(StoreError::InvalidLeaseOwnership {
                workspace_id: workspace.to_string(),
                session_id: session.to_string(),
                checkout_id: checkout.to_string(),
            });
        };
        if reconciliation_required {
            return Ok((stored_canonical, true));
        }

        let root_identity = crate::repo::workspace::canonical_workspace_root(&root)?;
        let checkout_identity = Self::canonicalize_checkout(&checkout_path)?;
        let canonical = checkout_identity.to_string_lossy().into_owned();
        if root_identity != checkout_identity || stored_canonical != canonical {
            return Ok((canonical, true));
        }
        Ok((canonical, false))
    }

    fn unreleased_lease_for_canonical_in(
        tx: &Transaction<'_>,
        canonical: &str,
    ) -> Result<Option<WorkspaceWriteLease>> {
        tx.query_row(
            "SELECT l.id, l.workspace_id, l.session_id, l.checkout_id, l.mode, l.state, \
                    l.acquired_ms, l.heartbeat_ms, l.released_ms, l.generation \
             FROM workspace_write_leases l \
             WHERE l.canonical_path = ?1 AND l.state != 'released' LIMIT 1",
            params![canonical],
            lease_from_row,
        )
        .optional()
        .map_err(Into::into)
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

    /// Looks up the exact fenced lease named by a conflict. Unlike
    /// [`HierarchyRepo::active_lease`], this crosses Workspace aliases that resolve to
    /// the same canonical checkout; the lease id is globally unique.
    pub fn lease(&self, id: &LeaseId) -> Result<Option<WorkspaceWriteLease>> {
        self.conn
            .query_row(
                "SELECT id, workspace_id, session_id, checkout_id, mode, state, acquired_ms, \
                        heartbeat_ms, released_ms, generation \
                 FROM workspace_write_leases WHERE id = ?1 LIMIT 1",
                params![id.as_str()],
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

    /// Fences every lease inherited from an earlier daemon generation.
    ///
    /// No live lease may be adopted merely because its owning Session was loaded
    /// from SQLite. Keeping the old heartbeat is intentional: it records the last
    /// proof emitted by the previous daemon and must not be confused with proof
    /// from this one. Released rows are historical and remain untouched.
    pub fn require_recovery_after_daemon_restart(&self) -> Result<usize> {
        self.conn
            .execute(
                "UPDATE workspace_write_leases SET state = 'recovery_required' \
                 WHERE state != 'released'",
                [],
            )
            .map_err(Into::into)
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
    use turn_core::event::Confidence;
    use turn_core::model::{
        ActivityPreview, HierarchyNodeKind, Layout, Pane, PaneKind, PreviewSource, ProcessNode,
        Session, SessionMode, Workspace,
    };

    const T0: i64 = 1_700_000_000_000;

    fn saved_workspace_at(store: &Store, name: &str, root: &Path) -> Workspace {
        let workspace = Workspace::new(name, root.to_string_lossy(), T0);
        store.workspaces().save(&workspace).unwrap();
        workspace
    }

    fn worktree_pair(
        workspace: &Workspace,
        path: &Path,
        name: &str,
    ) -> (Session, WorkspaceCheckout) {
        let checkout_id = CheckoutId::new();
        let branch = format!("turn/{name}");
        let path = path.to_string_lossy().into_owned();
        let canonical_path = std::fs::canonicalize(&path)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let checkout = WorkspaceCheckout {
            id: checkout_id.clone(),
            workspace_id: workspace.id.clone(),
            path: path.clone(),
            canonical_path,
            branch: Some(branch.clone()),
            primary: false,
            shared_resources: vec!["docker".into()],
            created_ms: T0,
        };
        let mut session = Session::new(
            workspace.id.clone(),
            name,
            path.clone(),
            Layout::single(Pane::new(PaneKind::Agent).with_command("claude")),
            T0,
        );
        session.mode = SessionMode::IsolatedWorktree;
        session.checkout_id = checkout_id;
        session.worktree_path = Some(path);
        session.git_branch = Some(branch);
        (session, checkout)
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
    fn reconciliation_blocks_acquisition_and_is_never_cleared_as_a_side_effect() {
        let store = testing::store();
        let workspace = testing::saved_workspace(&store, "legacy-lease");
        let session = testing::saved_session(&store, &workspace.id, "writer");
        let checkout = CheckoutId::primary_for(&workspace.id);
        store
            .connection()
            .execute(
                "UPDATE workspaces SET lease_reconciliation_required = 1 WHERE id = ?1",
                params![workspace.id.as_str()],
            )
            .unwrap();

        let error = store
            .hierarchy()
            .acquire_write_lease(&workspace.id, &session.id, &checkout, T0)
            .unwrap_err();
        assert!(matches!(
            error,
            StoreError::LeaseReconciliationRequired { .. }
        ));
        let (required, leases): (bool, i64) = store
            .connection()
            .query_row(
                "SELECT w.lease_reconciliation_required, \
                        (SELECT COUNT(*) FROM workspace_write_leases) \
                 FROM workspaces w WHERE w.id = ?1",
                params![workspace.id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert!(required, "acquisition must not perform reconciliation");
        assert_eq!(leases, 0, "the refusal leaves no partial claim");
    }

    #[test]
    fn launch_verification_requires_active_state_and_never_reacquires() {
        let store = testing::store();
        let workspace = testing::saved_workspace(&store, "launch-authority");
        let session = testing::saved_session(&store, &workspace.id, "writer");
        let checkout = CheckoutId::primary_for(&workspace.id);
        let lease = store
            .hierarchy()
            .acquire_write_lease(&workspace.id, &session.id, &checkout, T0)
            .unwrap();
        assert_eq!(
            store
                .hierarchy()
                .verify_active_write_lease(&workspace.id, &session.id, &checkout)
                .unwrap()
                .id,
            lease.id
        );

        store
            .connection()
            .execute(
                "UPDATE workspace_write_leases SET state = 'recovery_required' WHERE id = ?1",
                params![lease.id.as_str()],
            )
            .unwrap();
        let error = store
            .hierarchy()
            .verify_active_write_lease(&workspace.id, &session.id, &checkout)
            .expect_err("recovery is not live write authority");
        assert!(matches!(
            error,
            StoreError::LeaseReconciliationRequired { .. }
        ));
        let count: i64 = store
            .connection()
            .query_row("SELECT COUNT(*) FROM workspace_write_leases", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 1, "verification must not mint a replacement lease");
    }

    #[test]
    fn daemon_restart_fences_every_unreleased_lease_without_forging_a_heartbeat() {
        let store = testing::store();
        let mut leases = Vec::new();
        for (name, state, heartbeat) in [
            ("active", LeaseState::Active, T0 + 10),
            ("stale", LeaseState::Stale, T0 + 20),
            ("recovery", LeaseState::RecoveryRequired, T0 + 30),
            ("released", LeaseState::Released, T0 + 40),
        ] {
            let workspace = testing::saved_workspace(&store, name);
            let session = testing::saved_session(&store, &workspace.id, name);
            let checkout = CheckoutId::primary_for(&workspace.id);
            let lease = store
                .hierarchy()
                .acquire_write_lease(&workspace.id, &session.id, &checkout, T0)
                .unwrap();
            store
                .connection()
                .execute(
                    "UPDATE workspace_write_leases SET state = ?2, heartbeat_ms = ?3, \
                     released_ms = CASE WHEN ?2 = 'released' THEN ?3 ELSE NULL END \
                     WHERE id = ?1",
                    params![
                        lease.id.as_str(),
                        tag("lease state", &state).unwrap(),
                        heartbeat
                    ],
                )
                .unwrap();
            leases.push((lease, state, heartbeat));
        }

        assert_eq!(
            store
                .hierarchy()
                .require_recovery_after_daemon_restart()
                .unwrap(),
            3
        );

        for (before, prior_state, heartbeat) in leases {
            let after = store
                .hierarchy()
                .lease(&before.id)
                .unwrap()
                .expect("the historical lease row");
            let expected = if prior_state == LeaseState::Released {
                LeaseState::Released
            } else {
                LeaseState::RecoveryRequired
            };
            assert_eq!(after.state, expected);
            assert_eq!(after.heartbeat_ms, heartbeat, "restart is not a heartbeat");
            assert_eq!(after.id, before.id);
            assert_eq!(after.generation, before.generation);
        }
    }

    #[test]
    fn preview_history_is_newest_first_and_applies_the_limit_to_the_newest_entries() {
        let store = testing::store();
        let workspace = testing::saved_workspace(&store, "previews");
        let session = testing::saved_session(&store, &workspace.id, "history");
        let mut node = ProcessNode::agent(session.id.clone(), "claude", &session.cwd, T0);

        for sequence in 1..=6_u64 {
            node.activity_preview = Some(ActivityPreview {
                node_id: node.id.clone(),
                raw_source_sequence: Some(sequence),
                normalized_text: format!("preview {sequence}"),
                source: PreviewSource::SemanticEvent,
                confidence: Confidence::Explicit,
                stable: true,
                contains_sensitive_data: false,
                redacted: false,
                updated_ms: T0 + sequence as i64,
            });
            store.nodes().upsert(&node).unwrap();
        }

        let texts: Vec<_> = store
            .hierarchy()
            .preview_history(&node.id, 4)
            .unwrap()
            .into_iter()
            .map(|preview| preview.normalized_text)
            .collect();
        assert_eq!(texts, ["preview 6", "preview 5", "preview 4", "preview 3"]);
    }

    #[test]
    fn a_recovery_owner_remains_a_typed_conflict_for_another_writer() {
        let store = testing::store();
        let workspace = testing::saved_workspace(&store, "recovery-owner");
        let owner = testing::saved_session(&store, &workspace.id, "owner");
        let contender = testing::saved_session(&store, &workspace.id, "contender");
        let checkout = CheckoutId::primary_for(&workspace.id);
        let lease = store
            .hierarchy()
            .acquire_write_lease(&workspace.id, &owner.id, &checkout, T0)
            .unwrap();
        store
            .connection()
            .execute(
                "UPDATE workspace_write_leases SET state = 'recovery_required' WHERE id = ?1",
                params![lease.id.as_str()],
            )
            .unwrap();
        store
            .connection()
            .execute(
                "UPDATE workspaces SET lease_reconciliation_required = 1 WHERE id = ?1",
                params![workspace.id.as_str()],
            )
            .unwrap();

        let error = store
            .hierarchy()
            .acquire_write_lease(&workspace.id, &contender.id, &checkout, T0 + 1)
            .expect_err("the recovery owner must remain visible to conflict UX");
        assert!(matches!(
            error,
            StoreError::WriteLeaseHeld {
                owner_session_id,
                lease_id,
                ..
            } if owner_session_id == owner.id.as_str() && lease_id == lease.id.as_str()
        ));
    }

    #[test]
    fn a_checkout_identity_drift_requires_reconciliation_before_writing() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        let other = temp.path().join("other");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&other).unwrap();
        let store = testing::store();
        let workspace = saved_workspace_at(&store, "drift", &root);
        let session = testing::saved_session(&store, &workspace.id, "writer");
        let checkout = CheckoutId::primary_for(&workspace.id);
        let other = std::fs::canonicalize(other)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        store
            .connection()
            .execute(
                "INSERT INTO checkout_write_fences (canonical_path, generation) VALUES (?1, 0)",
                params![&other],
            )
            .unwrap();
        let guarded = store
            .connection()
            .execute(
                "UPDATE workspace_checkouts SET path = ?2, canonical_path = ?2 \
                 WHERE workspace_id = ?1 AND is_primary = 1",
                params![workspace.id.as_str(), &other],
            )
            .unwrap_err();
        assert!(matches!(
            guarded,
            rusqlite::Error::SqliteFailure(code, _)
                if code.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_TRIGGER
        ));
        // Defence in depth: even a database whose guard was removed behind
        // Turn's back must fail closed at lease acquisition.
        store
            .connection()
            .execute_batch("DROP TRIGGER validate_safe_primary_checkout_identity_update")
            .unwrap();
        store
            .connection()
            .execute(
                "UPDATE workspace_checkouts SET path = ?2, canonical_path = ?2 \
                 WHERE workspace_id = ?1 AND is_primary = 1",
                params![workspace.id.as_str(), &other],
            )
            .unwrap();

        let error = store
            .hierarchy()
            .acquire_write_lease(&workspace.id, &session.id, &checkout, T0)
            .unwrap_err();
        assert!(matches!(
            error,
            StoreError::LeaseReconciliationRequired { .. }
        ));
        assert!(store
            .hierarchy()
            .active_lease(&workspace.id)
            .unwrap()
            .is_none());
    }

    #[test]
    fn creating_a_main_session_and_lease_is_atomic_on_conflict() {
        let store = testing::store();
        let workspace = testing::saved_workspace(&store, "atomic");
        let make = |name: &str| {
            let mut session = Session::new(
                workspace.id.clone(),
                name,
                workspace.root.clone(),
                Layout::single(Pane::new(PaneKind::Agent).with_command("claude")),
                T0,
            );
            session.mode = SessionMode::MainCheckout;
            session
        };
        let first = make("first writer");
        let second = make("second writer");

        let lease = store
            .hierarchy()
            .create_session(&first, T0)
            .unwrap()
            .expect("main checkout receives a lease");
        assert_eq!(lease.session_id, first.id);

        let error = store
            .hierarchy()
            .create_session(&second, T0 + 1)
            .expect_err("the second writer is refused");
        assert!(matches!(error, StoreError::WriteLeaseHeld { .. }));
        assert!(
            store.sessions().get(&second.id).unwrap().is_none(),
            "the rejected Session row must roll back with the lease attempt"
        );

        let mut review = make("review");
        review.mode = SessionMode::ReadOnly;
        assert!(store
            .hierarchy()
            .create_session(&review, T0 + 2)
            .unwrap()
            .is_none());
        assert_eq!(
            store.sessions().get(&review.id).unwrap().unwrap().mode,
            SessionMode::ReadOnly
        );
    }

    #[test]
    fn read_only_creation_uses_the_primary_without_claiming_its_lease() {
        let temp = tempfile::tempdir().unwrap();
        let primary = temp.path().join("primary");
        std::fs::create_dir(&primary).unwrap();
        let store = testing::store();
        let workspace = saved_workspace_at(&store, "read-only", &primary);

        let mut writer = Session::new(
            workspace.id.clone(),
            "writer",
            workspace.root.clone(),
            Layout::single(Pane::new(PaneKind::Agent).with_command("claude")),
            T0,
        );
        writer.mode = SessionMode::MainCheckout;
        let lease = store
            .hierarchy()
            .create_session(&writer, T0)
            .unwrap()
            .unwrap();

        let mut review = Session::new(
            workspace.id.clone(),
            "review",
            workspace.root.clone(),
            Layout::single(Pane::new(PaneKind::Shell)),
            T0 + 1,
        );
        // A stale/optimistic field on the object is not evidence that a launcher
        // installed a real guard. Only the explicit method argument is trusted.
        review.read_only_enforced = true;
        store
            .hierarchy()
            .create_read_only_session(&review, false)
            .unwrap();

        let stored = store.sessions().get(&review.id).unwrap().unwrap();
        assert_eq!(stored.mode, SessionMode::ReadOnly);
        assert_eq!(stored.checkout_id, CheckoutId::primary_for(&workspace.id));
        assert!(stored.worktree_path.is_none());
        assert!(!stored.read_only_enforced);
        assert_eq!(
            store
                .hierarchy()
                .active_lease(&workspace.id)
                .unwrap()
                .unwrap()
                .id,
            lease.id,
            "a reader must not disturb the primary writer's lease"
        );

        let guarded = Session::new(
            workspace.id.clone(),
            "guarded review",
            workspace.root.clone(),
            Layout::single(Pane::new(PaneKind::Shell)),
            T0 + 2,
        );
        store
            .hierarchy()
            .create_read_only_session(&guarded, true)
            .unwrap();
        assert!(
            store
                .sessions()
                .get(&guarded.id)
                .unwrap()
                .unwrap()
                .read_only_enforced,
            "true is persisted only when the launcher says the guard is active"
        );

        let lease_count: i64 = store
            .connection()
            .query_row("SELECT COUNT(*) FROM workspace_write_leases", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(lease_count, 1);
    }

    #[test]
    fn worktree_checkout_and_session_roll_back_together() {
        let temp = tempfile::tempdir().unwrap();
        let primary = temp.path().join("primary");
        let worktree = temp.path().join("worktree");
        std::fs::create_dir(&primary).unwrap();
        std::fs::create_dir(&worktree).unwrap();
        let store = testing::store();
        let workspace = saved_workspace_at(&store, "atomic-worktree", &primary);
        let (mut session, checkout) = worktree_pair(&workspace, &worktree, "alternative");
        session.parent_session = Some(SessionId::new());

        let error = store
            .hierarchy()
            .create_worktree_session(&session, &checkout)
            .expect_err("the unknown parent must fail after checkout insertion");
        assert!(matches!(error, StoreError::UnknownReference { .. }));
        assert!(store.sessions().get(&session.id).unwrap().is_none());
        let checkout_count: i64 = store
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM workspace_checkouts WHERE id = ?1",
                params![checkout.id.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(checkout_count, 0, "no orphan checkout may survive");
        let fence_count: i64 = store
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM checkout_write_fences WHERE canonical_path = ?1",
                params![checkout.canonical_path],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(fence_count, 0, "the failed checkout fence also rolls back");

        session.parent_session = None;
        store
            .hierarchy()
            .create_worktree_session(&session, &checkout)
            .unwrap();
        let stored = store.sessions().get(&session.id).unwrap().unwrap();
        assert_eq!(stored.mode, SessionMode::IsolatedWorktree);
        assert_eq!(stored.checkout_id, checkout.id);
        assert_eq!(
            stored.worktree_path.as_deref(),
            Some(checkout.path.as_str())
        );
        let worktree_leases: i64 = store
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM workspace_write_leases WHERE session_id = ?1",
                params![session.id.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            worktree_leases, 0,
            "worktrees do not claim the primary lease"
        );
    }

    #[test]
    fn worktree_creation_rejects_foreign_ownership_without_partial_rows() {
        let temp = tempfile::tempdir().unwrap();
        let primary_a = temp.path().join("primary-a");
        let primary_b = temp.path().join("primary-b");
        let worktree = temp.path().join("worktree");
        for path in [&primary_a, &primary_b, &worktree] {
            std::fs::create_dir(path).unwrap();
        }
        let store = testing::store();
        let a = saved_workspace_at(&store, "owner-a", &primary_a);
        let b = saved_workspace_at(&store, "owner-b", &primary_b);
        let (session, mut checkout) = worktree_pair(&a, &worktree, "foreign");
        checkout.workspace_id = b.id.clone();

        let error = store
            .hierarchy()
            .create_worktree_session(&session, &checkout)
            .unwrap_err();
        assert!(matches!(error, StoreError::InvalidCheckout { .. }));
        assert!(store.sessions().get(&session.id).unwrap().is_none());
        let checkout_count: i64 = store
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM workspace_checkouts WHERE id = ?1",
                params![checkout.id.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(checkout_count, 0);

        // The schema enforces the ownership relation even for a caller that
        // bypasses HierarchyRepo and writes through the generic SessionRepo.
        let mut foreign_reader = Session::new(
            a.id.clone(),
            "bad reader",
            a.root.clone(),
            Layout::single(Pane::new(PaneKind::Shell)),
            T0,
        );
        foreign_reader.checkout_id = CheckoutId::primary_for(&b.id);
        assert!(store.sessions().save(&foreign_reader).is_err());
        assert!(store.sessions().get(&foreign_reader.id).unwrap().is_none());
    }

    #[test]
    fn isolated_worktree_has_an_independent_canonical_path_and_no_primary_lease() {
        let temp = tempfile::tempdir().unwrap();
        let primary = temp.path().join("primary");
        let worktree = temp.path().join("worktree");
        std::fs::create_dir(&primary).unwrap();
        std::fs::create_dir(&worktree).unwrap();
        let store = testing::store();
        let workspace = saved_workspace_at(&store, "parallel", &primary);

        let mut writer = Session::new(
            workspace.id.clone(),
            "writer",
            workspace.root.clone(),
            Layout::single(Pane::new(PaneKind::Agent)),
            T0,
        );
        writer.mode = SessionMode::MainCheckout;
        let writer_lease = store
            .hierarchy()
            .create_session(&writer, T0)
            .unwrap()
            .unwrap();
        let (isolated, checkout) = worktree_pair(&workspace, &worktree, "parallel-fix");
        store
            .hierarchy()
            .create_worktree_session(&isolated, &checkout)
            .unwrap();

        let primary_checkout = store
            .hierarchy()
            .primary_checkout(&workspace.id)
            .unwrap()
            .unwrap();
        assert_ne!(checkout.canonical_path, primary_checkout.canonical_path);
        assert_eq!(
            store
                .hierarchy()
                .checkout(&workspace.id, &checkout.id)
                .unwrap()
                .unwrap(),
            checkout
        );
        assert_eq!(
            store
                .hierarchy()
                .checkouts_for_workspace(&workspace.id)
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            store
                .hierarchy()
                .active_lease(&workspace.id)
                .unwrap()
                .unwrap()
                .id,
            writer_lease.id
        );

        let alias = temp.path().join("worktree-alias");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&worktree, &alias).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&worktree, &alias).unwrap();
        let (alias_session, mut alias_checkout) =
            worktree_pair(&workspace, &alias, "same-directory");
        // worktree_pair has already resolved the symlink. The API must detect
        // the existing canonical checkout rather than trusting the new id/path.
        alias_checkout.canonical_path = checkout.canonical_path.clone();
        let error = store
            .hierarchy()
            .create_worktree_session(&alias_session, &alias_checkout)
            .unwrap_err();
        assert!(matches!(error, StoreError::CheckoutPathConflict { .. }));
        assert!(store.sessions().get(&alias_session.id).unwrap().is_none());
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
    fn a_session_cannot_acquire_a_lease_for_a_different_checkout() {
        let store = testing::store();
        let temp = tempfile::tempdir().unwrap();
        let primary = temp.path().join("primary");
        let worktree = temp.path().join("worktree");
        std::fs::create_dir_all(&primary).unwrap();
        std::fs::create_dir_all(&worktree).unwrap();
        let workspace = saved_workspace_at(&store, "checkout-binding", &primary);
        let (isolated, isolated_checkout) = worktree_pair(&workspace, &worktree, "isolated");
        store
            .hierarchy()
            .create_worktree_session(&isolated, &isolated_checkout)
            .unwrap();
        let primary_checkout = CheckoutId::primary_for(&workspace.id);

        let error = store
            .hierarchy()
            .acquire_write_lease(&workspace.id, &isolated.id, &primary_checkout, T0 + 1)
            .expect_err("a worktree Session is not assigned to the primary checkout");
        assert!(matches!(error, StoreError::InvalidLeaseOwnership { .. }));

        let primary_session = testing::saved_session(&store, &workspace.id, "primary-reader");
        let inverse = store
            .hierarchy()
            .acquire_write_lease(
                &workspace.id,
                &primary_session.id,
                &isolated_checkout.id,
                T0 + 2,
            )
            .expect_err("a primary Session is not assigned to an isolated checkout");
        assert!(matches!(inverse, StoreError::InvalidLeaseOwnership { .. }));

        assert!(store
            .hierarchy()
            .active_lease(&workspace.id)
            .unwrap()
            .is_none());
        let restored = store.sessions().get(&isolated.id).unwrap().unwrap();
        assert_eq!(restored.mode, SessionMode::IsolatedWorktree);
        assert_eq!(restored.checkout_id, isolated_checkout.id);
        assert_eq!(restored.cwd, isolated.cwd);
        assert_eq!(restored.worktree_path, isolated.worktree_path);
    }

    #[test]
    fn concurrent_aliasing_workspace_registration_has_one_global_winner() {
        #[derive(Debug)]
        enum Outcome {
            Saved,
            Alias,
        }

        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("turn.db");
        let checkout_root = temp.path().join("same-checkout");
        std::fs::create_dir(&checkout_root).unwrap();

        let stores = [
            Store::open_at(&database).unwrap(),
            Store::open_at(&database).unwrap(),
        ];
        let workspaces = [
            Workspace::new("first", checkout_root.to_string_lossy(), T0),
            Workspace::new("second", checkout_root.join(".").to_string_lossy(), T0),
        ];
        let barrier = Arc::new(Barrier::new(2));
        let contenders = workspaces
            .into_iter()
            .zip(stores)
            .map(|(workspace, store)| {
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    match store.workspaces().save(&workspace) {
                        Ok(()) => Outcome::Saved,
                        Err(StoreError::WorkspaceRootAlias { .. }) => Outcome::Alias,
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
                .filter(|outcome| matches!(outcome, Outcome::Saved))
                .count(),
            1,
            "exactly one canonical Workspace registration: {outcomes:?}"
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(outcome, Outcome::Alias))
                .count(),
            1,
            "the loser receives a typed alias refusal: {outcomes:?}"
        );

        let check = Store::open_at(&database).unwrap();
        assert_eq!(check.workspaces().count().unwrap(), 1);
    }

    #[test]
    fn an_alternate_path_spelling_cannot_create_a_second_fence_namespace() {
        let temp = tempfile::tempdir().unwrap();
        let checkout_root = temp.path().join("same-checkout");
        std::fs::create_dir(&checkout_root).unwrap();
        let store = testing::store();
        saved_workspace_at(&store, "first", &checkout_root);
        let alias = Workspace::new("alias", checkout_root.join(".").to_string_lossy(), T0);

        let error = store.workspaces().save(&alias).unwrap_err();
        assert!(matches!(error, StoreError::WorkspaceRootAlias { .. }));
        let fences: i64 = store
            .connection()
            .query_row("SELECT COUNT(*) FROM checkout_write_fences", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(fences, 1, "the rejected spelling creates no second fence");
    }

    #[test]
    fn the_database_trigger_is_the_final_workspace_alias_arbiter() {
        let temp = tempfile::tempdir().unwrap();
        let checkout_root = temp.path().join("same-checkout");
        std::fs::create_dir(&checkout_root).unwrap();
        let store = testing::store();
        let first = saved_workspace_at(&store, "first", &checkout_root);
        let canonical = store
            .hierarchy()
            .primary_checkout(&first.id)
            .unwrap()
            .unwrap()
            .canonical_path;
        let second = Workspace::new("second", &canonical, T0);
        store
            .connection()
            .execute(
                "INSERT INTO workspaces \
                 (id, name, root, env_json, init_commands_json, attention_json, created_ms, \
                  last_used_ms, tmux_enabled, archived, lease_reconciliation_required) \
             VALUES (?1, 'second', ?2, '[]', '[]', '{}', ?3, ?3, 0, 0, 0)",
                params![second.id.as_str(), &canonical, T0],
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
                    CheckoutId::primary_for(&second.id).as_str(),
                    second.id.as_str(),
                    &canonical,
                    T0,
                ],
            )
            .unwrap_err();
        assert!(matches!(
            error,
            rusqlite::Error::SqliteFailure(code, _)
                if code.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_TRIGGER
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
                    || code.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_TRIGGER
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
