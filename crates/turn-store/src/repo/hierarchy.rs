//! ADR-040 persistence: checkout leases, view bindings, previews and per-surface tree state.

use crate::codec::{from_json, from_tag, json, tag};
use crate::error::{Result, StoreError};
use crate::redact::{checkout_for_persistence, redact_secrets};
use crate::repo::session::SessionRepo;
use rusqlite::{params, Connection, OptionalExtension, Row, Transaction, TransactionBehavior};
use std::path::{Path, PathBuf};
use turn_core::ids::{CheckoutId, LeaseId, NodeId, PaneId, SessionId, WorkspaceId};
use turn_core::model::{
    ActivityPreview, HierarchyNodeKind, LeaseState, PaneNodeBinding, Session, SessionMode,
    SessionStatus, TreeSurfacePreferences, TreeUiState, WorkspaceCheckout, WorkspaceWriteLease,
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
        self.acquire_write_lease_with_id(workspace, session, checkout, None, now_ms)
    }

    /// Acquires the SQLite half of a lease whose id may already identify a
    /// host-global checkout lock claim. Supplying the id lets the daemon publish one
    /// stable owner across the kernel and durable fencing boundaries.
    pub fn acquire_write_lease_with_id(
        &self,
        workspace: &WorkspaceId,
        session: &SessionId,
        checkout: &CheckoutId,
        lease_id: Option<&LeaseId>,
        now_ms: i64,
    ) -> Result<WorkspaceWriteLease> {
        // `IMMEDIATE` serialises the read-check-generation-write sequence across
        // daemon processes. The global partial unique index remains the final
        // arbiter, but no contender can read a generation that another writer is
        // concurrently about to consume.
        let tx = Transaction::new_unchecked(self.conn, TransactionBehavior::Immediate)?;
        let lease = Self::acquire_in(&tx, workspace, session, checkout, lease_id, now_ms)?;
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
        self.create_session_with_lease_id(session, None, now_ms)
    }

    /// Creates a Session using the id already placed in its host-global checkout
    /// lock claim. Read-only Sessions ignore `lease_id` because they acquire no
    /// writing authority.
    pub fn create_session_with_lease_id(
        &self,
        session: &Session,
        lease_id: Option<&LeaseId>,
        now_ms: i64,
    ) -> Result<Option<WorkspaceWriteLease>> {
        let tx = Transaction::new_unchecked(self.conn, TransactionBehavior::Immediate)?;
        Self::ensure_workspace_accepts_session(&tx, session)?;
        let lease = match session.mode {
            SessionMode::MainCheckout => {
                Self::ensure_new_session(&tx, session)?;
                SessionRepo::save_in_transaction(&tx, session)?;
                Some(Self::acquire_in(
                    &tx,
                    &session.workspace_id,
                    &session.id,
                    &session.checkout_id,
                    lease_id,
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
        Self::ensure_workspace_accepts_session(tx, session)?;
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
        if redact_secrets(&checkout.path) != checkout.path
            || redact_secrets(&checkout.canonical_path) != checkout.canonical_path
        {
            return Err(StoreError::SecretInStructuralField {
                what: "checkout path",
                owner_id: checkout.id.to_string(),
            });
        }
        let canonical = Self::validate_worktree_shape(session, checkout)?;
        if redact_secrets(&canonical) != canonical {
            return Err(StoreError::SecretInStructuralField {
                what: "checkout path",
                owner_id: checkout.id.to_string(),
            });
        }
        let safe_checkout = checkout_for_persistence(checkout);
        let tx = Transaction::new_unchecked(self.conn, TransactionBehavior::Immediate)?;
        Self::ensure_workspace_accepts_session(&tx, session)?;
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
                safe_checkout.id.as_str(),
                safe_checkout.workspace_id.as_str(),
                safe_checkout.path,
                canonical,
                safe_checkout.branch,
                json("checkout shared resources", &safe_checkout.shared_resources)?,
                safe_checkout.created_ms,
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
        if session.status == SessionStatus::Archived {
            return Err(StoreError::ArchivedSession {
                session_id: session.id.to_string(),
            });
        }
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

    fn ensure_workspace_accepts_session(tx: &Transaction<'_>, session: &Session) -> Result<()> {
        let archived: Option<bool> = tx
            .query_row(
                "SELECT archived FROM workspaces WHERE id = ?1",
                params![session.workspace_id.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        match archived {
            Some(false) => Ok(()),
            Some(true) => Err(StoreError::ArchivedWorkspace {
                workspace_id: session.workspace_id.to_string(),
            }),
            None => Err(StoreError::UnknownReference {
                what: "session",
                missing: session.workspace_id.to_string(),
            }),
        }
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
        lease_id: Option<&LeaseId>,
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
        if let Some(lease_id) = lease_id {
            lease.id = lease_id.clone();
        }
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
        let changed = tx.execute(
            "UPDATE sessions SET mode = 'main_checkout', checkout_id = ?2, \
                    worktree_path = NULL, read_only_enforced = 0 \
             WHERE id = ?1 AND status != 'archived'",
            params![session.as_str(), checkout.as_str()],
        )?;
        if changed != 1 {
            return Err(StoreError::ArchivedSession {
                session_id: session.to_string(),
            });
        }
        Ok(lease)
    }

    fn verified_checkout_identity_in(
        tx: &Transaction<'_>,
        workspace: &WorkspaceId,
        session: &SessionId,
        checkout: &CheckoutId,
    ) -> Result<(String, bool)> {
        let identity: Option<(String, bool, String, String, bool, bool)> = tx
            .query_row(
                "SELECT w.root, w.lease_reconciliation_required, c.path, c.canonical_path, \
                        w.archived, s.status = 'archived' \
                 FROM sessions s \
                 JOIN workspaces w ON w.id = s.workspace_id \
                 JOIN workspace_checkouts c ON c.workspace_id = s.workspace_id \
                 WHERE s.id = ?1 AND s.workspace_id = ?2 \
                   AND c.id = ?3 AND c.workspace_id = ?2 \
                   AND s.checkout_id = c.id AND c.is_primary = 1",
                params![session.as_str(), workspace.as_str(), checkout.as_str()],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .optional()?;
        let Some((
            root,
            reconciliation_required,
            checkout_path,
            stored_canonical,
            workspace_archived,
            session_archived,
        )) = identity
        else {
            return Err(StoreError::InvalidLeaseOwnership {
                workspace_id: workspace.to_string(),
                session_id: session.to_string(),
                checkout_id: checkout.to_string(),
            });
        };
        if workspace_archived {
            return Err(StoreError::ArchivedWorkspace {
                workspace_id: workspace.to_string(),
            });
        }
        if session_archived {
            return Err(StoreError::ArchivedSession {
                session_id: session.to_string(),
            });
        }
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

    /// Gives up every lease this daemon holds, as the last durable act of a clean stop.
    ///
    /// This is the other half of [`HierarchyRepo::require_recovery_after_daemon_restart`],
    /// and without it every ordinary quit looked exactly like a crash: the next start
    /// found an unreleased lease, could not tell the two apart, and asked the user to
    /// confirm write access they had never given up.
    ///
    /// Only `active` rows are released. A `recovery_required` row is evidence that some
    /// *earlier* generation stopped without releasing and that this daemon never adopted
    /// it; releasing it here would destroy the only record of that and let the next start
    /// grant authority silently. Owning the data directory lock is what makes "every
    /// active row" mean "every lease this process holds".
    ///
    /// The owning Session keeps `main_checkout`: it is still the writer the user chose,
    /// and the next start reacquires for it. Demoting it to read-only would make a clean
    /// quit silently change the Session's mode.
    pub fn release_active_write_leases(&self, now_ms: i64) -> Result<usize> {
        self.conn
            .execute(
                "UPDATE workspace_write_leases \
                 SET state = 'released', released_ms = ?1, heartbeat_ms = ?1 \
                 WHERE state = 'active'",
                params![now_ms],
            )
            .map_err(Into::into)
    }

    /// Records that write authority for a checkout is being *withheld* pending an
    /// explicit confirmation, when no unreleased lease survived to be fenced.
    ///
    /// The case is a previous daemon that released its lease cleanly and left a process
    /// behind that outlived it. There is then nothing to fence, but granting the checkout
    /// again silently would put a second writer in it. This row is not authority: it is
    /// `recovery_required` from the moment it exists, holds the current fence generation
    /// so [`HierarchyRepo::reclaim_write_lease`] can adopt exactly it, and its timestamps
    /// describe when the withholding was recorded rather than forging an acquisition.
    ///
    /// Returns the existing unreleased lease untouched when there already is one, so a
    /// caller can never replace a live or fenced claim by asking for this.
    pub fn withhold_write_lease(
        &self,
        workspace: &WorkspaceId,
        session: &SessionId,
        checkout: &CheckoutId,
        now_ms: i64,
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
        if let Some(held) = Self::unreleased_lease_for_canonical_in(&tx, &canonical)? {
            tx.commit()?;
            return Ok(held);
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
        lease.state = LeaseState::RecoveryRequired;
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
        tx.commit()?;
        Ok(lease)
    }

    pub fn require_recovery(&self, id: &LeaseId, now_ms: i64) -> Result<bool> {
        Ok(self.conn.execute(
            "UPDATE workspace_write_leases SET state = 'recovery_required', heartbeat_ms = ?2 \
             WHERE id = ?1 AND state != 'released'",
            params![id.as_str(), now_ms],
        )? > 0)
    }

    /// Explicitly adopts the exact lease fenced by a previous daemon generation.
    ///
    /// This is one transaction: there is never a gap in which the Session is demoted
    /// to read-only or another writer can acquire the checkout. Advancing the checkout
    /// generation invalidates any helper that retained the previous daemon's token.
    pub fn reclaim_write_lease(
        &self,
        workspace: &WorkspaceId,
        session: &SessionId,
        checkout: &CheckoutId,
        lease_id: &LeaseId,
        expected_generation: u64,
        now_ms: i64,
    ) -> Result<Option<WorkspaceWriteLease>> {
        let tx = Transaction::new_unchecked(self.conn, TransactionBehavior::Immediate)?;
        let canonical: Option<String> = tx
            .query_row(
                "SELECT canonical_path FROM workspace_write_leases \
                 WHERE id = ?1 AND generation = ?2 AND workspace_id = ?3 \
                   AND session_id = ?4 AND checkout_id = ?5 \
                   AND state = 'recovery_required' LIMIT 1",
                params![
                    lease_id.as_str(),
                    expected_generation as i64,
                    workspace.as_str(),
                    session.as_str(),
                    checkout.as_str(),
                ],
                |row| row.get(0),
            )
            .optional()?;
        let Some(canonical) = canonical else {
            tx.commit()?;
            return Ok(None);
        };

        let fence_changed = tx.execute(
            "UPDATE checkout_write_fences SET generation = generation + 1 \
             WHERE canonical_path = ?1 AND generation = ?2",
            params![&canonical, expected_generation as i64],
        )?;
        if fence_changed != 1 {
            tx.commit()?;
            return Ok(None);
        }
        let generation: i64 = tx.query_row(
            "SELECT generation FROM checkout_write_fences WHERE canonical_path = ?1",
            params![&canonical],
            |row| row.get(0),
        )?;
        let changed = tx.execute(
            "UPDATE workspace_write_leases \
             SET state = 'active', heartbeat_ms = ?3, generation = ?2 \
             WHERE id = ?1 AND generation = ?4 AND state = 'recovery_required'",
            params![
                lease_id.as_str(),
                generation,
                now_ms,
                expected_generation as i64,
            ],
        )?;
        if changed != 1 {
            return Err(StoreError::InvalidLeaseOwnership {
                workspace_id: workspace.to_string(),
                session_id: session.to_string(),
                checkout_id: checkout.to_string(),
            });
        }
        let lease = tx.query_row(
            "SELECT id, workspace_id, session_id, checkout_id, mode, state, acquired_ms, \
                    heartbeat_ms, released_ms, generation \
             FROM workspace_write_leases WHERE id = ?1",
            params![lease_id.as_str()],
            lease_from_row,
        )?;
        tx.commit()?;
        Ok(Some(lease))
    }

    /// Releases exactly the lease generation the caller observed and demotes its
    /// owning Session in the same transaction.
    ///
    /// Addressing a Session alone is not fencing: after a handoff, a stale caller
    /// could release the new owner's lease. Both the immutable lease id and its
    /// monotonic checkout generation must match. A caller must never persist the
    /// Session mode as a second step: a failure there would leave a Main Checkout
    /// Session without its authority.
    pub fn release_write_lease_and_assign_read_only(
        &self,
        id: &LeaseId,
        generation: u64,
        read_only_enforced: bool,
        now_ms: i64,
    ) -> Result<bool> {
        let tx = Transaction::new_unchecked(self.conn, TransactionBehavior::Immediate)?;
        let owner: Option<String> = tx
            .query_row(
                "SELECT session_id FROM workspace_write_leases \
                 WHERE id = ?1 AND generation = ?2 AND state != 'released'",
                params![id.as_str(), generation as i64],
                |row| row.get(0),
            )
            .optional()?;
        let Some(owner) = owner else {
            tx.commit()?;
            return Ok(false);
        };
        let changed = tx.execute(
            "UPDATE workspace_write_leases SET state = 'released', released_ms = ?3, \
                    heartbeat_ms = ?3 \
             WHERE id = ?1 AND generation = ?2 AND state != 'released'",
            params![id.as_str(), generation as i64, now_ms],
        )?;
        if changed != 1 {
            tx.commit()?;
            return Ok(false);
        }
        let session_changed = tx.execute(
            "UPDATE sessions SET mode = 'read_only', worktree_path = NULL, \
                    read_only_enforced = ?2 WHERE id = ?1",
            params![&owner, read_only_enforced],
        )?;
        if session_changed != 1 {
            return Err(StoreError::UnknownReference {
                what: "workspace write lease",
                missing: owner,
            });
        }
        tx.commit()?;
        Ok(true)
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

    /// Drops the ephemeral Pane owned by one UI surface.
    ///
    /// Temporary bindings deliberately survive neither the owning window nor a
    /// daemon generation. Keeping them would make a reconnected tree advertise a
    /// Pane the new UI cannot focus because it never created that view.
    pub fn clear_temporary_bindings_for_surface(&self, surface_id: &str) -> Result<usize> {
        Ok(self.conn.execute(
            "DELETE FROM pane_node_bindings WHERE temporary = 1 AND surface_id = ?1",
            params![surface_id],
        )?)
    }

    /// Removes every temporary UI binding after a daemon restart. Saved Layout
    /// bindings are permanent (`temporary = 0`) and are left untouched.
    pub fn clear_all_temporary_bindings(&self) -> Result<usize> {
        Ok(self
            .conn
            .execute("DELETE FROM pane_node_bindings WHERE temporary = 1", [])?)
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
        save_tree_state_in(&tx, state)?;
        tx.commit()?;
        Ok(())
    }

    /// Saves one complete batch of tree decisions atomically.
    ///
    /// Expand-all and sibling reordering use this path so a replacement UI can
    /// never observe half the branch in its old state and half in its new state.
    pub fn save_tree_states(&self, states: &[TreeUiState]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        for state in states {
            save_tree_state_in(&tx, state)?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn tree_state(&self, surface_id: &str) -> Result<Vec<TreeUiState>> {
        let mut stmt = self.conn.prepare(
            "SELECT surface_id, node_kind, node_id, expanded, expansion_set, selected, \
                    manual_order, visibility_mode, updated_ms \
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
                expansion_set: row.get("expansion_set")?,
                selected: row.get("selected")?,
                manual_order: row.get("manual_order")?,
                visibility_mode: row.get("visibility_mode")?,
                updated_ms: row.get("updated_ms")?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn save_tree_surface_preferences(&self, state: &TreeSurfacePreferences) -> Result<()> {
        self.conn.execute(
            "INSERT INTO tree_surface_preferences \
                 (surface_id, filters_json, visibility_mode, scroll_node_kind, \
                  scroll_node_id, updated_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
             ON CONFLICT(surface_id) DO UPDATE SET \
                 filters_json = excluded.filters_json, \
                 visibility_mode = excluded.visibility_mode, \
                 scroll_node_kind = excluded.scroll_node_kind, \
                 scroll_node_id = excluded.scroll_node_id, \
                 updated_ms = excluded.updated_ms",
            params![
                state.surface_id,
                json("tree filters", &state.filters)?,
                tag("tree visibility mode", &state.visibility_mode)?,
                state
                    .scroll_anchor_kind
                    .as_ref()
                    .map(|kind| tag("hierarchy node kind", kind))
                    .transpose()?,
                state.scroll_anchor_id,
                state.updated_ms,
            ],
        )?;
        Ok(())
    }

    pub fn tree_surface_preferences(&self, surface_id: &str) -> Result<TreeSurfacePreferences> {
        let row = self
            .conn
            .query_row(
                "SELECT surface_id, filters_json, visibility_mode, scroll_node_kind, \
                        scroll_node_id, updated_ms \
                 FROM tree_surface_preferences WHERE surface_id = ?1",
                params![surface_id],
                |row| {
                    Ok((
                        row.get::<_, String>("surface_id")?,
                        row.get::<_, String>("filters_json")?,
                        row.get::<_, String>("visibility_mode")?,
                        row.get::<_, Option<String>>("scroll_node_kind")?,
                        row.get::<_, Option<String>>("scroll_node_id")?,
                        row.get::<_, i64>("updated_ms")?,
                    ))
                },
            )
            .optional()?;
        let Some((surface_id, filters, visibility, anchor_kind, anchor_id, updated_ms)) = row
        else {
            return Ok(TreeSurfacePreferences::normal(surface_id));
        };
        Ok(TreeSurfacePreferences {
            filters: from_json("tree filters", &surface_id, &filters)?,
            visibility_mode: from_tag("tree visibility mode", &surface_id, &visibility)?,
            scroll_anchor_kind: anchor_kind
                .map(|kind| from_tag("hierarchy node kind", &surface_id, &kind))
                .transpose()?,
            scroll_anchor_id: anchor_id,
            surface_id,
            updated_ms,
        })
    }
}

fn save_tree_state_in(tx: &Transaction<'_>, state: &TreeUiState) -> Result<()> {
    if state.selected {
        tx.execute(
            "UPDATE tree_ui_state SET selected = 0, updated_ms = ?2 \
             WHERE surface_id = ?1 AND selected = 1",
            params![state.surface_id, state.updated_ms],
        )?;
    }
    tx.execute(
        "INSERT INTO tree_ui_state \
             (surface_id, node_kind, node_id, expanded, expansion_set, selected, \
              manual_order, visibility_mode, updated_ms) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) \
         ON CONFLICT(surface_id, node_kind, node_id) DO UPDATE SET \
             expanded = excluded.expanded, expansion_set = excluded.expansion_set, \
             selected = excluded.selected, manual_order = excluded.manual_order, \
             visibility_mode = excluded.visibility_mode, updated_ms = excluded.updated_ms",
        params![
            state.surface_id,
            tag("hierarchy node kind", &state.node_kind)?,
            state.node_id,
            state.expanded,
            state.expansion_set,
            state.selected,
            state.manual_order,
            state.visibility_mode,
            state.updated_ms,
        ],
    )?;
    Ok(())
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
        Session, SessionMode, TreeFilter, TreeSurfacePreferences, TreeVisibilityMode, Workspace,
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
            .release_write_lease_and_assign_read_only(&lease.id, lease.generation, true, T0 + 2)
            .unwrap());
        let second_lease = store
            .hierarchy()
            .acquire_write_lease(&workspace.id, &second.id, &checkout, T0 + 3)
            .unwrap();
        assert!(second_lease.generation > lease.generation);
        let demoted = store.sessions().get(&first.id).unwrap().unwrap();
        assert_eq!(
            demoted.mode,
            SessionMode::ReadOnly,
            "release and Session demotion are one durable transition"
        );
        assert!(
            demoted.read_only_enforced,
            "the precomputed guard state belongs to the same transition"
        );
    }

    #[test]
    fn release_and_session_demotion_roll_back_as_one_transition() {
        let store = testing::store();
        let workspace = testing::saved_workspace(&store, "atomic-release");
        let session = testing::saved_session(&store, &workspace.id, "writer");
        let checkout = CheckoutId::primary_for(&workspace.id);
        let lease = store
            .hierarchy()
            .acquire_write_lease(&workspace.id, &session.id, &checkout, T0)
            .unwrap();
        assert_eq!(
            store.sessions().get(&session.id).unwrap().unwrap().mode,
            SessionMode::MainCheckout
        );

        store
            .connection()
            .execute_batch(
                "CREATE TRIGGER fail_session_demotion \
                 BEFORE UPDATE OF mode ON sessions \
                 WHEN NEW.mode = 'read_only' \
                 BEGIN SELECT RAISE(ABORT, 'injected Session demotion failure'); END;",
            )
            .unwrap();
        let error = store
            .hierarchy()
            .release_write_lease_and_assign_read_only(&lease.id, lease.generation, true, T0 + 1)
            .expect_err("the injected second write must abort the whole transaction");
        assert!(matches!(error, StoreError::Sqlite(_)));
        assert_eq!(
            store
                .hierarchy()
                .active_lease(&workspace.id)
                .unwrap()
                .unwrap(),
            lease,
            "the lease update must roll back with the failed Session update"
        );
        assert_eq!(
            store.sessions().get(&session.id).unwrap().unwrap().mode,
            SessionMode::MainCheckout
        );

        store
            .connection()
            .execute_batch("DROP TRIGGER fail_session_demotion;")
            .unwrap();
        assert!(store
            .hierarchy()
            .release_write_lease_and_assign_read_only(&lease.id, lease.generation, true, T0 + 2)
            .unwrap());
        assert!(store
            .hierarchy()
            .active_lease(&workspace.id)
            .unwrap()
            .is_none());
        assert_eq!(
            store.sessions().get(&session.id).unwrap().unwrap().mode,
            SessionMode::ReadOnly
        );
    }

    #[test]
    fn reclaiming_a_recovery_lease_is_atomic_and_advances_its_fence() {
        let store = testing::store();
        let workspace = testing::saved_workspace(&store, "atomic-reclaim");
        let session = testing::saved_session(&store, &workspace.id, "writer");
        let checkout = CheckoutId::primary_for(&workspace.id);
        let lease = store
            .hierarchy()
            .acquire_write_lease(&workspace.id, &session.id, &checkout, T0)
            .unwrap();
        assert!(store
            .hierarchy()
            .require_recovery(&lease.id, T0 + 1)
            .unwrap());

        let reclaimed = store
            .hierarchy()
            .reclaim_write_lease(
                &workspace.id,
                &session.id,
                &checkout,
                &lease.id,
                lease.generation,
                T0 + 2,
            )
            .unwrap()
            .expect("the exact recovery claim is adopted");
        assert_eq!(reclaimed.id, lease.id);
        assert_eq!(reclaimed.state, LeaseState::Active);
        assert_eq!(reclaimed.generation, lease.generation + 1);
        assert_eq!(
            store.sessions().get(&session.id).unwrap().unwrap().mode,
            SessionMode::MainCheckout,
            "reclaim never creates a read-only gap"
        );
        let rows: i64 = store
            .connection()
            .query_row("SELECT COUNT(*) FROM workspace_write_leases", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(rows, 1, "reclaim updates the existing claim in place");
    }

    #[test]
    fn a_stale_reclaim_changes_neither_the_lease_nor_its_fence() {
        let store = testing::store();
        let workspace = testing::saved_workspace(&store, "stale-reclaim");
        let session = testing::saved_session(&store, &workspace.id, "writer");
        let checkout = CheckoutId::primary_for(&workspace.id);
        let lease = store
            .hierarchy()
            .acquire_write_lease(&workspace.id, &session.id, &checkout, T0)
            .unwrap();
        assert!(store
            .hierarchy()
            .require_recovery(&lease.id, T0 + 1)
            .unwrap());

        assert!(store
            .hierarchy()
            .reclaim_write_lease(
                &workspace.id,
                &session.id,
                &checkout,
                &lease.id,
                lease.generation + 1,
                T0 + 2,
            )
            .unwrap()
            .is_none());
        let current = store
            .hierarchy()
            .active_lease(&workspace.id)
            .unwrap()
            .unwrap();
        assert_eq!(current.state, LeaseState::RecoveryRequired);
        assert_eq!(current.generation, lease.generation);
        assert_eq!(
            store.sessions().get(&session.id).unwrap().unwrap().mode,
            SessionMode::MainCheckout
        );
    }

    #[test]
    fn archived_workspaces_cannot_create_session_authority() {
        let store = testing::store();
        let mut workspace = testing::saved_workspace(&store, "archived-create");
        workspace.archived = true;
        store.workspaces().save(&workspace).unwrap();
        let mut session = Session::new(
            workspace.id.clone(),
            "hidden writer",
            workspace.root.clone(),
            Layout::single(Pane::new(PaneKind::Agent)),
            T0 + 1,
        );
        session.mode = SessionMode::MainCheckout;

        let error = store
            .hierarchy()
            .create_session(&session, T0 + 1)
            .expect_err("an archived Workspace cannot mint hidden authority");
        assert!(matches!(error, StoreError::ArchivedWorkspace { .. }));
        assert!(store.sessions().get(&session.id).unwrap().is_none());
        assert!(store
            .hierarchy()
            .active_lease(&workspace.id)
            .unwrap()
            .is_none());
    }

    #[test]
    fn archived_sessions_and_workspaces_cannot_acquire_authority() {
        let store = testing::store();
        let mut workspace = testing::saved_workspace(&store, "archived-acquire");
        let mut session = testing::saved_session(&store, &workspace.id, "reader");
        let checkout = CheckoutId::primary_for(&workspace.id);

        session.status = SessionStatus::Archived;
        store.sessions().save(&session).unwrap();
        let archived_session = store
            .hierarchy()
            .acquire_write_lease(&workspace.id, &session.id, &checkout, T0 + 1)
            .expect_err("an archived Session cannot become a hidden writer");
        assert!(matches!(
            archived_session,
            StoreError::ArchivedSession { .. }
        ));

        session.status = SessionStatus::Active;
        store.sessions().save(&session).unwrap();
        workspace.archived = true;
        store.workspaces().save(&workspace).unwrap();
        let archived_workspace = store
            .hierarchy()
            .acquire_write_lease(&workspace.id, &session.id, &checkout, T0 + 2)
            .expect_err("an archived Workspace cannot grant authority");
        assert!(matches!(
            archived_workspace,
            StoreError::ArchivedWorkspace { .. }
        ));
        assert!(store
            .hierarchy()
            .active_lease(&workspace.id)
            .unwrap()
            .is_none());
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

    /// The half of the lifecycle that was missing: a clean stop gives the checkout
    /// back, so the next start has nothing to ask about.
    #[test]
    fn a_clean_stop_releases_active_authority_and_leaves_the_session_as_the_writer() {
        let store = testing::store();
        let workspace = testing::saved_workspace(&store, "clean-stop");
        let session = testing::saved_session(&store, &workspace.id, "writer");
        let checkout = CheckoutId::primary_for(&workspace.id);
        let lease = store
            .hierarchy()
            .acquire_write_lease(&workspace.id, &session.id, &checkout, T0)
            .unwrap();

        assert_eq!(
            store
                .hierarchy()
                .release_active_write_leases(T0 + 5)
                .unwrap(),
            1
        );
        let released = store.hierarchy().lease(&lease.id).unwrap().unwrap();
        assert_eq!(released.state, LeaseState::Released);
        assert_eq!(released.released_ms, Some(T0 + 5));
        assert!(store
            .hierarchy()
            .active_lease(&workspace.id)
            .unwrap()
            .is_none());
        assert_eq!(
            store.sessions().get(&session.id).unwrap().unwrap().mode,
            SessionMode::MainCheckout,
            "a clean stop must not silently change the writer the user chose"
        );

        // And the next start can simply take it again: no fenced row remains.
        let reacquired = store
            .hierarchy()
            .acquire_write_lease(&workspace.id, &session.id, &checkout, T0 + 6)
            .unwrap();
        assert_eq!(reacquired.state, LeaseState::Active);
        assert!(reacquired.generation > lease.generation);
    }

    /// A `recovery_required` row is the only evidence that some earlier daemon died
    /// without releasing. A later clean stop must not launder it into a release.
    #[test]
    fn releasing_on_a_clean_stop_never_clears_an_unadopted_recovery_claim() {
        let store = testing::store();
        let workspace = testing::saved_workspace(&store, "unadopted-recovery");
        let session = testing::saved_session(&store, &workspace.id, "writer");
        let checkout = CheckoutId::primary_for(&workspace.id);
        let lease = store
            .hierarchy()
            .acquire_write_lease(&workspace.id, &session.id, &checkout, T0)
            .unwrap();
        assert_eq!(
            store
                .hierarchy()
                .require_recovery_after_daemon_restart()
                .unwrap(),
            1
        );

        assert_eq!(
            store
                .hierarchy()
                .release_active_write_leases(T0 + 5)
                .unwrap(),
            0
        );
        let after = store.hierarchy().lease(&lease.id).unwrap().unwrap();
        assert_eq!(after.state, LeaseState::RecoveryRequired);
        assert_eq!(after.released_ms, None);
        assert_eq!(after.generation, lease.generation);
    }

    #[test]
    fn withholding_authority_records_a_recovery_claim_the_confirm_flow_can_adopt() {
        let store = testing::store();
        let workspace = testing::saved_workspace(&store, "withheld");
        let session = testing::saved_session(&store, &workspace.id, "writer");
        let checkout = CheckoutId::primary_for(&workspace.id);
        let released = store
            .hierarchy()
            .acquire_write_lease(&workspace.id, &session.id, &checkout, T0)
            .unwrap();
        store
            .hierarchy()
            .release_active_write_leases(T0 + 1)
            .unwrap();

        let withheld = store
            .hierarchy()
            .withhold_write_lease(&workspace.id, &session.id, &checkout, T0 + 2)
            .unwrap();
        assert_eq!(withheld.state, LeaseState::RecoveryRequired);
        assert_ne!(withheld.id, released.id, "a new record, not a resurrection");
        assert_eq!(withheld.released_ms, None);
        assert_eq!(withheld.acquired_ms, T0 + 2);
        assert_eq!(
            withheld.generation, released.generation,
            "withholding is not an acquisition, so it advances no fence"
        );
        // It is not authority, and cannot become authority by accident.
        assert!(store
            .hierarchy()
            .verify_active_write_lease(&workspace.id, &session.id, &checkout)
            .is_err());

        let reclaimed = store
            .hierarchy()
            .reclaim_write_lease(
                &workspace.id,
                &session.id,
                &checkout,
                &withheld.id,
                withheld.generation,
                T0 + 3,
            )
            .unwrap()
            .expect("the user's confirmation adopts exactly the withheld claim");
        assert_eq!(reclaimed.state, LeaseState::Active);
        assert_eq!(reclaimed.generation, withheld.generation + 1);
    }

    #[test]
    fn withholding_authority_cannot_displace_a_live_or_fenced_claim() {
        let store = testing::store();
        let workspace = testing::saved_workspace(&store, "withhold-conflict");
        let session = testing::saved_session(&store, &workspace.id, "writer");
        let checkout = CheckoutId::primary_for(&workspace.id);
        let live = store
            .hierarchy()
            .acquire_write_lease(&workspace.id, &session.id, &checkout, T0)
            .unwrap();

        let observed = store
            .hierarchy()
            .withhold_write_lease(&workspace.id, &session.id, &checkout, T0 + 1)
            .unwrap();
        assert_eq!(observed.id, live.id);
        assert_eq!(observed.state, LeaseState::Active);
        let rows: i64 = store
            .connection()
            .query_row("SELECT COUNT(*) FROM workspace_write_leases", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(rows, 1);
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
    fn a_preallocated_host_lock_id_is_preserved_across_sqlite_acquisition() {
        let store = testing::store();
        let workspace = testing::saved_workspace(&store, "joined authority");
        let mut session = Session::new(
            workspace.id.clone(),
            "writer",
            workspace.root.clone(),
            Layout::single(Pane::new(PaneKind::AgentTree)),
            T0,
        );
        session.mode = SessionMode::MainCheckout;
        let first_id = LeaseId::new();
        let first = store
            .hierarchy()
            .create_session_with_lease_id(&session, Some(&first_id), T0)
            .unwrap()
            .expect("the main checkout lease");
        assert_eq!(first.id, first_id);

        assert!(store
            .hierarchy()
            .release_write_lease_and_assign_read_only(&first.id, first.generation, false, T0 + 1)
            .unwrap());
        let second_id = LeaseId::new();
        let second = store
            .hierarchy()
            .acquire_write_lease_with_id(
                &workspace.id,
                &session.id,
                &session.checkout_id,
                Some(&second_id),
                T0 + 2,
            )
            .unwrap();
        assert_eq!(second.id, second_id);
        assert!(second.generation > first.generation);
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
            .release_write_lease_and_assign_read_only(&old.id, old.generation + 1, false, T0 + 1)
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
            .release_write_lease_and_assign_read_only(&old.id, old.generation, false, T0 + 2)
            .unwrap());
        let current = store
            .hierarchy()
            .acquire_write_lease(&workspace.id, &second.id, &checkout, T0 + 3)
            .unwrap();
        assert!(current.generation > old.generation);

        assert!(!store
            .hierarchy()
            .release_write_lease_and_assign_read_only(&current.id, old.generation, false, T0 + 4)
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
    fn temporary_bindings_are_scoped_and_can_be_purged_without_touching_layout_panes() {
        let store = testing::store();
        let session = testing::saved_session_anywhere(&store, "temporary-scope");
        let node = ProcessNode::agent(session.id.clone(), "claude", "/repo", T0);
        store.nodes().upsert(&node).unwrap();
        for (name, temporary, surface) in [
            ("saved", false, None),
            ("temp-a", true, Some("window-a")),
            ("temp-b", true, Some("window-b")),
        ] {
            store
                .hierarchy()
                .bind_pane(&PaneNodeBinding {
                    pane_id: PaneId::from_stored(name),
                    session_id: session.id.clone(),
                    node_id: node.id.clone(),
                    temporary,
                    surface_id: surface.map(str::to_string),
                    opened_ms: T0,
                })
                .unwrap();
        }

        assert_eq!(
            store
                .hierarchy()
                .clear_temporary_bindings_for_surface("window-a")
                .unwrap(),
            1
        );
        let remaining = store.hierarchy().bindings_for_session(&session.id).unwrap();
        assert!(remaining
            .iter()
            .any(|binding| binding.pane_id.as_str() == "saved"));
        assert!(remaining
            .iter()
            .any(|binding| binding.pane_id.as_str() == "temp-b"));
        assert_eq!(store.hierarchy().clear_all_temporary_bindings().unwrap(), 1);
        let remaining = store.hierarchy().bindings_for_session(&session.id).unwrap();
        assert_eq!(remaining.len(), 1);
        assert!(!remaining[0].temporary);
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
                    expansion_set: true,
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

    #[test]
    fn tree_presentation_and_manual_order_survive_reopening_sqlite() {
        let directory = tempfile::tempdir().unwrap();
        {
            let store = Store::open_in(directory.path()).unwrap();
            store
                .hierarchy()
                .save_tree_surface_preferences(&TreeSurfacePreferences {
                    surface_id: "window-restart".into(),
                    filters: vec![TreeFilter::Attention, TreeFilter::Agents],
                    visibility_mode: TreeVisibilityMode::Technical,
                    scroll_anchor_kind: Some(HierarchyNodeKind::Process),
                    scroll_anchor_id: Some("proc_217".into()),
                    updated_ms: T0,
                })
                .unwrap();
            store
                .hierarchy()
                .save_tree_states(&[
                    TreeUiState {
                        surface_id: "window-restart".into(),
                        node_kind: HierarchyNodeKind::Process,
                        node_id: "proc_217".into(),
                        expanded: true,
                        expansion_set: true,
                        selected: true,
                        manual_order: Some(0),
                        visibility_mode: None,
                        updated_ms: T0,
                    },
                    TreeUiState {
                        surface_id: "window-restart".into(),
                        node_kind: HierarchyNodeKind::Process,
                        node_id: "proc_216".into(),
                        expanded: false,
                        expansion_set: true,
                        selected: false,
                        manual_order: Some(1),
                        visibility_mode: None,
                        updated_ms: T0,
                    },
                ])
                .unwrap();
        }

        let reopened = Store::open_in(directory.path()).unwrap();
        let preferences = reopened
            .hierarchy()
            .tree_surface_preferences("window-restart")
            .unwrap();
        assert_eq!(
            preferences.filters,
            [TreeFilter::Attention, TreeFilter::Agents]
        );
        assert_eq!(preferences.visibility_mode, TreeVisibilityMode::Technical);
        assert_eq!(preferences.scroll_anchor_id.as_deref(), Some("proc_217"));
        let order = reopened.hierarchy().tree_state("window-restart").unwrap();
        assert_eq!(
            order
                .iter()
                .map(|row| (
                    row.node_id.as_str(),
                    row.manual_order,
                    row.selected,
                    row.expanded,
                    row.expansion_set,
                ))
                .collect::<Vec<_>>(),
            [
                ("proc_217", Some(0), true, true, true),
                ("proc_216", Some(1), false, false, true),
            ]
        );
    }
}
