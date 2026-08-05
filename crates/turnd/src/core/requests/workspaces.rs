//! Workspace operations.

use super::{check_name, Answer};
use crate::core::Core;
use turn_core::ids::{SessionId, WorkspaceId};
use turn_core::model::Workspace;
use turn_proto::{CloseDisposition, ProtoError, Response};

impl Core {
    pub(super) fn list_workspaces(&self, include_archived: bool, now_ms: i64) -> Answer {
        Ok(Response::Workspaces {
            workspaces: self.workspace_summaries(include_archived, now_ms),
        })
    }

    pub(super) fn create_workspace(&mut self, name: String, root: String, now_ms: i64) -> Answer {
        let name = check_name(&name)?;
        let root = root.trim();
        if root.is_empty() {
            return Err(ProtoError::invalid("A workspace needs a root directory"));
        }
        // It is required to be absolute because every Session inherits it. The
        // store also requires it to exist and resolves its filesystem identity;
        // fencing a caller-provided spelling of a missing path is unsafe because
        // a later symlink could alias another checkout.
        if !root.starts_with('/') {
            return Err(ProtoError::invalid(
                "A workspace root must be an absolute path",
            ));
        }

        let workspace = Workspace::new(name, root, now_ms);
        let id = workspace.id.clone();
        self.store
            .workspaces()
            .save(&workspace)
            .map_err(workspace_store)?;
        let workspace = self
            .store
            .workspaces()
            .get(&id)
            .map_err(store)?
            .ok_or_else(|| ProtoError::internal("the saved Workspace disappeared"))?;
        self.workspaces.insert(id.clone(), workspace);
        tracing::info!(workspace = %id, root, "created a workspace");
        self.answer_workspace(&id, now_ms)
    }

    pub(super) fn rename_workspace(
        &mut self,
        id: &WorkspaceId,
        name: String,
        now_ms: i64,
    ) -> Answer {
        let name = check_name(&name)?;
        let workspace = self
            .workspaces
            .get_mut(id)
            .ok_or_else(|| ProtoError::not_found("workspace", id.as_str()))?;
        workspace.name = name;
        let workspace = workspace.clone();
        self.store.workspaces().save(&workspace).map_err(store)?;
        self.answer_workspace(id, now_ms)
    }

    pub(super) fn archive_workspace(
        &mut self,
        id: &WorkspaceId,
        archived: bool,
        now_ms: i64,
    ) -> Answer {
        let workspace = self
            .workspaces
            .get_mut(id)
            .ok_or_else(|| ProtoError::not_found("workspace", id.as_str()))?;
        workspace.archived = archived;
        let workspace = workspace.clone();
        self.store.workspaces().save(&workspace).map_err(store)?;
        // Archiving is a filing decision, not an instruction to stop work. The
        // processes carry on; the row leaves the switcher.
        self.answer_workspace(id, now_ms)
    }

    /// Copies a workspace's settings, with no sessions.
    pub(super) fn duplicate_workspace(
        &mut self,
        id: &WorkspaceId,
        name: Option<String>,
        now_ms: i64,
    ) -> Answer {
        let source = self.workspace(id)?.clone();
        let name = match name {
            Some(name) => check_name(&name)?,
            None => format!("{} (copy)", source.name),
        };
        let mut copy = source.clone();
        copy.id = WorkspaceId::new();
        copy.name = name;
        copy.created_ms = now_ms;
        copy.last_used_ms = now_ms;
        copy.archived = false;
        let new_id = copy.id.clone();
        self.store
            .workspaces()
            .save(&copy)
            .map_err(workspace_store)?;
        self.workspaces.insert(new_id.clone(), copy);
        self.answer_workspace(&new_id, now_ms)
    }

    /// Closes every session in a workspace.
    ///
    /// The workspace itself stays on disk. "Close" is about what is on screen and what
    /// is running; deleting a project because its last window was closed is not
    /// something the protocol can express, and should not be.
    pub(super) fn close_workspace(
        &mut self,
        id: &WorkspaceId,
        disposition: CloseDisposition,
        now_ms: i64,
    ) -> Answer {
        self.workspace(id)?;
        let sessions: Vec<SessionId> = self
            .sessions
            .values()
            .filter(|session| &session.workspace_id == id)
            .map(|session| session.id.clone())
            .collect();
        for session in sessions {
            self.close_session(&session, disposition, now_ms)?;
        }
        Ok(Response::Ack)
    }

    fn answer_workspace(&self, id: &WorkspaceId, now_ms: i64) -> Answer {
        let workspace = self
            .workspace_summary(id, now_ms)
            .ok_or_else(|| ProtoError::not_found("workspace", id.as_str()))?;
        Ok(Response::Workspace { workspace })
    }
}

/// A store failure the user needs to know about: the change is in memory but not on
/// disk, so it will not survive a restart.
pub(super) fn store(error: turn_store::StoreError) -> ProtoError {
    tracing::error!(%error, "a store write failed");
    ProtoError::new(
        turn_proto::ErrorCode::Unavailable,
        "The change could not be written to disk",
    )
    .with_detail(error.to_string())
}

/// Filesystem identity failures are durable safety refusals, not transient store
/// outages. Keep that distinction machine-readable so clients never offer an
/// automatic retry that could mint a divergent checkout fence.
fn workspace_store(error: turn_store::StoreError) -> ProtoError {
    match error {
        turn_store::StoreError::WorkspaceRoot { path, cause } => ProtoError::refused(
            "A Workspace root must already exist as a directory before Turn can fence it",
        )
        .with_detail(format!("{path}: {cause}")),
        turn_store::StoreError::WorkspaceRootAlias {
            canonical_path,
            existing_workspace_id,
        } => ProtoError::refused("That checkout is already registered by another Workspace")
            .with_detail(format!(
                "{canonical_path} belongs to {existing_workspace_id}"
            )),
        other @ turn_store::StoreError::LeaseReconciliationRequired { .. } => ProtoError::refused(
            "Changing a Workspace checkout identity requires explicit lease reconciliation",
        )
        .with_detail(other.to_string()),
        other => store(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::testing::Harness;

    #[tokio::test]
    async fn create_workspace_refuses_a_missing_root_without_persisting_anything() {
        let mut harness = Harness::new().await;
        let missing = harness._dir.path().join("not-cloned-yet");
        let error = harness
            .core
            .create_workspace("missing".into(), missing.to_string_lossy().into_owned(), 10)
            .expect_err("a textual path is not a safe checkout identity");

        assert_eq!(error.code, turn_proto::ErrorCode::Refused);
        assert!(error.message.contains("already exist"));
        assert!(harness.core.workspaces.is_empty());
        assert_eq!(harness.core.store.workspaces().count().unwrap(), 0);
    }

    #[tokio::test]
    async fn create_workspace_returns_and_persists_the_canonical_root() {
        let mut harness = Harness::new().await;
        let root = harness._dir.path().join("repo");
        std::fs::create_dir(&root).unwrap();
        let spelling = root.join(".").to_string_lossy().into_owned();
        let expected = std::fs::canonicalize(root)
            .unwrap()
            .to_string_lossy()
            .into_owned();

        let id = match harness
            .core
            .create_workspace("canonical".into(), spelling, 10)
            .unwrap()
        {
            Response::Workspace { workspace } => {
                assert_eq!(workspace.root, expected);
                workspace.id
            }
            other => panic!("unexpected {other:?}"),
        };
        assert_eq!(harness.core.workspaces[&id].root, expected);
        assert_eq!(
            harness
                .core
                .store
                .hierarchy()
                .primary_checkout(&id)
                .unwrap()
                .unwrap()
                .canonical_path,
            expected
        );
    }
}
