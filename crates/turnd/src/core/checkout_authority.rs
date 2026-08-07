//! Joining SQLite lease state to host-wide checkout authority.

use super::Core;
use crate::checkout_lock::{
    CheckoutLockError, CheckoutLockInheritance, CheckoutLockOwner, CheckoutWriteLock,
};
use std::path::Path;
use turn_core::ids::{LeaseId, SessionId, WorkspaceId};
use turn_core::model::{LeaseState, Session, WorkspaceWriteLease};
use turn_proto::{
    ErrorCode, ProtoError, ProtoErrorContext, SessionConflictAlternative, WriteLeaseOwnerView,
};

impl Core {
    pub(crate) fn checkout_lock_claim(
        &self,
        session: &Session,
        lease: &WorkspaceWriteLease,
    ) -> Result<CheckoutWriteLock, ProtoError> {
        let checkout = self.checkout_for_session(session)?;
        let owner = owner_view(session);
        let data_dir = self.data_dir.clone();
        let claim = lease.clone();
        CheckoutWriteLock::acquire(Path::new(&checkout.path), move |identity| {
            CheckoutLockOwner::new(&data_dir, identity, claim, owner)
        })
        .map_err(|error| {
            self.map_checkout_lock_error(
                &session.workspace_id,
                &session.id,
                &session.checkout_id,
                error,
            )
        })
    }

    /// Publishes the final SQLite generation into owner metadata and retains the
    /// kernel lock in Core. Metadata is diagnostic; a failed refresh does not discard
    /// an already-held safety boundary or permit a process to launch without it.
    pub(crate) fn install_checkout_write_lock(
        &mut self,
        session_id: &SessionId,
        lease: &WorkspaceWriteLease,
        mut lock: CheckoutWriteLock,
    ) {
        if let Some(session) = self.sessions.get(session_id) {
            let owner = CheckoutLockOwner::new(
                &self.data_dir,
                lock.identity(),
                lease.clone(),
                owner_view(session),
            );
            if let Err(error) = lock.update_owner(owner) {
                tracing::warn!(%error, lease = %lease.id, "could not refresh checkout lock owner metadata");
            }
        }
        self.checkout_write_locks.insert(lease.id.clone(), lock);
    }

    pub(crate) fn drop_checkout_write_lock(&mut self, lease_id: &LeaseId) {
        self.checkout_write_locks.remove(lease_id);
    }

    pub(crate) fn require_checkout_write_lock(
        &self,
        session: &Session,
        lease: &WorkspaceWriteLease,
    ) -> Result<(), ProtoError> {
        let checkout = self.checkout_for_session(session)?;
        let Some(lock) = self.checkout_write_locks.get(&lease.id) else {
            return Err(ProtoError::refused(
                "This Session has no host-wide checkout write lock",
            ));
        };
        match lock.protects(Path::new(&checkout.path), lease) {
            Ok(true) => Ok(()),
            Ok(false) => Err(ProtoError::refused(
                "The host-wide checkout lock does not match this Session lease",
            )),
            Err(error) => Err(ProtoError::refused(
                "Turn cannot verify the host-wide checkout write lock",
            )
            .with_detail(error.to_string())),
        }
    }

    pub(crate) fn checkout_lock_inheritance(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<CheckoutLockInheritance>, ProtoError> {
        let session = self.session(session_id)?;
        if session.mode != turn_core::model::SessionMode::MainCheckout {
            return Ok(None);
        }
        let lease = self
            .store
            .hierarchy()
            .verify_active_write_lease(&session.workspace_id, session_id, &session.checkout_id)
            .map_err(|error| {
                ProtoError::refused("This Session has no durable write authority to inherit")
                    .with_detail(error.to_string())
            })?;
        self.require_checkout_write_lock(session, &lease)?;
        self.checkout_write_locks[&lease.id]
            .inherit_for_spawn()
            .map(Some)
            .map_err(|error| {
                ProtoError::new(
                    ErrorCode::Unavailable,
                    "Turn could not carry checkout authority into the new process",
                )
                .with_detail(error.to_string())
            })
    }

    pub(crate) fn refresh_checkout_lock_owner(&mut self, session_id: &SessionId) {
        let Some(session) = self.sessions.get(session_id).cloned() else {
            return;
        };
        if session.mode != turn_core::model::SessionMode::MainCheckout {
            return;
        }
        let Ok(Some(lease)) = self.store.hierarchy().active_lease(&session.workspace_id) else {
            return;
        };
        if lease.state != LeaseState::Active || lease.session_id != *session_id {
            return;
        }
        let Some(lock) = self.checkout_write_locks.get_mut(&lease.id) else {
            return;
        };
        let owner = CheckoutLockOwner::new(
            &self.data_dir,
            lock.identity(),
            lease.clone(),
            owner_view(&session),
        );
        if let Err(error) = lock.update_owner(owner) {
            tracing::warn!(%error, lease = %lease.id, "could not update checkout lock owner metadata");
        }
    }

    pub(crate) fn heartbeat_checkout_lock_owner(
        &mut self,
        lease: &WorkspaceWriteLease,
        now_ms: i64,
    ) {
        let Some(session) = self.sessions.get(&lease.session_id).cloned() else {
            return;
        };
        let Some(lock) = self.checkout_write_locks.get_mut(&lease.id) else {
            tracing::warn!(lease = %lease.id, "refused to advertise a lease heartbeat without its host checkout lock");
            return;
        };
        let mut current = lease.clone();
        current.heartbeat_ms = now_ms;
        let owner = CheckoutLockOwner::new(
            &self.data_dir,
            lock.identity(),
            current,
            owner_view(&session),
        );
        if let Err(error) = lock.update_owner(owner) {
            tracing::warn!(%error, lease = %lease.id, "could not heartbeat checkout lock owner metadata");
        }
    }

    fn map_checkout_lock_error(
        &self,
        workspace_id: &WorkspaceId,
        requesting_session_id: &SessionId,
        checkout_id: &turn_core::ids::CheckoutId,
        error: CheckoutLockError,
    ) -> ProtoError {
        if let CheckoutLockError::Contended {
            owner: Some(held), ..
        } = error
        {
            let held = *held;
            return ProtoError::workspace_write_lease_conflict(
                ProtoErrorContext::WorkspaceWriteLeaseConflict {
                    workspace_id: workspace_id.clone(),
                    checkout_id: checkout_id.clone(),
                    requesting_session_id: Some(requesting_session_id.clone()),
                    lease: Box::new(held.lease),
                    owner: Box::new(held.owner),
                    // An owner in another daemon cannot be focused through this
                    // socket. The remaining choices are all local and explicit.
                    alternatives: vec![
                        SessionConflictAlternative::CreateReadOnly,
                        SessionConflictAlternative::CreateIsolatedWorktree,
                        SessionConflictAlternative::Cancel,
                    ],
                },
            );
        }
        match error {
            CheckoutLockError::Contended { .. } => ProtoError::new(
                ErrorCode::Conflict,
                "Another Turn daemon owns this checkout, but its owner metadata could not be read",
            )
            .with_detail(error.to_string()),
            other => ProtoError::new(
                ErrorCode::Unavailable,
                "Turn could not establish the host-wide checkout write lock",
            )
            .with_detail(other.to_string()),
        }
    }
}

fn owner_view(session: &Session) -> WriteLeaseOwnerView {
    WriteLeaseOwnerView {
        session_id: session.id.clone(),
        session_name: session.name.clone(),
        mode: session.mode,
        cwd: session.cwd.clone(),
        branch: session.git_branch.clone(),
        last_activity_ms: session.last_activity_ms,
    }
}
