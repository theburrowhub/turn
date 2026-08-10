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
        self.bump_hierarchy();
        self.push_hierarchy_all(now_ms);
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
        self.bump_hierarchy();
        self.push_hierarchy_all(now_ms);
        self.answer_workspace(id, now_ms)
    }

    pub(super) fn archive_workspace(
        &mut self,
        id: &WorkspaceId,
        archived: bool,
        now_ms: i64,
    ) -> Answer {
        if archived
            && self.sessions.values().any(|session| {
                session.workspace_id == *id && session.tree.iter().any(|node| node.is_running())
            })
        {
            return Err(ProtoError::new(
                turn_proto::ErrorCode::Conflict,
                "Stop every Session process before archiving this Workspace",
            ));
        }
        if archived
            && self
                .store
                .hierarchy()
                .active_lease(id)
                .map_err(store)?
                .is_some()
        {
            return Err(ProtoError::new(
                turn_proto::ErrorCode::Conflict,
                "Release this Workspace's primary-checkout write lease before archiving it",
            ));
        }
        if archived {
            let sessions: Vec<SessionId> = self
                .sessions
                .values()
                .filter(|session| session.workspace_id == *id)
                .map(|session| session.id.clone())
                .collect();
            for session_id in sessions {
                self.clear_session_temporary_bindings(&session_id, now_ms)?;
            }
        }
        let workspace = self
            .workspaces
            .get_mut(id)
            .ok_or_else(|| ProtoError::not_found("workspace", id.as_str()))?;
        workspace.archived = archived;
        let workspace = workspace.clone();
        self.store.workspaces().save(&workspace).map_err(store)?;
        // Archiving is a filing decision, not an instruction to stop work. A live
        // write owner was rejected above so filing cannot hide checkout authority.
        self.bump_hierarchy();
        self.push_hierarchy_all(now_ms);
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
        self.bump_hierarchy();
        self.push_hierarchy_all(now_ms);
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
        // No pre-validation pass. There used to be one, and its reason was sound for the
        // behaviour it guarded: stopping halfway through leaves a partially stopped
        // Workspace, so better to refuse before touching the first Session. Ending no
        // longer stops halfway — an unreachable process is reported, not an obstacle — so
        // the only thing a pre-pass could still do is refuse the whole act because of a
        // process in the fourth Session that nobody can stop anyway.
        let mut escaped = Vec::new();
        for session in &sessions {
            match self.close_session(session, disposition, now_ms) {
                Ok(Response::Closed { escaped: from_one }) => {
                    super::sessions::merge_escaped(&mut escaped, from_one)
                }
                Ok(_) => {}
                // One Session failing is not the rest of them keeping their processes.
                // `KeepProcesses` is the strict disposition and may still refuse; the
                // destructive ones no longer return an error for anything but a Session
                // that is not there.
                Err(error) if disposition == CloseDisposition::KeepProcesses => return Err(error),
                Err(error) => {
                    tracing::warn!(%error, session = %session, "could not end this Session with its Workspace");
                }
            }
        }
        // The Sessions' rows leave the tree — that is what ending each one does — and the
        // Workspace's row stays.
        //
        // That asymmetry is deliberate. A Session is a *task*: finishing it means it is over, so
        // its row goes. A Workspace is a *project* — a directory the user will come back to —
        // and filing the project away because its last task stopped would mean restoring it
        // before starting the next one. The ways to get a Workspace out of the tree are its own:
        // `ArchiveWorkspace` reversibly, `DeleteWorkspace` for good. This request stops work,
        // which is why what it is called in the window is "Stop all sessions".
        self.bump_hierarchy();
        self.push_hierarchy_all(now_ms);
        Ok(Response::Closed { escaped })
    }

    /// Removes a Workspace and everything under it from Turn for good.
    ///
    /// Its Sessions cannot outlive it, so they are deleted one by one through
    /// [`Self::delete_session`] rather than left to the database's cascade: each one has
    /// processes to stop, clients to detach and a scratch directory to remove, and none of
    /// that is the schema's job. The Workspace row goes last, once nothing depends on it.
    ///
    /// A process Turn cannot stop does not stop this either, for the same reason it no longer
    /// stops `close_workspace`. Deleting is the most authoritative thing in the protocol; if
    /// ending a Session cannot be vetoed by a survivor of a previous daemon, forgetting one
    /// certainly cannot. The survivors come back in the answer so the user knows to go and look.
    ///
    /// The checkout is not touched. It is a directory the user chose, Turn does not own it, and
    /// no file, branch or worktree in it is removed. Every surface that offers this has to say
    /// so, because "delete workspace" without that sentence sounds like a question about their
    /// code rather than about Turn's record of it.
    pub(super) fn delete_workspace(
        &mut self,
        id: &WorkspaceId,
        disposition: CloseDisposition,
        now_ms: i64,
    ) -> Answer {
        if matches!(disposition, CloseDisposition::KeepProcesses) {
            return Err(ProtoError::refused(
                "Deleting a Workspace cannot keep its processes running",
            )
            .with_detail(
                "Nothing would name them afterwards. Stop them, or close the Workspace \
                 instead of deleting it.",
            ));
        }
        let Ok(workspace) = self.workspace(id) else {
            tracing::info!(workspace = %id, "delete asked for a Workspace that is already gone");
            return Ok(Response::Closed {
                escaped: Vec::new(),
            });
        };
        let name = workspace.name.clone();
        let sessions: Vec<SessionId> = self
            .sessions
            .values()
            .filter(|session| &session.workspace_id == id)
            .map(|session| session.id.clone())
            .collect();
        let mut escaped = Vec::new();
        for session in &sessions {
            match self.delete_session(session, disposition, now_ms) {
                Ok(Response::Closed { escaped: from_one }) => {
                    super::sessions::merge_escaped(&mut escaped, from_one)
                }
                Ok(_) => {}
                // The Workspace row is going regardless. A Session that would not delete
                // is a row in a table, and leaving the Workspace half gone because of it
                // is the state with no name in the UI that this used to guard against.
                Err(error) => {
                    tracing::error!(%error, session = %session, "could not delete this Session with its Workspace");
                }
            }
        }

        self.store.workspaces().delete(id).map_err(store)?;
        self.workspaces.remove(id);
        tracing::info!(workspace = %id, %name, sessions = sessions.len(), "deleted");
        self.bump_hierarchy();
        self.push_hierarchy_all(now_ms);
        Ok(Response::Closed { escaped })
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
    use turn_core::model::PaneKind;
    use turn_proto::{NewPane, Request, ServerEvent, ServerMessage};

    fn only_hierarchy_push(
        frames: &mut tokio::sync::mpsc::Receiver<turn_proto::ServerFrame>,
    ) -> turn_proto::HierarchySnapshot {
        let pushes: Vec<_> = std::iter::from_fn(|| frames.try_recv().ok())
            .filter_map(|frame| match frame.message {
                ServerMessage::Event {
                    event: ServerEvent::HierarchyChanged { snapshot },
                } => Some(*snapshot),
                _ => None,
            })
            .collect();
        assert_eq!(pushes.len(), 1, "one mutation has one authoritative push");
        pushes.into_iter().next().unwrap()
    }

    #[tokio::test]
    async fn workspace_mutations_advance_and_publish_the_unified_hierarchy() {
        let mut harness = Harness::new().await;
        let (client, mut frames) = harness.add_client(16);
        let initial_revision = match harness
            .core
            .dispatch(
                client,
                Request::GetHierarchy {
                    surface_id: "workspace-window".into(),
                    include_archived: false,
                },
                1,
            )
            .unwrap()
        {
            Response::Hierarchy { snapshot } => snapshot.revision,
            other => panic!("unexpected {other:?}"),
        };

        let root = harness._dir.path().join("published-workspace");
        std::fs::create_dir(&root).unwrap();
        let workspace_id = match harness
            .core
            .create_workspace("first name".into(), root.to_string_lossy().into_owned(), 2)
            .unwrap()
        {
            Response::Workspace { workspace } => workspace.id,
            other => panic!("unexpected {other:?}"),
        };
        let created = only_hierarchy_push(&mut frames);
        assert_eq!(created.revision, initial_revision + 1);
        assert_eq!(created.workspaces[0].workspace.id, workspace_id);

        harness
            .core
            .rename_workspace(&workspace_id, "published name".into(), 3)
            .unwrap();
        let renamed = only_hierarchy_push(&mut frames);
        assert_eq!(renamed.revision, created.revision + 1);
        assert_eq!(renamed.workspaces[0].workspace.name, "published name");

        harness
            .core
            .archive_workspace(&workspace_id, true, 4)
            .unwrap();
        let archived = only_hierarchy_push(&mut frames);
        assert_eq!(archived.revision, renamed.revision + 1);
        assert!(archived.workspaces.is_empty());

        harness
            .core
            .archive_workspace(&workspace_id, false, 5)
            .unwrap();
        let restored = only_hierarchy_push(&mut frames);
        assert_eq!(restored.revision, archived.revision + 1);
        assert_eq!(restored.workspaces[0].workspace.id, workspace_id);
    }

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

    #[tokio::test]
    async fn a_workspace_with_an_unreleased_writer_cannot_be_archived() {
        let mut harness = Harness::new().await;
        let root = harness._dir.path().join("archive-root");
        std::fs::create_dir(&root).unwrap();
        let workspace = match harness
            .core
            .create_workspace(
                "visible authority".into(),
                root.to_string_lossy().into_owned(),
                10,
            )
            .unwrap()
        {
            Response::Workspace { workspace } => workspace.id,
            other => panic!("unexpected {other:?}"),
        };
        harness
            .core
            .create_session(
                &workspace,
                "writer".into(),
                None,
                Some(vec![NewPane::new(PaneKind::AgentTree)]),
                None,
                Vec::new(),
                11,
            )
            .unwrap();
        let lease = harness
            .core
            .store
            .hierarchy()
            .active_lease(&workspace)
            .unwrap()
            .expect("the main Session owns the primary checkout");

        let error = harness
            .core
            .archive_workspace(&workspace, true, 12)
            .expect_err("archiving must not hide a live write owner");
        assert_eq!(error.code, turn_proto::ErrorCode::Conflict);
        assert!(!harness.core.workspaces[&workspace].archived);
        assert!(
            !harness
                .core
                .store
                .workspaces()
                .get(&workspace)
                .unwrap()
                .unwrap()
                .archived
        );

        harness
            .core
            .release_workspace_write_lease(&workspace, &lease.id, lease.generation, 13)
            .unwrap();
        harness
            .core
            .archive_workspace(&workspace, true, 14)
            .unwrap();
        assert!(harness.core.workspaces[&workspace].archived);
    }
}
