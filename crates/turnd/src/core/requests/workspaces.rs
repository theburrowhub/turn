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
        // Not required to exist: a user may point Turn at a directory a moment before
        // they clone into it. It is required to be absolute, because every session
        // inherits it as a working directory and a relative one would resolve against
        // whatever directory the daemon happened to start in.
        if !root.starts_with('/') {
            return Err(ProtoError::invalid(
                "A workspace root must be an absolute path",
            ));
        }

        let workspace = Workspace::new(name, root, now_ms);
        let id = workspace.id.clone();
        self.store.workspaces().save(&workspace).map_err(store)?;
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
        self.store.workspaces().save(&copy).map_err(store)?;
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
