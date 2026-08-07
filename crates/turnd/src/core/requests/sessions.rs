//! Session and template operations.

use super::workspaces::store;
use super::{check_name, Answer};
use crate::core::Core;
use crate::paths;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command as SystemCommand;
use turn_core::ids::{CheckoutId, NodeId, SessionId, TemplateId, WorkspaceId};
use turn_core::model::{
    Direction, Layout, Pane, PaneKind, Session, SessionMode, SessionStatus, Template, Workspace,
    WorkspaceCheckout,
};
use turn_core::state::Lifecycle;
use turn_proto::{
    CloseDisposition, ErrorCode, NewPane, ProtoError, Response, ServerEvent, TemplateSummary,
};

impl Core {
    /// Proves that a destructive close can actually signal every process it would claim
    /// to stop. A restored orphan has a PID-shaped observation but no owned handle; PID
    /// reuse makes signalling it blindly unsafe, and fabricating an exit would release
    /// checkout authority while the real process may still be writing.
    pub(crate) fn ensure_session_processes_stoppable(
        &self,
        id: &SessionId,
        disposition: CloseDisposition,
    ) -> Result<(), ProtoError> {
        if disposition == CloseDisposition::KeepProcesses {
            return Ok(());
        }
        let session = self.session(id)?;
        let unreachable: Vec<String> = session
            .tree
            .iter()
            .filter(|node| {
                if !node.is_running() || self.processes.contains_key(&node.id) {
                    return false;
                }
                // A PID is an independent runtime boundary. Even when its edge was
                // discovered below an owned PTY, it may have detached from that process
                // group; Turn must not claim it died merely because the parent did.
                node.lifecycle == Lifecycle::Orphaned || node.pid.is_some()
            })
            .map(|node| format!("{} ({})", node.title, node.id))
            .collect();
        if unreachable.is_empty() {
            return Ok(());
        }
        Err(ProtoError::new(
            ErrorCode::Conflict,
            "Turn cannot safely stop processes that survived the previous daemon",
        )
        .with_detail(format!(
            "Stop these processes outside Turn, then retry: {}",
            unreachable.join(", ")
        )))
    }

    /// Proves that one Pane's exact runtime can be signalled before removing its view.
    /// A semantic child can be retired through an owned ancestor when ending the whole
    /// Session, but closing one Pane has no such authority: it must own that node's PTY.
    pub(crate) fn ensure_node_process_stoppable(
        &self,
        session_id: &SessionId,
        node_id: &NodeId,
        disposition: CloseDisposition,
    ) -> Result<(), ProtoError> {
        if disposition == CloseDisposition::KeepProcesses {
            return Ok(());
        }
        let node = self
            .session(session_id)?
            .tree
            .get(node_id)
            .ok_or_else(|| ProtoError::not_found("process", node_id.as_str()))?;
        if !node.is_running() || self.processes.contains_key(node_id) {
            return Ok(());
        }
        Err(ProtoError::new(
            ErrorCode::Conflict,
            "Turn cannot safely stop this process because it does not own its terminal",
        )
        .with_detail("Stop the process outside Turn, then retry closing the Pane"))
    }

    /// Removes surface-scoped temporary views for a Session without touching the
    /// underlying Agents. This is shared by detach/end/archive paths so no hidden
    /// binding or output pump survives after its Session leaves the visible desk.
    pub(crate) fn clear_session_temporary_bindings(
        &mut self,
        session_id: &SessionId,
        now_ms: i64,
    ) -> Result<(), ProtoError> {
        self.clear_temporary_bindings(session_id, None, now_ms)
    }

    /// Removes temporary views for a precise set of retiring nodes. Relaunch uses this
    /// after the replacement process has started so an ephemeral preview cannot remain
    /// bound to a node that is about to leave the tree.
    pub(crate) fn clear_node_temporary_bindings(
        &mut self,
        session_id: &SessionId,
        node_ids: &[NodeId],
        now_ms: i64,
    ) -> Result<(), ProtoError> {
        let node_ids: HashSet<_> = node_ids.iter().cloned().collect();
        self.clear_temporary_bindings(session_id, Some(&node_ids), now_ms)
    }

    fn clear_temporary_bindings(
        &mut self,
        session_id: &SessionId,
        node_ids: Option<&HashSet<NodeId>>,
        now_ms: i64,
    ) -> Result<(), ProtoError> {
        let bindings: Vec<_> = self
            .store
            .hierarchy()
            .bindings_for_session(session_id)
            .map_err(store)?
            .into_iter()
            .filter(|binding| {
                binding.temporary
                    && node_ids.is_none_or(|node_ids| node_ids.contains(&binding.node_id))
            })
            .collect();
        if bindings.is_empty() {
            return Ok(());
        }
        let affected_nodes: HashSet<_> = bindings
            .iter()
            .map(|binding| binding.node_id.clone())
            .collect();
        for binding in bindings {
            self.detach_everyone(session_id, &binding.pane_id);
            self.store
                .hierarchy()
                .unbind_pane(session_id, &binding.pane_id)
                .map_err(store)?;
        }
        self.bump_hierarchy();
        for node_id in affected_nodes {
            self.stop_pump_if_unwatched(&node_id);
            self.push_pane_bindings(session_id, &node_id, now_ms);
        }
        Ok(())
    }

    /// Creates a session and starts the processes its panes describe.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn create_session(
        &mut self,
        workspace_id: &WorkspaceId,
        name: String,
        cwd: Option<String>,
        panes: Option<Vec<NewPane>>,
        note: Option<String>,
        tags: Vec<String>,
        now_ms: i64,
    ) -> Answer {
        let name = check_name(&name)?;
        let workspace = self.workspace(workspace_id)?.clone();
        require_workspace_accepts_sessions(&workspace)?;
        let cwd = cwd
            .filter(|cwd| !cwd.trim().is_empty())
            .unwrap_or_else(|| workspace.root.clone());

        let layout = match panes {
            Some(panes) if !panes.is_empty() => layout_from_panes(&panes),
            // A session with nothing in it has nowhere to type. One shell is the
            // smallest thing that is still a working session.
            _ => Layout::single(default_shell_pane()),
        };

        let mut session = Session::new(workspace_id.clone(), name, cwd, layout, now_ms);
        session.note = note;
        session.tags = tags;
        session.attention = workspace.attention.clone();
        session.env = workspace.env.clone();
        session.mode = SessionMode::MainCheckout;
        // Resolve Session and Pane cwds before the transaction that acquires the
        // primary checkout lease. A path escape must leave no Session row, lease,
        // init command, or PTY behind.
        session.cwd = self.validate_session_definition_cwds(&session)?;
        let id = session.id.clone();
        // The store arbitrates and persists this Session in one IMMEDIATE
        // transaction. Nothing user-configured is executed before the exclusive
        // primary-checkout lease exists.
        self.store
            .hierarchy()
            .create_session(&session, now_ms)
            .map_err(|error| self.map_lease_store_error(workspace_id, Some(&id), error))?;
        self.sessions.insert(id.clone(), session);

        self.run_init_commands(&id, &workspace.init_commands.clone(), now_ms);
        self.materialise_session(&id, now_ms);
        self.touch_workspace(workspace_id, now_ms);
        self.persist_session(&id)?;
        self.bump_hierarchy();
        self.push_hierarchy_all(now_ms);
        self.answer_session(&id, now_ms)
    }

    /// Creates the explicit safe alternative to a second checkout writer.
    ///
    /// On a supported platform every configured process starts inside an inherited
    /// OS write guard. Unsupported platforms retain the safe degraded mode: the
    /// Session is persisted visibly unenforced and no process is launched.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn create_read_only_session(
        &mut self,
        workspace_id: &WorkspaceId,
        name: String,
        cwd: Option<String>,
        panes: Option<Vec<NewPane>>,
        note: Option<String>,
        tags: Vec<String>,
        now_ms: i64,
    ) -> Answer {
        let name = check_name(&name)?;
        let workspace = self.workspace(workspace_id)?.clone();
        require_workspace_accepts_sessions(&workspace)?;
        let cwd = cwd
            .filter(|cwd| !cwd.trim().is_empty())
            .unwrap_or_else(|| workspace.root.clone());
        let layout = match panes {
            Some(panes) if !panes.is_empty() => layout_from_panes(&panes),
            _ => Layout::single(default_shell_pane()),
        };
        let mut session = Session::new(workspace_id.clone(), name, cwd, layout, now_ms);
        session.note = note;
        session.tags = tags;
        session.attention = workspace.attention.clone();
        session.env = workspace.env.clone();
        session.mode = SessionMode::ReadOnly;
        session.cwd = self.validate_session_definition_cwds(&session)?;
        session.read_only_enforced = self.read_only_sandbox(&session)?.is_some();
        let enforced = session.read_only_enforced;
        let id = session.id.clone();
        self.store
            .hierarchy()
            .create_read_only_session(&session, enforced)
            .map_err(store)?;
        self.sessions.insert(id.clone(), session);
        if enforced {
            self.run_init_commands(&id, &workspace.init_commands.clone(), now_ms);
            self.materialise_session(&id, now_ms);
            self.persist_session(&id)?;
        }
        self.touch_workspace(workspace_id, now_ms);
        self.bump_hierarchy();
        self.push_hierarchy_all(now_ms);
        self.answer_session(&id, now_ms)
    }

    /// Creates the safe read-only alternative from the same authoritative
    /// Template request that lost the primary-checkout lease race.
    ///
    /// The client supplies only Template identity and interpolation inputs. It
    /// cannot flatten a summary back into panes, environment or policy. As with
    /// ordinary read-only Sessions, configured commands launch only when a technical
    /// write guard is available.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn create_read_only_session_from_template(
        &mut self,
        workspace_id: &WorkspaceId,
        template_id: &TemplateId,
        name: Option<String>,
        cwd: Option<String>,
        branch: Option<String>,
        task: Option<String>,
        now_ms: i64,
    ) -> Answer {
        let workspace = self.workspace(workspace_id)?.clone();
        require_workspace_accepts_sessions(&workspace)?;
        let template = self
            .templates
            .get(template_id)
            .ok_or_else(|| ProtoError::not_found("template", template_id.as_str()))?
            .clone();
        let cwd = cwd
            .filter(|cwd| !cwd.trim().is_empty())
            .unwrap_or_else(|| workspace.root.clone());
        let git_branch = branch.clone();
        let mut session = instantiate_template_session(
            workspace_id,
            &workspace,
            &template,
            name,
            cwd,
            branch.as_deref(),
            task.as_deref(),
            git_branch,
            SessionMode::ReadOnly,
            now_ms,
        )?;
        session.cwd = self.validate_session_definition_cwds(&session)?;
        session.read_only_enforced = self.read_only_sandbox(&session)?.is_some();
        let enforced = session.read_only_enforced;
        let id = session.id.clone();
        self.store
            .hierarchy()
            .create_read_only_session(&session, enforced)
            .map_err(store)?;
        self.sessions.insert(id.clone(), session);
        if enforced {
            let init: Vec<String> = workspace
                .init_commands
                .iter()
                .cloned()
                .chain(template.init_commands.iter().cloned())
                .collect();
            self.run_init_commands(&id, &init, now_ms);
            self.materialise_session(&id, now_ms);
            self.persist_session(&id)?;
        }
        self.touch_workspace(workspace_id, now_ms);
        self.bump_hierarchy();
        self.push_hierarchy_all(now_ms);
        self.answer_session(&id, now_ms)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn create_worktree_session(
        &mut self,
        workspace_id: &WorkspaceId,
        name: String,
        branch: String,
        worktree_path: Option<String>,
        panes: Option<Vec<NewPane>>,
        note: Option<String>,
        tags: Vec<String>,
        now_ms: i64,
    ) -> Answer {
        let name = check_name(&name)?;
        let branch = validate_git_branch(&branch)?;
        let workspace = self.workspace(workspace_id)?.clone();
        require_workspace_accepts_sessions(&workspace)?;
        let path = resolve_worktree_path(
            &self.data_dir,
            workspace_id,
            &branch,
            worktree_path.as_deref(),
        )?;
        if path.exists() {
            return Err(ProtoError::new(
                turn_proto::ErrorCode::Conflict,
                "The isolated worktree path already exists",
            )
            .with_detail(path.display().to_string()));
        }
        let parent = path
            .parent()
            .ok_or_else(|| ProtoError::invalid("The worktree path needs a parent directory"))?;
        std::fs::create_dir_all(parent).map_err(|error| {
            ProtoError::new(
                turn_proto::ErrorCode::Unavailable,
                "The worktree parent directory could not be created",
            )
            .with_detail(error.to_string())
        })?;
        create_git_worktree(Path::new(&workspace.root), &path, &branch)?;

        let canonical = std::fs::canonicalize(&path).map_err(|error| {
            rollback_git_worktree(Path::new(&workspace.root), &path);
            ProtoError::new(
                turn_proto::ErrorCode::Unavailable,
                "The new worktree could not be resolved",
            )
            .with_detail(error.to_string())
        })?;
        let layout = match panes {
            Some(panes) if !panes.is_empty() => layout_from_panes(&panes),
            _ => Layout::single(default_shell_pane()),
        };
        let mut session = Session::new(
            workspace_id.clone(),
            name,
            canonical.to_string_lossy(),
            layout,
            now_ms,
        );
        session.note = note;
        session.tags = tags;
        session.attention = workspace.attention.clone();
        session.env = workspace.env.clone();
        session.mode = SessionMode::IsolatedWorktree;
        session.checkout_id = CheckoutId::new();
        session.worktree_path = Some(canonical.to_string_lossy().to_string());
        session.git_branch = Some(branch.clone());
        let id = session.id.clone();
        let inherited_resources = self
            .store
            .hierarchy()
            .primary_checkout(workspace_id)
            .map_err(store)?
            .map(|checkout| checkout.shared_resources)
            .unwrap_or_default();
        let checkout = WorkspaceCheckout {
            id: session.checkout_id.clone(),
            workspace_id: workspace_id.clone(),
            path: canonical.to_string_lossy().to_string(),
            canonical_path: canonical.to_string_lossy().to_string(),
            branch: Some(branch),
            primary: false,
            shared_resources: inherited_resources,
            created_ms: now_ms,
        };
        session.cwd = match self.validate_session_definition_cwds_for_checkout(&session, &checkout)
        {
            Ok(cwd) => cwd,
            Err(error) => {
                rollback_git_worktree(Path::new(&workspace.root), &path);
                return Err(error);
            }
        };
        if let Err(error) = self
            .store
            .hierarchy()
            .create_worktree_session(&session, &checkout)
        {
            rollback_git_worktree(Path::new(&workspace.root), &path);
            return Err(store(error));
        }
        self.sessions.insert(id.clone(), session);
        self.run_init_commands(&id, &workspace.init_commands.clone(), now_ms);
        self.materialise_session(&id, now_ms);
        self.touch_workspace(workspace_id, now_ms);
        self.persist_session(&id)?;
        self.bump_hierarchy();
        self.push_hierarchy_all(now_ms);
        self.answer_session(&id, now_ms)
    }

    /// Creates the isolated alternative from an authoritative Template rather
    /// than from a client's lossy summary of it.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn create_worktree_session_from_template(
        &mut self,
        workspace_id: &WorkspaceId,
        template_id: &TemplateId,
        name: Option<String>,
        cwd: Option<String>,
        template_branch: Option<String>,
        task: Option<String>,
        branch: String,
        worktree_path: Option<String>,
        now_ms: i64,
    ) -> Answer {
        let branch = validate_git_branch(&branch)?;
        let workspace = self.workspace(workspace_id)?.clone();
        require_workspace_accepts_sessions(&workspace)?;
        let template = self
            .templates
            .get(template_id)
            .ok_or_else(|| ProtoError::not_found("template", template_id.as_str()))?
            .clone();
        // Validate the user-visible name before Git or the filesystem is changed.
        let name = render_template_session_name(
            &template,
            name,
            template_branch.as_deref(),
            task.as_deref(),
        )?;
        let path = resolve_worktree_path(
            &self.data_dir,
            workspace_id,
            &branch,
            worktree_path.as_deref(),
        )?;
        if path.exists() {
            return Err(ProtoError::new(
                turn_proto::ErrorCode::Conflict,
                "The isolated worktree path already exists",
            )
            .with_detail(path.display().to_string()));
        }
        let parent = path
            .parent()
            .ok_or_else(|| ProtoError::invalid("The worktree path needs a parent directory"))?;
        std::fs::create_dir_all(parent).map_err(|error| {
            ProtoError::new(
                turn_proto::ErrorCode::Unavailable,
                "The worktree parent directory could not be created",
            )
            .with_detail(error.to_string())
        })?;
        create_git_worktree(Path::new(&workspace.root), &path, &branch)?;

        let canonical = std::fs::canonicalize(&path).map_err(|error| {
            rollback_git_worktree(Path::new(&workspace.root), &path);
            ProtoError::new(
                turn_proto::ErrorCode::Unavailable,
                "The new worktree could not be resolved",
            )
            .with_detail(error.to_string())
        })?;
        let session_cwd = match remap_template_cwd_to_worktree(
            Path::new(&workspace.root),
            &canonical,
            cwd.as_deref(),
        ) {
            Ok(cwd) => cwd,
            Err(error) => {
                rollback_git_worktree(Path::new(&workspace.root), &path);
                return Err(error);
            }
        };
        let mut session = match instantiate_template_session(
            workspace_id,
            &workspace,
            &template,
            Some(name),
            session_cwd,
            template_branch.as_deref(),
            task.as_deref(),
            Some(branch.clone()),
            SessionMode::IsolatedWorktree,
            now_ms,
        ) {
            Ok(session) => session,
            Err(error) => {
                rollback_git_worktree(Path::new(&workspace.root), &path);
                return Err(error);
            }
        };
        if let Err(error) = remap_absolute_template_pane_cwds(
            &mut session.layout,
            Path::new(&workspace.root),
            &canonical,
        ) {
            rollback_git_worktree(Path::new(&workspace.root), &path);
            return Err(error);
        }
        session.checkout_id = CheckoutId::new();
        session.worktree_path = Some(canonical.to_string_lossy().to_string());
        let id = session.id.clone();
        let inherited_resources = match self
            .store
            .hierarchy()
            .primary_checkout(workspace_id)
            .map_err(store)
        {
            Ok(checkout) => checkout
                .map(|checkout| checkout.shared_resources)
                .unwrap_or_default(),
            Err(error) => {
                rollback_git_worktree(Path::new(&workspace.root), &path);
                return Err(error);
            }
        };
        let checkout = WorkspaceCheckout {
            id: session.checkout_id.clone(),
            workspace_id: workspace_id.clone(),
            path: canonical.to_string_lossy().to_string(),
            canonical_path: canonical.to_string_lossy().to_string(),
            branch: Some(branch),
            primary: false,
            shared_resources: inherited_resources,
            created_ms: now_ms,
        };
        session.cwd = match self.validate_session_definition_cwds_for_checkout(&session, &checkout)
        {
            Ok(cwd) => cwd,
            Err(error) => {
                rollback_git_worktree(Path::new(&workspace.root), &path);
                return Err(error);
            }
        };
        if let Err(error) = self
            .store
            .hierarchy()
            .create_worktree_session(&session, &checkout)
        {
            rollback_git_worktree(Path::new(&workspace.root), &path);
            return Err(store(error));
        }
        self.sessions.insert(id.clone(), session);
        let init: Vec<String> = workspace
            .init_commands
            .iter()
            .cloned()
            .chain(template.init_commands.iter().cloned())
            .collect();
        self.run_init_commands(&id, &init, now_ms);
        self.materialise_session(&id, now_ms);
        self.touch_workspace(workspace_id, now_ms);
        self.persist_session(&id)?;
        self.bump_hierarchy();
        self.push_hierarchy_all(now_ms);
        self.answer_session(&id, now_ms)
    }

    /// Creates a session from a template.
    ///
    /// The layout comes from [`Template::instantiate`], which mints fresh pane ids, so
    /// two sessions made from one template never share identity.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn create_session_from_template(
        &mut self,
        workspace_id: &WorkspaceId,
        template_id: &TemplateId,
        name: Option<String>,
        cwd: Option<String>,
        branch: Option<String>,
        task: Option<String>,
        now_ms: i64,
    ) -> Answer {
        let workspace = self.workspace(workspace_id)?.clone();
        require_workspace_accepts_sessions(&workspace)?;
        let template = self
            .templates
            .get(template_id)
            .ok_or_else(|| ProtoError::not_found("template", template_id.as_str()))?
            .clone();

        let name = match name {
            Some(name) => check_name(&name)?,
            None => template
                .render_name(branch.as_deref(), task.as_deref())
                .unwrap_or_else(|| template.name.clone()),
        };
        let cwd = cwd
            .filter(|cwd| !cwd.trim().is_empty())
            .unwrap_or_else(|| workspace.root.clone());

        let mut session = Session::new(
            workspace_id.clone(),
            name,
            cwd,
            template.instantiate(),
            now_ms,
        );
        session.template_id = Some(template.id.clone());
        // A template's own policy overrides the workspace's; without one the workspace
        // decides, which is what makes a workspace-wide "stay quiet" setting mean
        // something.
        session.attention = template
            .attention
            .clone()
            .unwrap_or_else(|| workspace.attention.clone());
        session.env = workspace
            .env
            .iter()
            .cloned()
            .chain(template.env.iter().cloned())
            .collect();
        session.tmux = template.tmux;
        session.git_branch = branch;
        session.mode = SessionMode::MainCheckout;
        // Template Pane cwds are untrusted configuration at this boundary. Resolve
        // every one before a lease or a template init command has side effects.
        session.cwd = self.validate_session_definition_cwds(&session)?;
        let id = session.id.clone();
        self.store
            .hierarchy()
            .create_session(&session, now_ms)
            .map_err(|error| self.map_lease_store_error(workspace_id, Some(&id), error))?;
        self.sessions.insert(id.clone(), session);

        let init: Vec<String> = workspace
            .init_commands
            .iter()
            .cloned()
            .chain(template.init_commands.iter().cloned())
            .collect();
        self.run_init_commands(&id, &init, now_ms);
        self.materialise_session(&id, now_ms);
        self.touch_workspace(workspace_id, now_ms);
        self.persist_session(&id)?;
        self.bump_hierarchy();
        self.push_hierarchy_all(now_ms);
        self.answer_session(&id, now_ms)
    }

    pub(super) fn rename_session(&mut self, id: &SessionId, name: String, now_ms: i64) -> Answer {
        let name = check_name(&name)?;
        self.session_mut(id)?.name = name;
        self.persist_session(id)?;
        self.push_session_state(id, now_ms);
        self.answer_session(id, now_ms)
    }

    /// Files a session away, or brings it back.
    ///
    /// Processes are untouched either way: archiving is about the sidebar. A Session
    /// that owns the primary checkout cannot be hidden while its write lease remains
    /// live, because clients exclude archived Sessions from normal navigation and the
    /// authority would become impossible to reach or release. The user must stop or
    /// detach write-capable work and release the lease first.
    pub(super) fn archive_session(
        &mut self,
        id: &SessionId,
        archived: bool,
        now_ms: i64,
    ) -> Answer {
        if archived {
            let source = self.session(id)?;
            if source.tree.iter().any(|node| node.is_running()) {
                return Err(ProtoError::new(
                    ErrorCode::Conflict,
                    "End this Session's processes before archiving it",
                ));
            }
            let workspace_id = source.workspace_id.clone();
            let owns_unreleased_lease = self
                .store
                .hierarchy()
                .active_lease(&workspace_id)
                .map_err(store)?
                .is_some_and(|lease| lease.session_id == *id);
            if owns_unreleased_lease {
                return Err(ProtoError::new(
                    turn_proto::ErrorCode::Conflict,
                    "Release this Session's primary-checkout write lease before archiving it",
                ));
            }
        } else {
            let workspace_id = self.session(id)?.workspace_id.clone();
            if self.workspace(&workspace_id)?.archived {
                return Err(ProtoError::new(
                    ErrorCode::Conflict,
                    "Restore the Workspace before restoring one of its Sessions",
                ));
            }
        }
        if archived {
            self.clear_session_temporary_bindings(id, now_ms)?;
        }
        let session = self.session_mut(id)?;
        if archived {
            session.archive();
        } else {
            session.unarchive();
        }
        let workspace_id = session.workspace_id.clone();
        self.persist_session(id)?;
        if archived {
            self.push_all(ServerEvent::SessionRemoved {
                session_id: id.clone(),
                workspace_id,
            });
            self.bump_hierarchy();
            self.push_hierarchy_all(now_ms);
        } else {
            self.push_session_state(id, now_ms);
            self.bump_hierarchy();
            self.push_hierarchy_all(now_ms);
        }
        self.answer_session(id, now_ms)
    }

    /// Copies a session's shape and settings. No processes are started.
    ///
    /// The copy is a session set up for another run of the same task, which is not the
    /// same as another run: launching it is the user's next decision, not this one's.
    pub(super) fn duplicate_session(&mut self, id: &SessionId, now_ms: i64) -> Answer {
        let copy = self.session(id)?.duplicate(now_ms);
        let new_id = copy.id.clone();
        self.sessions.insert(new_id.clone(), copy);
        self.persist_session(&new_id)?;
        self.push_session_state(&new_id, now_ms);
        self.answer_session(&new_id, now_ms)
    }

    /// Closes a session, doing exactly what the disposition says.
    ///
    /// There is no default for a reason: the whole point of the daemon is that
    /// processes outlive the window, so "close" is ambiguous in a way that would either
    /// kill work the user wanted kept or leak processes they thought were gone.
    ///
    /// * `KeepProcesses` detaches the clients and leaves everything running. The
    ///   session stays in the list; reopening it re-attaches to the same ptys.
    /// * `Terminate` and `Kill` stop the processes and park the session as paused. It
    ///   stays on disk and in the list, because a stopped session is still a task the
    ///   user was working on — filing it away is [`Self::archive_session`].
    pub(super) fn close_session(
        &mut self,
        id: &SessionId,
        disposition: CloseDisposition,
        now_ms: i64,
    ) -> Answer {
        self.ensure_session_processes_stoppable(id, disposition)?;
        let session = self.session(id)?;
        let panes: Vec<turn_core::ids::PaneId> = session
            .layout
            .panes()
            .iter()
            .map(|p| p.id.clone())
            .collect();
        let nodes: Vec<turn_core::ids::NodeId> = session
            .tree
            .iter()
            .filter(|node| node.is_running() && self.processes.contains_key(&node.id))
            .map(|node| node.id.clone())
            .collect();
        // Detach every client from this session's panes whatever the disposition: the
        // session is being closed on screen in all three cases.
        for pane in &panes {
            self.detach_everyone(id, pane);
        }
        self.clear_session_temporary_bindings(id, now_ms)?;
        for node in &nodes {
            self.stop_pump_if_unwatched(node);
        }

        match disposition {
            CloseDisposition::KeepProcesses => {
                tracing::info!(session = %id, processes = nodes.len(), "closed, processes kept");
            }
            CloseDisposition::Terminate | CloseDisposition::Kill => {
                // The panes are being closed, so their ptys go too — which is what makes
                // this stop a shell that ignores `SIGTERM` rather than leaving it running
                // with nothing on screen to reach it by.
                for node in &nodes {
                    self.stop_and_release(
                        id,
                        node,
                        matches!(disposition, CloseDisposition::Kill),
                        now_ms,
                    );
                }
                let remaining: Vec<String> = self
                    .session(id)?
                    .tree
                    .iter()
                    .filter(|node| node.is_running())
                    .map(|node| format!("{} ({})", node.title, node.id))
                    .collect();
                if let Ok(session) = self.session_mut(id) {
                    if session.status != SessionStatus::Archived {
                        session.status = if remaining.is_empty() {
                            SessionStatus::Paused
                        } else {
                            SessionStatus::Active
                        };
                    }
                }
                if !remaining.is_empty() {
                    self.persist_session(id)?;
                    self.push_session_state(id, now_ms);
                    return Err(ProtoError::new(
                        ErrorCode::Conflict,
                        "Some child processes are still running outside Turn",
                    )
                    .with_detail(format!(
                        "Stop these processes outside Turn, then retry: {}",
                        remaining.join(", ")
                    )));
                }
                let restore_update = self.resolve_restore_session(id);
                // The injected agent configuration goes with the processes it was
                // written for. Nothing will read it again, and a settings file naming a
                // hook URL that no longer answers is worse than no file.
                paths::remove_session_scratch(&self.data_dir, id);
                self.persist_session(id)?;
                self.push_session_state(id, now_ms);
                if let Some(update) = restore_update {
                    self.push_all(update);
                }
            }
        }
        Ok(Response::Ack)
    }

    /// Captures a session's current arrangement as a template.
    pub(super) fn create_layout_template(
        &mut self,
        name: String,
        mut layout: Layout,
        description: Option<String>,
        now_ms: i64,
    ) -> Answer {
        let name = check_name(&name)?;
        validate_template_layout(&layout)?;
        if self
            .templates
            .values()
            .any(|template| template.name.eq_ignore_ascii_case(&name))
        {
            return Err(ProtoError::new(
                turn_proto::ErrorCode::Conflict,
                "A layout with that name already exists",
            ));
        }
        layout.normalise();
        let mut template = Template::from_layout(name, &layout, now_ms);
        template.description = description;
        self.store.templates().save(&template).map_err(store)?;
        let summary = TemplateSummary::from_template(&template);
        self.templates.insert(template.id.clone(), template);
        Ok(Response::Template { template: summary })
    }

    /// Captures a session's current arrangement as a template.
    pub(super) fn save_layout_as_template(
        &mut self,
        id: &SessionId,
        name: String,
        description: Option<String>,
        hotkey: Option<String>,
        now_ms: i64,
    ) -> Answer {
        let name = check_name(&name)?;
        let session = self.session(id)?;
        // `from_layout` strips process bindings: a template describes what to start,
        // never which instance it was captured from.
        let mut template = Template::from_layout(name, &session.layout, now_ms);
        template.description = description;
        template.hotkey = hotkey;
        template.attention = Some(session.attention.clone());
        template.env = session.env.clone();
        template.tmux = session.tmux;
        self.store.templates().save(&template).map_err(store)?;
        let summary = TemplateSummary::from_template(&template);
        self.templates.insert(template.id.clone(), template);
        Ok(Response::Template { template: summary })
    }

    /// Runs a workspace's or template's start-up commands.
    ///
    /// These are commands the *user* configured, which is what makes running them
    /// legitimate — Turn never runs something it inferred from an agent's output. Each
    /// one becomes a background node with no pane, so its exit status is visible in the
    /// tree rather than disappearing.
    fn run_init_commands(&mut self, session: &SessionId, commands: &[String], now_ms: i64) {
        for command in commands.iter().filter(|c| !c.trim().is_empty()) {
            if let Err(error) = self.spawn_init_command(session, command, now_ms) {
                tracing::warn!(%session, command, %error, "an init command could not be started");
            }
        }
    }

    fn touch_workspace(&mut self, id: &WorkspaceId, now_ms: i64) {
        if let Some(workspace) = self.workspaces.get_mut(id) {
            workspace.touch(now_ms);
            let workspace = workspace.clone();
            let _ = self.store.workspaces().save(&workspace);
        }
    }

    pub(super) fn answer_session(&self, id: &SessionId, now_ms: i64) -> Answer {
        let session = self
            .session_summary(id, now_ms)
            .ok_or_else(|| ProtoError::not_found("session", id.as_str()))?;
        Ok(Response::Session {
            session: Box::new(session),
        })
    }
}

fn require_workspace_accepts_sessions(workspace: &Workspace) -> Result<(), ProtoError> {
    if workspace.archived {
        Err(ProtoError::refused(
            "Unarchive the Workspace before creating a Session",
        ))
    } else {
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
fn instantiate_template_session(
    workspace_id: &WorkspaceId,
    workspace: &Workspace,
    template: &Template,
    name: Option<String>,
    cwd: String,
    render_branch: Option<&str>,
    task: Option<&str>,
    git_branch: Option<String>,
    mode: SessionMode,
    now_ms: i64,
) -> Result<Session, ProtoError> {
    let name = render_template_session_name(template, name, render_branch, task)?;
    let mut session = Session::new(
        workspace_id.clone(),
        name,
        cwd,
        template.instantiate(),
        now_ms,
    );
    session.template_id = Some(template.id.clone());
    session.attention = template
        .attention
        .clone()
        .unwrap_or_else(|| workspace.attention.clone());
    session.env = workspace
        .env
        .iter()
        .cloned()
        .chain(template.env.iter().cloned())
        .collect();
    session.tmux = template.tmux;
    session.git_branch = git_branch;
    session.mode = mode;
    session.note = task.map(str::to_owned);
    Ok(session)
}

fn validate_template_layout(layout: &Layout) -> Result<(), ProtoError> {
    const MAX_PANES: usize = 16;
    const MAX_DEPTH: usize = 8;
    const MAX_PROGRAM_BYTES: usize = 1_024;
    const MAX_ARGUMENTS: usize = 128;

    fn visit(
        node: &turn_core::model::LayoutNode,
        depth: usize,
        pane_ids: &mut HashSet<String>,
    ) -> Result<usize, ProtoError> {
        if depth > MAX_DEPTH {
            return Err(ProtoError::invalid("A layout may be at most 8 splits deep"));
        }
        match node {
            turn_core::model::LayoutNode::Leaf(pane) => {
                if !pane_ids.insert(pane.id.as_str().to_string()) {
                    return Err(ProtoError::invalid("A layout contains a duplicate Pane id"));
                }
                if pane
                    .command
                    .as_ref()
                    .is_some_and(|program| program.trim().is_empty())
                {
                    return Err(ProtoError::invalid(
                        "A Pane program must be omitted or contain an executable name",
                    ));
                }
                if pane
                    .command
                    .as_ref()
                    .is_some_and(|program| program.len() > MAX_PROGRAM_BYTES)
                {
                    return Err(ProtoError::invalid("A Pane program is too long"));
                }
                if pane.args.len() > MAX_ARGUMENTS {
                    return Err(ProtoError::invalid("A Pane has too many arguments"));
                }
                Ok(1)
            }
            turn_core::model::LayoutNode::Split(split) => {
                if split.children.len() < 2 {
                    return Err(ProtoError::invalid(
                        "Every layout split must contain at least two cells",
                    ));
                }
                let mut panes = 0;
                for child in &split.children {
                    if !child.size.is_finite() || child.size < 0.0 {
                        return Err(ProtoError::invalid(
                            "Every layout cell size must be a finite positive fraction",
                        ));
                    }
                    panes += visit(&child.node, depth + 1, pane_ids)?;
                }
                Ok(panes)
            }
        }
    }

    let panes = visit(&layout.root, 0, &mut HashSet::new())?;
    if panes == 0 || panes > MAX_PANES {
        return Err(ProtoError::invalid(format!(
            "A layout must contain between 1 and {MAX_PANES} cells"
        )));
    }
    Ok(())
}

fn render_template_session_name(
    template: &Template,
    name: Option<String>,
    branch: Option<&str>,
    task: Option<&str>,
) -> Result<String, ProtoError> {
    let rendered = match name {
        Some(name) => name,
        None => template
            .render_name(branch, task)
            .unwrap_or_else(|| template.name.clone()),
    };
    check_name(&rendered)
}

/// Maps a cwd from the primary checkout to the same repository-relative
/// directory in a freshly created worktree. No process is launched in the
/// primary checkout, and an absolute path outside it is never carried over.
fn remap_template_cwd_to_worktree(
    primary_root: &Path,
    worktree_root: &Path,
    requested: Option<&str>,
) -> Result<String, ProtoError> {
    let primary = std::fs::canonicalize(primary_root).map_err(|error| {
        ProtoError::refused("The primary checkout cannot be resolved safely")
            .with_detail(format!("{}: {error}", primary_root.display()))
    })?;
    let worktree = std::fs::canonicalize(worktree_root).map_err(|error| {
        ProtoError::refused("The isolated checkout cannot be resolved safely")
            .with_detail(format!("{}: {error}", worktree_root.display()))
    })?;
    let source = match requested.filter(|cwd| !cwd.trim().is_empty()) {
        Some(cwd) if Path::new(cwd).is_absolute() => PathBuf::from(cwd),
        Some(cwd) => primary.join(cwd),
        None => primary.clone(),
    };
    let source = std::fs::canonicalize(&source).map_err(|error| {
        ProtoError::refused("The Template working directory cannot be resolved safely")
            .with_detail(format!("{}: {error}", source.display()))
    })?;
    if !source.is_dir() || !source.starts_with(&primary) {
        return Err(ProtoError::refused(
            "The Template working directory is outside the primary checkout",
        )
        .with_detail(source.display().to_string()));
    }
    let relative = source.strip_prefix(&primary).map_err(|_| {
        ProtoError::refused("The Template working directory cannot be mapped safely")
    })?;
    let target = std::fs::canonicalize(worktree.join(relative)).map_err(|error| {
        ProtoError::refused("The Template working directory is absent from the worktree")
            .with_detail(format!("{}: {error}", worktree.join(relative).display()))
    })?;
    if !target.is_dir() || !target.starts_with(&worktree) {
        return Err(ProtoError::refused(
            "The mapped Template working directory is outside the worktree",
        )
        .with_detail(target.display().to_string()));
    }
    Ok(target.to_string_lossy().into_owned())
}

/// Absolute Pane cwds saved from the primary checkout need the same repository-
/// relative translation as the Session cwd. Relative Pane cwds are already
/// relative to the newly mapped Session cwd and remain byte-for-byte intact.
fn remap_absolute_template_pane_cwds(
    layout: &mut Layout,
    primary_root: &Path,
    worktree_root: &Path,
) -> Result<(), ProtoError> {
    let absolute: Vec<_> = layout
        .panes()
        .into_iter()
        .filter_map(|pane| {
            pane.cwd
                .as_ref()
                .filter(|cwd| Path::new(cwd).is_absolute())
                .map(|cwd| (pane.id.clone(), cwd.clone()))
        })
        .collect();
    for (pane_id, cwd) in absolute {
        let mapped = remap_template_cwd_to_worktree(primary_root, worktree_root, Some(&cwd))?;
        if let Some(pane) = layout.get_mut(&pane_id) {
            pane.cwd = Some(mapped);
        }
    }
    Ok(())
}

/// Builds a layout from a list of panes.
///
/// Each new pane joins the previous one's split in the same direction, which
/// [`Layout::split`] turns into siblings of one horizontal split rather than a nest of
/// lopsided pairs — so three panes are three equal columns in the order they were
/// asked for, and the dividers line up.
fn layout_from_panes(specs: &[NewPane]) -> Layout {
    let mut iter = specs.iter();
    // An empty list is the same request as no list at all: a session with nothing in it
    // has nowhere to type.
    let first = match iter.next() {
        Some(spec) => pane_from_spec(spec),
        None => default_shell_pane(),
    };
    let first_id = first.id.clone();
    let mut layout = Layout::single(first);
    let mut previous = first_id.clone();

    for spec in iter {
        let pane = pane_from_spec(spec);
        let id = pane.id.clone();
        if layout.split(&previous, Direction::Horizontal, pane) {
            previous = id;
        }
    }
    layout.active = Some(first_id);
    layout.normalise();
    layout
}

/// The pane a session falls back to: one shell, safe to bring back on restore.
fn default_shell_pane() -> Pane {
    Pane::new(PaneKind::Shell)
        .with_title("shell")
        .with_restore(turn_core::model::RestoreBehaviour::Relaunch)
}

/// Turns a client's pane request into a pane, with an id the daemon minted.
pub(super) fn pane_from_spec(spec: &NewPane) -> Pane {
    let mut pane = Pane::new(spec.kind);
    pane.title = spec.title.clone();
    // A title supplied in an API request is an explicit user choice. Built-in and
    // saved templates construct Panes directly and retain their lower-priority
    // fallback titles.
    pane.title_is_user_set = spec.title.is_some();
    pane.command = spec.command.clone();
    pane.args = spec.args.clone();
    pane.cwd = spec.cwd.clone();
    pane.env = spec.env.clone();
    pane.restore = spec.restore;
    pane
}

fn validate_git_branch(raw: &str) -> Result<String, ProtoError> {
    let branch = raw.trim();
    if branch.is_empty() || branch.chars().count() > 200 {
        return Err(ProtoError::invalid(
            "A worktree branch must contain between 1 and 200 characters",
        ));
    }
    let output = SystemCommand::new("git")
        .args(["check-ref-format", "--branch", branch])
        .output()
        .map_err(|error| {
            ProtoError::new(
                turn_proto::ErrorCode::Unavailable,
                "Git is required to validate an isolated worktree branch",
            )
            .with_detail(error.to_string())
        })?;
    if !output.status.success() {
        return Err(ProtoError::invalid("That is not a valid Git branch name"));
    }
    Ok(branch.to_string())
}

fn resolve_worktree_path(
    data_dir: &Path,
    workspace_id: &WorkspaceId,
    branch: &str,
    requested: Option<&str>,
) -> Result<PathBuf, ProtoError> {
    let path = match requested.map(str::trim).filter(|path| !path.is_empty()) {
        Some(path) => PathBuf::from(path),
        None => {
            let slug: String = branch
                .chars()
                .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
                .collect();
            paths::worktree_root(data_dir, workspace_id).join(slug)
        }
    };
    if !path.is_absolute() {
        return Err(ProtoError::invalid(
            "An explicit worktree path must be absolute",
        ));
    }
    if path.parent().is_none() || path.file_name().is_none() {
        return Err(ProtoError::invalid(
            "The filesystem root cannot be used as a worktree",
        ));
    }
    if path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(ProtoError::invalid(
            "A worktree path cannot contain parent-directory components",
        ));
    }
    Ok(path)
}

fn create_git_worktree(root: &Path, target: &Path, branch: &str) -> Result<(), ProtoError> {
    let repository = SystemCommand::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|error| {
            ProtoError::new(
                turn_proto::ErrorCode::Unavailable,
                "Git could not inspect the Workspace repository",
            )
            .with_detail(error.to_string())
        })?;
    if !repository.status.success() {
        return Err(ProtoError::new(
            turn_proto::ErrorCode::Conflict,
            "The Workspace root is not inside a Git repository",
        ));
    }
    let branch_exists = SystemCommand::new("git")
        .arg("-C")
        .arg(root)
        .args(["show-ref", "--verify", "--quiet"])
        .arg(format!("refs/heads/{branch}"))
        .status()
        .map_err(|error| {
            ProtoError::new(
                turn_proto::ErrorCode::Unavailable,
                "Git could not inspect the requested branch",
            )
            .with_detail(error.to_string())
        })?
        .success();
    let mut command = SystemCommand::new("git");
    command.arg("-C").arg(root).args(["worktree", "add"]);
    if !branch_exists {
        command.arg("-b").arg(branch);
    }
    command.arg(target);
    if branch_exists {
        command.arg(branch);
    }
    let output = command.output().map_err(|error| {
        ProtoError::new(
            turn_proto::ErrorCode::Unavailable,
            "Git could not create the isolated worktree",
        )
        .with_detail(error.to_string())
    })?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr)
            .trim()
            .chars()
            .take(1_000)
            .collect::<String>();
        return Err(ProtoError::new(
            turn_proto::ErrorCode::Conflict,
            "Git refused to create the isolated worktree",
        )
        .with_detail(detail));
    }
    Ok(())
}

fn rollback_git_worktree(root: &Path, target: &Path) {
    let result = SystemCommand::new("git")
        .arg("-C")
        .arg(root)
        .args(["worktree", "remove", "--force"])
        .arg(target)
        .status();
    if result.as_ref().is_err() || result.is_ok_and(|status| !status.success()) {
        tracing::warn!(path = %target.display(), "could not roll back the new Git worktree");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::testing::Harness;
    use turn_core::ids::PaneId;
    #[cfg(target_os = "macos")]
    use turn_core::model::NodeKind;
    use turn_core::model::ProcessNode;
    use turn_core::state::Lifecycle;
    use turn_proto::{Request, ServerMessage};

    #[cfg(target_os = "macos")]
    struct GuardedProbeAgent;

    #[cfg(target_os = "macos")]
    impl turn_agents::AgentAdapter for GuardedProbeAgent {
        fn id(&self) -> &'static str {
            "guarded-probe-agent"
        }

        fn provider(&self) -> &'static str {
            "turn-test"
        }

        fn executables(&self) -> &'static [&'static str] {
            &["turn-read-only-test-agent"]
        }

        fn best_level(&self) -> turn_agents::IntegrationLevel {
            turn_agents::IntegrationLevel::Heuristic
        }

        fn capabilities(&self) -> turn_agents::Capabilities {
            turn_agents::Capabilities::default()
        }

        fn detect(&self, executable: &str) -> Option<PathBuf> {
            (executable == "turn-read-only-test-agent").then(|| PathBuf::from("/bin/sh"))
        }

        fn prepare(
            &self,
            ctx: &turn_agents::LaunchContext,
        ) -> Result<turn_agents::LaunchPlan, turn_agents::AdapterError> {
            Ok(turn_agents::LaunchPlan {
                command: "/bin/sh".into(),
                args: ctx.user_args.clone(),
                env: Vec::new(),
                level: turn_agents::IntegrationLevel::Heuristic,
                note: "test Agent executed through a real shell".into(),
            })
        }

        fn normalise(
            &self,
            _payload: &serde_json::Value,
            _ctx: &turn_agents::EventContext,
        ) -> Vec<turn_core::event::TurnEvent> {
            Vec::new()
        }
    }

    fn drain_session_events(
        frames: &mut tokio::sync::mpsc::Receiver<turn_proto::ServerFrame>,
    ) -> Vec<ServerEvent> {
        std::iter::from_fn(|| frames.try_recv().ok())
            .filter_map(|frame| match frame.message {
                ServerMessage::Event { event } => Some(event),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn closing_a_session_never_fabricates_the_death_of_an_uncontrolled_orphan() {
        let mut harness = Harness::new().await;
        let session_id = SessionId::from_stored("sess_orphan_close_guard");
        let pane_id = PaneId::from_stored("pane_orphan_close_guard");
        harness.add_session(session_id.clone(), pane_id, 10);
        let mut orphan = ProcessNode::process(
            session_id.clone(),
            turn_core::model::NodeKind::Shell,
            "sh",
            "/tmp",
            10,
        );
        orphan.lifecycle = Lifecycle::Orphaned;
        orphan.pid = Some(424_242);
        let orphan_id = harness
            .core
            .sessions
            .get_mut(&session_id)
            .unwrap()
            .tree
            .insert(orphan);

        let error = harness
            .core
            .close_session(&session_id, CloseDisposition::Terminate, 11)
            .expect_err("an unowned PID cannot be claimed as terminated");
        assert_eq!(error.code, ErrorCode::Conflict);
        assert_eq!(
            harness.core.sessions[&session_id]
                .tree
                .get(&orphan_id)
                .unwrap()
                .lifecycle,
            Lifecycle::Orphaned
        );
    }

    #[tokio::test]
    async fn a_semantic_subagent_does_not_block_ending_its_owned_parent_runtime() {
        let mut harness = Harness::new().await;
        let session_id = SessionId::from_stored("sess_owned_parent_with_subagent");
        let pane_id = PaneId::from_stored("pane_owned_parent_with_subagent");
        harness.add_session(session_id.clone(), pane_id.clone(), 10);
        let parent_id = harness.spawn_process(&session_id, &pane_id, 10).await;

        let mut child = ProcessNode::agent(session_id.clone(), "Reviewer", "/tmp", 11);
        child.kind = turn_core::model::NodeKind::Subagent;
        child.lifecycle = Lifecycle::Alive;
        child.parent = Some(parent_id.clone());
        child.relation = turn_core::model::Relation::Confirmed;
        let child_id = child.id.clone();
        harness
            .core
            .sessions
            .get_mut(&session_id)
            .unwrap()
            .tree
            .insert(child);

        let answer = harness
            .core
            .close_session(&session_id, CloseDisposition::Terminate, 12)
            .expect("the owned parent is the stoppable runtime boundary");
        assert_eq!(answer, Response::Ack);
        assert!(!harness.core.processes.contains_key(&parent_id));
        let session = &harness.core.sessions[&session_id];
        assert_eq!(session.status, SessionStatus::Paused);
        assert!(session
            .tree
            .get(&parent_id)
            .unwrap()
            .lifecycle
            .is_terminal());
        assert_eq!(
            session.tree.get(&child_id).unwrap().lifecycle,
            Lifecycle::Lost,
            "the virtual child retires with the runtime that reported it"
        );
    }

    #[tokio::test]
    async fn a_visual_layout_draft_becomes_a_persisted_reusable_template() {
        let mut harness = Harness::new().await;
        let mut layout = Template::two_shells(1).layout;
        let first = layout.panes()[0].id.clone();
        layout.get_mut(&first).unwrap().node_id =
            Some(turn_core::ids::NodeId::from_stored("proc_draft_must_drop"));

        let summary = match harness
            .core
            .create_layout_template(
                "Dev and tests".into(),
                layout,
                Some("Two commands".into()),
                2,
            )
            .unwrap()
        {
            Response::Template { template } => template,
            other => panic!("unexpected {other:?}"),
        };
        assert_eq!(summary.pane_count, 2);
        let stored = harness
            .core
            .store
            .templates()
            .get(&summary.id)
            .unwrap()
            .expect("persisted Template");
        assert!(stored
            .layout
            .panes()
            .iter()
            .all(|pane| pane.node_id.is_none()));
        let a = stored.instantiate();
        let b = stored.instantiate();
        assert!(
            a.panes().iter().all(|pane| b.get(&pane.id).is_none()),
            "each Session needs fresh Pane ids"
        );
    }

    #[tokio::test]
    async fn archiving_a_session_advances_and_publishes_the_unified_hierarchy() {
        let mut harness = Harness::new().await;
        let root = harness._dir.path().join("session-archive-workspace");
        std::fs::create_dir(&root).unwrap();
        let workspace_id = match harness
            .core
            .create_workspace(
                "archive projection".into(),
                root.to_string_lossy().into_owned(),
                1,
            )
            .unwrap()
        {
            Response::Workspace { workspace } => workspace.id,
            other => panic!("unexpected {other:?}"),
        };
        let (client, mut frames) = harness.add_client(16);
        let initial_revision = match harness
            .core
            .dispatch(
                client,
                Request::GetHierarchy {
                    surface_id: "session-window".into(),
                    include_archived: false,
                },
                2,
            )
            .unwrap()
        {
            Response::Hierarchy { snapshot } => snapshot.revision,
            other => panic!("unexpected {other:?}"),
        };
        let session_id = match harness
            .core
            .create_read_only_session(
                &workspace_id,
                "review".into(),
                None,
                Some(vec![NewPane::new(PaneKind::AgentTree)]),
                None,
                Vec::new(),
                3,
            )
            .unwrap()
        {
            Response::Session { session } => session.id,
            other => panic!("unexpected {other:?}"),
        };
        let created = drain_session_events(&mut frames)
            .into_iter()
            .find_map(|event| match event {
                ServerEvent::HierarchyChanged { snapshot } => Some(*snapshot),
                _ => None,
            })
            .expect("Session creation must publish the hierarchy");
        assert_eq!(created.revision, initial_revision + 1);

        harness.core.archive_session(&session_id, true, 4).unwrap();
        let archived_events = drain_session_events(&mut frames);
        assert!(archived_events.iter().any(|event| matches!(
            event,
            ServerEvent::SessionRemoved { session_id: removed, .. } if removed == &session_id
        )));
        let archived = archived_events
            .into_iter()
            .find_map(|event| match event {
                ServerEvent::HierarchyChanged { snapshot } => Some(*snapshot),
                _ => None,
            })
            .expect("archiving must replace the navigation projection");
        assert_eq!(archived.revision, created.revision + 1);
        assert!(archived.workspaces[0].sessions.is_empty());

        harness.core.archive_session(&session_id, false, 5).unwrap();
        let restored = drain_session_events(&mut frames)
            .into_iter()
            .find_map(|event| match event {
                ServerEvent::HierarchyChanged { snapshot } => Some(*snapshot),
                _ => None,
            })
            .expect("unarchiving must replace the navigation projection");
        assert_eq!(restored.revision, archived.revision + 1);
        assert_eq!(restored.workspaces[0].sessions[0].session.id, session_id);
    }

    #[tokio::test]
    async fn workspace_session_and_template_apis_reject_unsafe_navigation_names() {
        let mut harness = Harness::new().await;
        let session_id = SessionId::from_stored("sess_safe_name_boundary");
        harness.add_session(
            session_id.clone(),
            PaneId::from_stored("pane_safe_name_boundary"),
            10,
        );
        let workspace_id = harness.core.sessions[&session_id].workspace_id.clone();
        let workspace_root = harness._dir.path().to_string_lossy().into_owned();
        let before = (
            harness.core.workspaces.len(),
            harness.core.sessions.len(),
            harness.core.templates.len(),
        );

        for (offset, hostile) in [
            "forged\nrow",
            "clear\x1b[2Jscreen",
            "reverse\u{202e}name",
            "zero\u{200b}width",
            "join\u{200d}me",
        ]
        .into_iter()
        .enumerate()
        {
            let now = 20 + offset as i64;
            let workspace_error = harness
                .core
                .create_workspace(hostile.into(), workspace_root.clone(), now)
                .expect_err("an unsafe Workspace name must be rejected first");
            let session_error = harness
                .core
                .create_session(
                    &workspace_id,
                    hostile.into(),
                    None,
                    Some(vec![NewPane::new(PaneKind::AgentTree)]),
                    None,
                    Vec::new(),
                    now,
                )
                .expect_err("an unsafe Session name must be rejected first");
            let template_error = harness
                .core
                .save_layout_as_template(&session_id, hostile.into(), None, None, now)
                .expect_err("an unsafe Template name must be rejected first");

            for error in [workspace_error, session_error, template_error] {
                assert_eq!(error.code, turn_proto::ErrorCode::InvalidArgument);
            }
        }

        assert_eq!(
            (
                harness.core.workspaces.len(),
                harness.core.sessions.len(),
                harness.core.templates.len(),
            ),
            before,
            "refused labels must not create partial navigation state"
        );
    }

    #[tokio::test]
    async fn read_only_template_resolution_keeps_daemon_owned_configuration() {
        let mut harness = Harness::new().await;
        let project = harness._dir.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let root = harness._dir.path().to_string_lossy().into_owned();
        let workspace_id = match harness
            .core
            .create_workspace("space-troopers".into(), root, 10)
            .unwrap()
        {
            Response::Workspace { workspace } => workspace.id,
            other => panic!("unexpected {other:?}"),
        };
        harness.core.workspaces.get_mut(&workspace_id).unwrap().env =
            vec![("FROM_WORKSPACE".into(), "workspace".into())];
        harness
            .core
            .create_session(
                &workspace_id,
                "Primary writer".into(),
                None,
                Some(vec![NewPane::new(PaneKind::AgentTree)]),
                None,
                Vec::new(),
                11,
            )
            .unwrap();

        let mut template = Template::coding(12);
        template.name_pattern = Some("Review {branch}: {task}".into());
        template.env = vec![("FROM_TEMPLATE".into(), "template".into())];
        let policy = turn_core::attention::AttentionPolicy {
            cooldown_seconds: 37,
            ..Default::default()
        };
        template.attention = Some(policy.clone());
        template.tmux = true;
        let agent_id = template.layout.panes()[0].id.clone();
        let agent = template.layout.get_mut(&agent_id).unwrap();
        agent.args = vec!["--model".into(), "sonnet".into()];
        agent.cwd = Some(".".into());
        agent.env = vec![("PANE_SETTING".into(), "kept".into())];
        let template_id = template.id.clone();
        let expected_panes: Vec<_> = template
            .layout
            .panes()
            .into_iter()
            .map(|pane| {
                (
                    pane.kind,
                    pane.title.clone(),
                    pane.command.clone(),
                    pane.args.clone(),
                    pane.cwd.clone(),
                    pane.env.clone(),
                    pane.restore,
                )
            })
            .collect();
        harness.core.templates.insert(template_id.clone(), template);

        let response = harness
            .core
            .create_read_only_session_from_template(
                &workspace_id,
                &template_id,
                None,
                Some(project.to_string_lossy().into_owned()),
                Some("feat/layout".into()),
                Some("preserve everything".into()),
                13,
            )
            .unwrap();
        let id = match response {
            Response::Session { session } => session.id,
            other => panic!("unexpected {other:?}"),
        };
        let session = &harness.core.sessions[&id];
        let canonical_project = std::fs::canonicalize(&project).unwrap();
        assert_eq!(session.name, "Review feat/layout: preserve everything");
        assert_eq!(session.note.as_deref(), Some("preserve everything"));
        assert_eq!(session.mode, SessionMode::ReadOnly);
        assert_eq!(session.template_id.as_ref(), Some(&template_id));
        assert_eq!(Path::new(&session.cwd), canonical_project);
        assert_eq!(session.git_branch.as_deref(), Some("feat/layout"));
        assert_eq!(
            session.env,
            vec![
                ("FROM_WORKSPACE".into(), "workspace".into()),
                ("FROM_TEMPLATE".into(), "template".into())
            ]
        );
        assert_eq!(session.attention, policy);
        assert!(session.tmux);
        let actual_panes: Vec<_> = session
            .layout
            .panes()
            .into_iter()
            .map(|pane| {
                (
                    pane.kind,
                    pane.title.clone(),
                    pane.command.clone(),
                    pane.args.clone(),
                    pane.cwd.clone(),
                    pane.env.clone(),
                    pane.restore,
                )
            })
            .collect();
        assert_eq!(actual_panes, expected_panes);
        #[cfg(target_os = "macos")]
        {
            assert!(session.read_only_enforced);
            assert!(harness
                .core
                .processes
                .values()
                .any(|process| process.session_id == id));
        }
        #[cfg(not(target_os = "macos"))]
        {
            assert!(!session.read_only_enforced);
            assert!(harness
                .core
                .processes
                .values()
                .all(|process| process.session_id != id));
        }
        let stored = harness
            .core
            .store
            .sessions()
            .get(&id)
            .unwrap()
            .expect("the safe Session is durable");
        assert_eq!(stored.template_id.as_ref(), Some(&template_id));
        assert_eq!(stored.layout.pane_count(), 3);
    }

    #[test]
    fn worktree_template_mapping_preserves_relative_cwds_and_remaps_absolute_ones() {
        let temp = tempfile::tempdir().unwrap();
        let primary = temp.path().join("primary");
        let isolated = temp.path().join("isolated");
        for root in [&primary, &isolated] {
            std::fs::create_dir_all(root.join("project/tools")).unwrap();
        }
        let absolute = Pane::new(PaneKind::Agent)
            .with_command("claude")
            .with_cwd(primary.join("project/tools").to_string_lossy());
        let mut layout = Layout::single(absolute);
        let first = layout.active.clone().unwrap();
        layout.split(
            &first,
            Direction::Horizontal,
            Pane::new(PaneKind::Shell).with_cwd("tools"),
        );

        let primary_project = primary.join("project");
        let mapped_session = remap_template_cwd_to_worktree(
            &primary,
            &isolated,
            Some(primary_project.to_string_lossy().as_ref()),
        )
        .unwrap();
        let isolated_project = std::fs::canonicalize(isolated.join("project")).unwrap();
        assert_eq!(Path::new(&mapped_session), isolated_project);
        remap_absolute_template_pane_cwds(&mut layout, &primary, &isolated).unwrap();
        let panes = layout.panes();
        let isolated_tools = std::fs::canonicalize(isolated.join("project/tools"))
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert_eq!(panes[0].cwd.as_deref(), Some(isolated_tools.as_str()));
        assert_eq!(panes[1].cwd.as_deref(), Some("tools"));
    }

    #[test]
    fn a_list_of_panes_becomes_one_split_in_the_order_it_was_given() {
        let specs = vec![
            NewPane::new(PaneKind::Agent).with_command("claude"),
            NewPane::new(PaneKind::Shell),
            NewPane::new(PaneKind::Logs).with_command("tail -f log"),
        ];
        let layout = layout_from_panes(&specs);
        let panes = layout.panes();
        assert_eq!(panes.len(), 3);
        assert_eq!(panes[0].kind, PaneKind::Agent);
        assert_eq!(panes[1].kind, PaneKind::Shell);
        assert_eq!(panes[2].kind, PaneKind::Logs);
        assert!(
            layout.sizes_are_normalised(),
            "three panes must divide the space evenly rather than nest"
        );
        assert_eq!(layout.active.as_ref(), Some(&panes[0].id));
    }

    #[test]
    fn every_pane_gets_a_fresh_id_even_from_identical_requests() {
        let specs = vec![NewPane::new(PaneKind::Shell), NewPane::new(PaneKind::Shell)];
        let layout = layout_from_panes(&specs);
        let panes = layout.panes();
        assert_ne!(panes[0].id, panes[1].id);
    }

    #[test]
    fn a_pane_request_carries_its_command_and_restore_behaviour_through() {
        let spec = NewPane {
            kind: PaneKind::Server,
            title: Some("api".into()),
            command: Some("cargo run".into()),
            args: vec!["--release".into()],
            cwd: Some("api".into()),
            env: vec![("PORT".into(), "8080".into())],
            restore: turn_core::model::RestoreBehaviour::Relaunch,
        };
        let pane = pane_from_spec(&spec);
        assert!(
            pane.title_is_user_set,
            "an explicitly requested title must outrank a later OSC title"
        );
        assert_eq!(pane.command.as_deref(), Some("cargo run"));
        assert_eq!(pane.args, vec!["--release".to_string()]);
        assert_eq!(pane.cwd.as_deref(), Some("api"));
        assert_eq!(pane.env.len(), 1);
        assert_eq!(pane.restore, turn_core::model::RestoreBehaviour::Relaunch);
        assert!(pane.node_id.is_none(), "a pane starts with no process");
    }

    #[tokio::test]
    async fn a_second_main_checkout_session_is_rejected_before_any_runtime_state_exists() {
        let mut harness = Harness::new().await;
        let root = harness._dir.path().to_string_lossy().to_string();
        let workspace = match harness
            .core
            .create_workspace("space-troopers".into(), root, 10)
            .unwrap()
        {
            Response::Workspace { workspace } => workspace.id,
            other => panic!("unexpected {other:?}"),
        };
        let panes = Some(vec![NewPane::new(PaneKind::AgentTree)]);

        harness
            .core
            .create_session(
                &workspace,
                "Fix climbing bugs".into(),
                None,
                panes.clone(),
                None,
                Vec::new(),
                11,
            )
            .unwrap();
        let sessions_before = harness.core.sessions.len();
        let processes_before = harness.core.processes.len();

        let error = harness
            .core
            .create_session(
                &workspace,
                "Alternative writer".into(),
                None,
                panes,
                None,
                Vec::new(),
                12,
            )
            .expect_err("the existing lease must win");
        assert!(matches!(
            error.context.as_deref(),
            Some(turn_proto::ProtoErrorContext::WorkspaceWriteLeaseConflict {
                alternatives,
                ..
            }) if alternatives.contains(&turn_proto::SessionConflictAlternative::CreateReadOnly)
                && alternatives.contains(&turn_proto::SessionConflictAlternative::CreateIsolatedWorktree)
        ));
        assert_eq!(harness.core.sessions.len(), sessions_before);
        assert_eq!(harness.core.processes.len(), processes_before);
        assert_eq!(harness.core.store.sessions().count().unwrap(), 1);
    }

    #[tokio::test]
    async fn an_archived_workspace_cannot_create_a_hidden_session() {
        let mut harness = Harness::new().await;
        let root = harness._dir.path().to_string_lossy().to_string();
        let workspace = match harness
            .core
            .create_workspace("filed-project".into(), root, 10)
            .unwrap()
        {
            Response::Workspace { workspace } => workspace.id,
            other => panic!("unexpected {other:?}"),
        };
        harness
            .core
            .archive_workspace(&workspace, true, 11)
            .unwrap();
        let sessions_before = harness.core.sessions.len();
        let processes_before = harness.core.processes.len();

        let error = harness
            .core
            .create_session(
                &workspace,
                "Invisible writer".into(),
                None,
                Some(vec![NewPane::new(PaneKind::AgentTree)]),
                None,
                Vec::new(),
                12,
            )
            .expect_err("an archived Workspace must be restored before adding work");
        assert_eq!(error.code, turn_proto::ErrorCode::Refused);
        assert!(error.message.contains("Unarchive"));
        assert_eq!(harness.core.sessions.len(), sessions_before);
        assert_eq!(harness.core.processes.len(), processes_before);
        assert_eq!(harness.core.store.sessions().count().unwrap(), 0);
        assert!(harness
            .core
            .store
            .hierarchy()
            .active_lease(&workspace)
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn a_primary_writer_must_release_its_lease_before_it_can_be_archived() {
        let mut harness = Harness::new().await;
        let root = harness._dir.path().to_string_lossy().to_string();
        let workspace = match harness
            .core
            .create_workspace("archive-safety".into(), root, 10)
            .unwrap()
        {
            Response::Workspace { workspace } => workspace.id,
            other => panic!("unexpected {other:?}"),
        };
        let session = match harness
            .core
            .create_session(
                &workspace,
                "Visible writer".into(),
                None,
                Some(vec![NewPane::new(PaneKind::AgentTree)]),
                None,
                Vec::new(),
                11,
            )
            .unwrap()
        {
            Response::Session { session } => session.id,
            other => panic!("unexpected {other:?}"),
        };
        let lease = harness
            .core
            .store
            .hierarchy()
            .active_lease(&workspace)
            .unwrap()
            .expect("the main Session owns the primary checkout");

        let error = harness
            .core
            .archive_session(&session, true, 12)
            .expect_err("archiving must not hide live checkout authority");
        assert_eq!(error.code, turn_proto::ErrorCode::Conflict);
        assert_eq!(
            harness.core.sessions[&session].status,
            SessionStatus::Active
        );
        assert_eq!(
            harness
                .core
                .store
                .sessions()
                .get(&session)
                .unwrap()
                .unwrap()
                .status,
            SessionStatus::Active
        );
        assert_eq!(
            harness
                .core
                .store
                .hierarchy()
                .active_lease(&workspace)
                .unwrap()
                .unwrap()
                .id,
            lease.id
        );

        harness
            .core
            .release_workspace_write_lease(&workspace, &lease.id, lease.generation, 13)
            .unwrap();
        harness.core.archive_session(&session, true, 14).unwrap();
        assert_eq!(
            harness.core.sessions[&session].status,
            SessionStatus::Archived
        );
        assert!(harness
            .core
            .store
            .hierarchy()
            .active_lease(&workspace)
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn a_legacy_workspace_gets_a_typed_refusal_instead_of_an_implicit_lease() {
        let mut harness = Harness::new().await;
        let root = harness._dir.path().to_string_lossy().to_string();
        let workspace = match harness
            .core
            .create_workspace("legacy".into(), root, 10)
            .unwrap()
        {
            Response::Workspace { workspace } => workspace.id,
            other => panic!("unexpected {other:?}"),
        };
        let mut legacy = harness.core.workspaces[&workspace].clone();
        legacy.lease_reconciliation_required = true;
        harness.core.store.workspaces().save(&legacy).unwrap();
        harness.core.workspaces.insert(workspace.clone(), legacy);

        let error = harness
            .core
            .create_session(
                &workspace,
                "Unsafe writer".into(),
                None,
                Some(vec![NewPane::new(PaneKind::AgentTree)]),
                None,
                Vec::new(),
                11,
            )
            .expect_err("reconciliation is an explicit gate");
        assert_eq!(error.code, turn_proto::ErrorCode::Refused);
        assert!(error.message.contains("reconciliation"));
        assert!(harness.core.sessions.is_empty());
        assert_eq!(harness.core.store.sessions().count().unwrap(), 0);
        assert!(harness.core.workspaces[&workspace].lease_reconciliation_required);
    }

    #[tokio::test]
    async fn a_second_workspace_alias_is_refused_before_it_can_mint_a_checkout() {
        let mut harness = Harness::new().await;
        let root = harness._dir.path().to_string_lossy().to_string();
        let first_workspace = match harness
            .core
            .create_workspace("first alias".into(), root.clone(), 10)
            .unwrap()
        {
            Response::Workspace { workspace } => workspace.id,
            other => panic!("unexpected {other:?}"),
        };
        let error = harness
            .core
            .create_workspace("second alias".into(), root, 11)
            .expect_err("Workspace aliases are not a supported navigation identity");
        assert_eq!(error.code, turn_proto::ErrorCode::Refused);
        assert!(
            error.context.is_none(),
            "an alias is not a lease-owner conflict"
        );
        assert_eq!(harness.core.workspaces.len(), 1);
        assert!(harness.core.workspaces.contains_key(&first_workspace));
        assert_eq!(harness.core.store.workspaces().count().unwrap(), 1);
    }

    #[tokio::test]
    async fn workspace_a_cannot_create_a_main_or_template_session_rooted_in_workspace_b() {
        let mut harness = Harness::new().await;
        let root_a = harness._dir.path().join("workspace-a");
        let root_b = harness._dir.path().join("workspace-b");
        std::fs::create_dir_all(&root_a).unwrap();
        std::fs::create_dir_all(&root_b).unwrap();
        let workspace_a = match harness
            .core
            .create_workspace(
                "workspace A".into(),
                root_a.to_string_lossy().into_owned(),
                10,
            )
            .unwrap()
        {
            Response::Workspace { workspace } => workspace.id,
            other => panic!("unexpected {other:?}"),
        };
        let _workspace_b = harness
            .core
            .create_workspace(
                "workspace B".into(),
                root_b.to_string_lossy().into_owned(),
                11,
            )
            .unwrap();

        let error = harness
            .core
            .create_session(
                &workspace_a,
                "escaped main".into(),
                Some(root_b.to_string_lossy().into_owned()),
                Some(vec![NewPane::new(PaneKind::AgentTree)]),
                None,
                Vec::new(),
                12,
            )
            .expect_err("an absolute cwd in Workspace B must not acquire A's lease");
        assert_eq!(error.code, turn_proto::ErrorCode::Refused);
        assert!(error.message.contains("outside"));
        assert!(harness.core.sessions.is_empty());
        assert_eq!(harness.core.store.sessions().count().unwrap(), 0);
        assert!(harness
            .core
            .store
            .hierarchy()
            .active_lease(&workspace_a)
            .unwrap()
            .is_none());

        let marker = harness._dir.path().join("template-init-ran");
        let layout = Layout::single(
            Pane::new(PaneKind::Shell)
                .with_command("/bin/sh")
                .with_cwd(root_b.to_string_lossy()),
        );
        let mut template = Template::from_layout("escaped template", &layout, 13);
        template.init_commands = vec![format!("touch {}", marker.display())];
        let template_id = template.id.clone();
        harness.core.templates.insert(template_id.clone(), template);

        let error = harness
            .core
            .create_session_from_template(
                &workspace_a,
                &template_id,
                Some("escaped template session".into()),
                None,
                None,
                None,
                14,
            )
            .expect_err("a template Pane cannot escape before lease acquisition");
        assert_eq!(error.code, turn_proto::ErrorCode::Refused);
        assert!(error.message.contains("outside"));
        assert!(!marker.exists(), "template init ran before cwd validation");
        assert!(harness.core.sessions.is_empty());
        assert_eq!(harness.core.store.sessions().count().unwrap(), 0);
        assert!(harness
            .core
            .store
            .hierarchy()
            .active_lease(&workspace_a)
            .unwrap()
            .is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn dotdot_and_symlink_pane_escapes_are_refused_before_layout_or_pty_side_effects() {
        use std::os::unix::fs::symlink;

        let mut harness = Harness::new().await;
        let root_a = harness._dir.path().join("workspace-a");
        let nested_a = root_a.join("nested");
        let root_b = harness._dir.path().join("workspace-b");
        std::fs::create_dir_all(&nested_a).unwrap();
        std::fs::create_dir_all(&root_b).unwrap();
        symlink(&root_b, nested_a.join("escape-link")).unwrap();
        let workspace_a = match harness
            .core
            .create_workspace(
                "workspace A".into(),
                root_a.to_string_lossy().into_owned(),
                20,
            )
            .unwrap()
        {
            Response::Workspace { workspace } => workspace.id,
            other => panic!("unexpected {other:?}"),
        };
        harness
            .core
            .create_workspace(
                "workspace B".into(),
                root_b.to_string_lossy().into_owned(),
                21,
            )
            .unwrap();
        let response = harness
            .core
            .create_session(
                &workspace_a,
                "safe session".into(),
                Some(nested_a.to_string_lossy().into_owned()),
                Some(vec![NewPane::new(PaneKind::AgentTree)]),
                None,
                Vec::new(),
                22,
            )
            .unwrap();
        let session_id = match response {
            Response::Session { session } => session.id,
            other => panic!("unexpected {other:?}"),
        };
        let original_pane = harness.core.sessions[&session_id].layout.panes()[0]
            .id
            .clone();
        let original_count = harness.core.sessions[&session_id].layout.pane_count();
        let (client, _frames) = harness.add_client(9);

        for cwd in ["../../workspace-b", "escape-link"] {
            let error = harness
                .core
                .split_pane(
                    client,
                    &session_id,
                    &original_pane,
                    Direction::Horizontal,
                    NewPane {
                        kind: PaneKind::Shell,
                        title: Some("escape".into()),
                        command: Some("/bin/sh".into()),
                        args: Vec::new(),
                        cwd: Some(cwd.into()),
                        env: Vec::new(),
                        restore: turn_core::model::RestoreBehaviour::ReattachOnly,
                    },
                    23,
                )
                .expect_err("the Pane cwd must stay in Workspace A");
            assert_eq!(error.code, turn_proto::ErrorCode::Refused, "{cwd}");
            assert!(error.message.contains("outside"), "{cwd}: {error:?}");
            assert_eq!(
                harness.core.sessions[&session_id].layout.pane_count(),
                original_count,
                "an invalid Pane must not enter the Layout"
            );
            assert!(
                harness.core.processes.is_empty(),
                "an invalid Pane must not reach PTY spawn"
            );
        }

        // Defence in depth: even a restored/corrupted Layout that bypassed the
        // request preflight is checked again immediately before PTY creation.
        {
            let pane = harness
                .core
                .sessions
                .get_mut(&session_id)
                .unwrap()
                .layout
                .get_mut(&original_pane)
                .unwrap();
            pane.kind = PaneKind::Shell;
            pane.command = Some("/bin/sh".into());
            pane.cwd = Some("escape-link".into());
        }
        let error = harness
            .core
            .materialise_pane(&session_id, &original_pane, 24)
            .expect_err("the final launch boundary must distrust the stored Layout");
        assert_eq!(error.code, turn_proto::ErrorCode::Refused);
        assert!(error.message.contains("outside"));
        assert!(harness.core.processes.is_empty());

        let marker = harness._dir.path().join("escaped-init-ran");
        harness.core.sessions.get_mut(&session_id).unwrap().cwd =
            root_b.to_string_lossy().into_owned();
        let error = harness
            .core
            .spawn_init_command(&session_id, &format!("touch {}", marker.display()), 25)
            .expect_err("an init command must recheck the Session cwd at launch");
        assert_eq!(error.code, turn_proto::ErrorCode::Refused);
        assert!(error.message.contains("outside"));
        assert!(!marker.exists());
        assert!(harness.core.processes.is_empty());
    }

    #[cfg(not(target_os = "macos"))]
    #[tokio::test]
    async fn a_read_only_alternative_never_launches_without_a_technical_guard() {
        let mut harness = Harness::new().await;
        let root = harness._dir.path().to_string_lossy().to_string();
        let workspace = match harness
            .core
            .create_workspace("space-troopers".into(), root, 10)
            .unwrap()
        {
            Response::Workspace { workspace } => workspace.id,
            other => panic!("unexpected {other:?}"),
        };
        harness
            .core
            .create_session(
                &workspace,
                "Writer".into(),
                None,
                Some(vec![NewPane::new(PaneKind::AgentTree)]),
                None,
                Vec::new(),
                11,
            )
            .unwrap();
        let response = harness
            .core
            .create_read_only_session(
                &workspace,
                "Review current changes".into(),
                None,
                Some(vec![NewPane::new(PaneKind::Agent).with_command("claude")]),
                None,
                Vec::new(),
                12,
            )
            .unwrap();
        let id = match response {
            Response::Session { session } => session.id,
            other => panic!("unexpected {other:?}"),
        };
        let session = &harness.core.sessions[&id];
        assert_eq!(session.mode, SessionMode::ReadOnly);
        assert!(!session.read_only_enforced);
        assert!(
            session.tree.is_empty(),
            "an unguarded command must not start"
        );
        assert!(harness
            .core
            .processes
            .values()
            .all(|process| process.session_id != id));

        let marker = harness._dir.path().join("read-only-escape");
        let existing_pane = session.layout.panes()[0].id.clone();
        let (client, _frames) = harness.add_client(8);
        harness
            .core
            .split_pane(
                client,
                &id,
                &existing_pane,
                Direction::Horizontal,
                NewPane {
                    kind: PaneKind::Shell,
                    title: Some("write attempt".into()),
                    command: Some("sh".into()),
                    args: vec!["-c".into(), format!("touch {}", marker.display())],
                    cwd: None,
                    env: Vec::new(),
                    restore: turn_core::model::RestoreBehaviour::ReattachOnly,
                },
                13,
            )
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert!(
            !marker.exists(),
            "a later split bypassed the read-only guard"
        );
        assert!(harness
            .core
            .processes
            .values()
            .all(|process| process.session_id != id));
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn a_guarded_read_only_session_runs_shell_and_agent_panes_without_a_lease() {
        let mut harness = Harness::new().await;
        harness
            .core
            .registry
            .register(std::sync::Arc::new(GuardedProbeAgent));
        let checkout = harness._dir.path().join("checkout");
        std::fs::create_dir(&checkout).unwrap();
        let outside = tempfile::tempdir().unwrap();
        let shell_ran = outside.path().join("shell-ran");
        let agent_ran = outside.path().join("agent-ran");
        let blocked_shell_write = checkout.join("shell-write");
        let blocked_agent_write = checkout.join("agent-write");
        let workspace = match harness
            .core
            .create_workspace(
                "guarded-review".into(),
                checkout.to_string_lossy().into_owned(),
                10,
            )
            .unwrap()
        {
            Response::Workspace { workspace } => workspace.id,
            other => panic!("unexpected {other:?}"),
        };
        let writer = match harness
            .core
            .create_session(
                &workspace,
                "Writer".into(),
                None,
                Some(vec![NewPane::new(PaneKind::AgentTree)]),
                None,
                Vec::new(),
                11,
            )
            .unwrap()
        {
            Response::Session { session } => session.id,
            other => panic!("unexpected {other:?}"),
        };

        let probe = |kind, command: &str, ran: &Path, blocked: &Path| NewPane {
            kind,
            title: None,
            command: Some(command.into()),
            args: vec![
                "-c".into(),
                "touch \"$1\"; touch \"$2\" 2>/dev/null || true; sleep 5".into(),
                "turn-read-only-core-test".into(),
                ran.to_string_lossy().into_owned(),
                blocked.to_string_lossy().into_owned(),
            ],
            cwd: Some(".".into()),
            env: Vec::new(),
            restore: turn_core::model::RestoreBehaviour::ReattachOnly,
        };
        let response = harness
            .core
            .create_read_only_session(
                &workspace,
                "Review current changes".into(),
                None,
                Some(vec![
                    probe(PaneKind::Shell, "/bin/sh", &shell_ran, &blocked_shell_write),
                    probe(
                        PaneKind::Agent,
                        "turn-read-only-test-agent",
                        &agent_ran,
                        &blocked_agent_write,
                    ),
                ]),
                None,
                Vec::new(),
                12,
            )
            .unwrap();
        let id = match response {
            Response::Session { session } => session.id,
            other => panic!("unexpected {other:?}"),
        };

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline && (!shell_ran.exists() || !agent_ran.exists()) {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let session = &harness.core.sessions[&id];
        assert_eq!(session.mode, SessionMode::ReadOnly);
        assert!(session.read_only_enforced);
        assert_eq!(session.layout.panes().len(), 2);
        assert!(session
            .layout
            .panes()
            .iter()
            .all(|pane| pane.node_id.is_some()));
        assert!(session.tree.iter().any(|node| node.kind == NodeKind::Shell));
        assert!(session.tree.iter().any(|node| node.kind == NodeKind::Agent));
        assert_eq!(
            harness
                .core
                .processes
                .values()
                .filter(|process| process.session_id == id)
                .count(),
            2
        );
        assert!(shell_ran.exists(), "the guarded shell pane did not execute");
        assert!(agent_ran.exists(), "the guarded agent pane did not execute");
        assert!(!blocked_shell_write.exists());
        assert!(!blocked_agent_write.exists());

        let lease = harness
            .core
            .store
            .hierarchy()
            .active_lease(&workspace)
            .unwrap()
            .expect("the writer keeps the only checkout lease");
        assert_eq!(lease.session_id, writer);
        assert_ne!(lease.session_id, id);

        for process in harness
            .core
            .processes
            .values()
            .filter(|process| process.session_id == id)
        {
            let _ = process.pty.kill();
        }
    }

    #[tokio::test]
    async fn an_isolated_session_uses_a_real_independent_git_worktree() {
        let mut harness = Harness::new().await;
        let repository = harness._dir.path().join("repository");
        std::fs::create_dir_all(&repository).unwrap();
        let run_git = |args: &[&str]| {
            let status = SystemCommand::new("git")
                .arg("-C")
                .arg(&repository)
                .args(args)
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?} failed");
        };
        run_git(&["init"]);
        run_git(&["config", "user.email", "turn@example.invalid"]);
        run_git(&["config", "user.name", "Turn Test"]);
        std::fs::write(repository.join("README.md"), "turn\n").unwrap();
        run_git(&["add", "README.md"]);
        run_git(&["commit", "-m", "initial"]);

        let workspace = match harness
            .core
            .create_workspace(
                "space-troopers".into(),
                repository.to_string_lossy().to_string(),
                10,
            )
            .unwrap()
        {
            Response::Workspace { workspace } => workspace.id,
            other => panic!("unexpected {other:?}"),
        };
        let target = harness._dir.path().join("review-worktree");
        let response = harness
            .core
            .create_worktree_session(
                &workspace,
                "Alternative movement".into(),
                "turn/alternative-movement".into(),
                Some(target.to_string_lossy().to_string()),
                Some(vec![NewPane::new(PaneKind::AgentTree)]),
                None,
                Vec::new(),
                12,
            )
            .unwrap();
        let id = match response {
            Response::Session { session } => session.id,
            other => panic!("unexpected {other:?}"),
        };
        let session = &harness.core.sessions[&id];
        assert_eq!(session.mode, SessionMode::IsolatedWorktree);
        assert_ne!(Path::new(&session.cwd), repository.as_path());
        assert!(Path::new(&session.cwd).join("README.md").exists());
        assert_eq!(
            harness
                .core
                .store
                .hierarchy()
                .checkouts_for_workspace(&workspace)
                .unwrap()
                .len(),
            2
        );
        assert!(harness
            .core
            .store
            .hierarchy()
            .active_lease(&workspace)
            .unwrap()
            .is_none());

        let original_pane = harness.core.sessions[&id].layout.panes()[0].id.clone();
        let original_count = harness.core.sessions[&id].layout.pane_count();
        let (client, _frames) = harness.add_client(10);
        let error = harness
            .core
            .split_pane(
                client,
                &id,
                &original_pane,
                Direction::Horizontal,
                NewPane {
                    kind: PaneKind::Shell,
                    title: Some("primary escape".into()),
                    command: Some("/bin/sh".into()),
                    args: Vec::new(),
                    cwd: Some(repository.to_string_lossy().into_owned()),
                    env: Vec::new(),
                    restore: turn_core::model::RestoreBehaviour::ReattachOnly,
                },
                13,
            )
            .expect_err("a worktree Pane cannot start in the primary checkout");
        assert_eq!(error.code, turn_proto::ErrorCode::Refused);
        assert!(error.message.contains("outside"));
        assert_eq!(
            harness.core.sessions[&id].layout.pane_count(),
            original_count
        );
        assert!(harness.core.processes.is_empty());
    }
}
