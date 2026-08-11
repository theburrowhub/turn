//! Session and template operations.

use super::workspaces::store;
use super::{check_name, Answer};
use crate::core::Core;
use crate::paths;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command as SystemCommand;
use turn_core::ids::{CheckoutId, NodeId, SessionId, TemplateId, WorkspaceId};
#[cfg(test)]
use turn_core::model::SessionStatus;
use turn_core::model::{
    Direction, Layout, Pane, PaneKind, Session, SessionMode, Template, Workspace,
    WorkspaceCheckout, WorkspaceWriteLease,
};
use turn_core::state::Lifecycle;
use turn_proto::{
    CloseDisposition, ErrorCode, EscapedProcess, NewPane, ProtoError, Response, ServerEvent,
    WorkspaceSummary,
};

impl Core {
    /// The processes in this Session that a destructive close cannot signal, and that may
    /// therefore still be alive once it is over.
    ///
    /// A restored orphan has a PID-shaped observation but no owned handle. PID reuse makes
    /// signalling it blindly a coin flip on somebody else's process, and fabricating an
    /// exit would release checkout authority while the real process may still be writing.
    /// So Turn does neither: it names them.
    ///
    /// **This used to refuse the close.** It returned `Conflict` — "Turn cannot safely stop
    /// processes that survived the previous daemon" — and the user was left holding a
    /// Session they had already finished with, told to go and fix the daemon's problem
    /// before they were allowed to be rid of it. Ending a Session is the user saying it is
    /// over, and Turn declining to believe them in the name of safety protected nothing:
    /// the survivor kept running either way, and the only thing the refusal preserved was
    /// the row in the tree. What is owed here is an honest sentence, not a veto.
    pub(crate) fn escaped_session_processes(
        &self,
        id: &SessionId,
        disposition: CloseDisposition,
    ) -> Vec<EscapedProcess> {
        if disposition == CloseDisposition::KeepProcesses {
            return Vec::new();
        }
        let Ok(session) = self.session(id) else {
            return Vec::new();
        };
        session
            .tree
            .iter()
            .filter(|node| {
                if !node.is_running() || self.processes.contains_key(&node.id) {
                    return false;
                }
                // An agent Turn started inside a terminal it still owns is reachable:
                // closing that terminal ends it, so it is not a process that has
                // escaped Turn the way a survivor of a previous daemon has.
                if self.is_hosted(&node.id) {
                    return false;
                }
                // A PID is an independent runtime boundary. Even when its edge was
                // discovered below an owned PTY, it may have detached from that process
                // group; Turn must not claim it died merely because the parent did.
                node.lifecycle == Lifecycle::Orphaned || node.pid.is_some()
            })
            .map(|node| EscapedProcess {
                node_id: node.id.clone(),
                session_id: id.clone(),
                title: node.title.clone(),
                pid: node.pid,
            })
            .collect()
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
        if !node.is_running() || self.processes.contains_key(node_id) || self.is_hosted(node_id) {
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
        if let Some(held) = self
            .store
            .hierarchy()
            .active_lease(workspace_id)
            .map_err(store)?
        {
            return Err(self.local_lease_conflict(workspace_id, Some(&id), held));
        }
        let claim = WorkspaceWriteLease::active(
            workspace_id.clone(),
            id.clone(),
            session.checkout_id.clone(),
            now_ms,
        );
        let checkout_lock = self.checkout_lock_claim(&session, &claim)?;
        // The store arbitrates and persists this Session in one IMMEDIATE
        // transaction. Nothing user-configured is executed before the exclusive
        // primary-checkout lease exists.
        let lease = self
            .store
            .hierarchy()
            .create_session_with_lease_id(&session, Some(&claim.id), now_ms)
            .map_err(|error| self.map_lease_store_error(workspace_id, Some(&id), error))?
            .ok_or_else(|| ProtoError::internal("a main-checkout Session acquired no lease"))?;
        self.sessions.insert(id.clone(), session);
        self.install_checkout_write_lock(&id, &lease, checkout_lock);

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
        if let Some(held) = self
            .store
            .hierarchy()
            .active_lease(workspace_id)
            .map_err(store)?
        {
            return Err(self.local_lease_conflict(workspace_id, Some(&id), held));
        }
        let claim = WorkspaceWriteLease::active(
            workspace_id.clone(),
            id.clone(),
            session.checkout_id.clone(),
            now_ms,
        );
        let checkout_lock = self.checkout_lock_claim(&session, &claim)?;
        let lease = self
            .store
            .hierarchy()
            .create_session_with_lease_id(&session, Some(&claim.id), now_ms)
            .map_err(|error| self.map_lease_store_error(workspace_id, Some(&id), error))?
            .ok_or_else(|| ProtoError::internal("a main-checkout Session acquired no lease"))?;
        self.sessions.insert(id.clone(), session);
        self.install_checkout_write_lock(&id, &lease, checkout_lock);

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

    pub(super) fn set_session_favourite(
        &mut self,
        id: &SessionId,
        favourite: bool,
        now_ms: i64,
    ) -> Answer {
        self.session_mut(id)?.favourite = favourite;
        self.persist_session(id)?;
        self.push_session_state(id, now_ms);
        self.answer_session(id, now_ms)
    }

    pub(super) fn set_session_pinned(
        &mut self,
        id: &SessionId,
        pinned: bool,
        now_ms: i64,
    ) -> Answer {
        self.session_mut(id)?.pinned = pinned;
        self.persist_session(id)?;
        self.push_session_state(id, now_ms);
        self.answer_session(id, now_ms)
    }

    /// Closes a session, doing exactly what the disposition says.
    ///
    /// There is no default for a reason: the whole point of the daemon is that
    /// processes outlive the window, so "close" is ambiguous in a way that would either
    /// kill work the user wanted kept or leak processes they thought were gone.
    ///
    /// * `KeepProcesses` detaches the clients and leaves everything running. The
    ///   session stays in the list; reopening it re-attaches to the same ptys.
    /// * `Terminate` and `Kill` stop what Turn can reach, archive the Session, and answer
    ///   with the processes they could not reach.
    ///
    /// **`Terminate` and `Kill` cannot fail on the user's behalf.** Past the point where
    /// the disposition is known, everything after it is best-effort: a process that cannot
    /// be signalled, a write that will not land, a lease row that will not update. Each is
    /// logged and none of them abandons the act, because a half-ended Session is worse
    /// than either outcome and the user has already said what they want. The two things
    /// that still return an error are asking about a Session that does not exist, and
    /// `KeepProcesses`, which is not destructive and can therefore afford to be strict.
    pub(super) fn close_session(
        &mut self,
        id: &SessionId,
        disposition: CloseDisposition,
        now_ms: i64,
    ) -> Answer {
        let mut escaped = self.escaped_session_processes(id, disposition);
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
        if disposition == CloseDisposition::KeepProcesses {
            self.clear_session_temporary_bindings(id, now_ms)?;
        } else if let Err(error) = self.clear_session_temporary_bindings(id, now_ms) {
            tracing::warn!(%error, session = %id, "could not clear temporary bindings while ending");
        }
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
                // Whatever is still running now is a process Turn asked to stop and could
                // not, or one it never had a handle on. Either way it goes in the answer
                // rather than into an error: this used to `return Err` here, which left
                // the Session parked as `Active` with its row back in the tree — the
                // exact state the user had just asked to be rid of, restored on their
                // behalf as a safety measure that made nothing safer.
                merge_escaped(&mut escaped, self.still_running(id));
                let restore_update = self.resolve_restore_session(id);
                // The injected agent configuration goes with the processes it was
                // written for. Nothing will read it again, and a settings file naming a
                // hook URL that no longer answers is worse than no file.
                paths::remove_session_scratch(&self.data_dir, id);
                // Ending a Session means being done with it, so it lets go of the write lease it
                // was holding. A stopped Session that still owns the primary checkout locks
                // every other Session out of it for no reason, and it is also what would stop
                // the row from leaving the tree below.
                self.release_lease_held_by(id, now_ms);
                // And the row leaves the tree.
                //
                // This is the point of the whole verb, and for a long time it did not happen:
                // "End session" stopped the processes and left the row sitting in the tree as
                // `Paused`, which reads as a Session that is still a thing you are working on.
                // Ending something has to look like ending it. Archived rather than deleted, so
                // it is recoverable — the row comes back, stopped, when archived rows are shown
                // or when it is restored — and `DeleteSession` is still the one that forgets.
                if let Ok(session) = self.session_mut(id) {
                    session.archive();
                }
                let workspace_id = self
                    .session(id)
                    .map(|session| session.workspace_id.clone())
                    .ok();
                if let Err(error) = self.clear_session_temporary_bindings(id, now_ms) {
                    tracing::warn!(%error, session = %id, "could not clear temporary bindings while ending");
                }
                // A Session that will not persist has still ended. The window is told
                // either way, so the row leaves the tree now rather than after a restart
                // that may never come — and if the write really is broken the user finds
                // out from the log and from every other operation, not by being refused
                // the one thing they asked for.
                if let Err(error) = self.persist_session(id) {
                    tracing::error!(%error, session = %id, "ended a Session that would not persist");
                }
                if let Some(workspace_id) = workspace_id {
                    self.push_all(ServerEvent::SessionRemoved {
                        session_id: id.clone(),
                        workspace_id,
                    });
                }
                self.push_session_state(id, now_ms);
                if let Some(update) = restore_update {
                    self.push_all(update);
                }
                if !escaped.is_empty() {
                    tracing::warn!(
                        session = %id,
                        escaped = escaped.len(),
                        "ended a Session with processes Turn could not stop"
                    );
                }
            }
        }
        Ok(Response::Closed { escaped })
    }

    /// The Session's processes that are still marked running, as things that may have
    /// survived it. Read after the stop attempts, so it is the leftovers.
    fn still_running(&self, id: &SessionId) -> Vec<EscapedProcess> {
        let Ok(session) = self.session(id) else {
            return Vec::new();
        };
        session
            .tree
            .iter()
            .filter(|node| node.is_running())
            .map(|node| EscapedProcess {
                node_id: node.id.clone(),
                session_id: id.clone(),
                title: node.title.clone(),
                pid: node.pid,
            })
            .collect()
    }

    /// Releases the primary-checkout write lease this Session holds, if it holds one.
    ///
    /// Called when a Session ends or is deleted, and deliberately quiet: by this point its
    /// processes are stopped, so nothing can be writing to the checkout, and a lease left behind
    /// would lock every other Session out of a directory nobody is using. A failure is logged
    /// rather than returned — the Session has already ended, and refusing the whole operation
    /// because the lease row would not update would leave the user with neither.
    fn release_lease_held_by(&mut self, id: &SessionId, now_ms: i64) {
        let Ok(workspace_id) = self.session(id).map(|session| session.workspace_id.clone()) else {
            return;
        };
        let held = match self.store.hierarchy().active_lease(&workspace_id) {
            Ok(Some(lease)) if lease.session_id == *id => lease,
            Ok(_) => return,
            Err(error) => {
                tracing::warn!(%error, session = %id, "could not read the write lease while ending");
                return;
            }
        };
        match self
            .store
            .hierarchy()
            .release_write_lease_and_assign_read_only(&held.id, held.generation, false, now_ms)
        {
            Ok(true) => {
                if let Ok(session) = self.session_mut(id) {
                    session.mode = SessionMode::ReadOnly;
                    session.worktree_path = None;
                    session.read_only_enforced = false;
                }
                self.drop_checkout_write_lock(&held.id);
                self.push_workspace_lease(&workspace_id, None, now_ms);
                tracing::info!(session = %id, lease = %held.id, "write lease released as the session ended");
            }
            Ok(false) => {
                tracing::info!(session = %id, "the write lease was no longer active");
            }
            Err(error) => {
                tracing::warn!(%error, session = %id, "could not release the write lease while ending");
            }
        }
    }

    /// Removes a Session from Turn for good.
    ///
    /// The third verb, after archiving and closing, and the one the tree had no way to reach:
    /// a Session could be hidden or stopped, and never got rid of. Its record stayed, its row
    /// came back the next time archived rows were shown, and there was no answer to "I am done
    /// with this, take it away".
    ///
    /// The order is the whole implementation, and every step is a precondition for the next:
    ///
    /// 1. **`KeepProcesses` is refused.** Forgetting a Session while its processes run would
    ///    leave them alive with nothing left that names them — not in the tree, not in the
    ///    store, not in any pane. That is a leak the user cannot even see, let alone fix.
    /// 2. **Stop and detach**, through [`Self::close_session`], so there is one definition of
    ///    what stopping a Session means. It also inherits its refusal: a Session with a child
    ///    process Turn cannot reach is not deleted, and the error says which process.
    /// 3. **Delete the record**, which cascades to the layout, the process nodes, the event
    ///    log, the attention rows, the activity previews, the pane bindings, the write lease
    ///    and the per-window tree state.
    /// 4. **Drop what is only in memory**: the Session, its attention demands, its screens.
    ///
    /// Nothing on the user's disk is touched. The checkout, the branch and any worktree are
    /// theirs; Turn deletes its own record and nothing else. The scratch directory is Turn's
    /// and goes with it — `close_session` already removes it.
    ///
    /// Deleting a Session that is not here answers `Ack`. It has to: a client that lost the
    /// reply and retried would otherwise be told the thing it asked to remove cannot be found,
    /// which is the outcome it wanted.
    pub(super) fn delete_session(
        &mut self,
        id: &SessionId,
        disposition: CloseDisposition,
        now_ms: i64,
    ) -> Answer {
        if matches!(disposition, CloseDisposition::KeepProcesses) {
            return Err(ProtoError::refused(
                "Deleting a Session cannot keep its processes running",
            )
            .with_detail(
                "Nothing would name them afterwards. Stop them, or close the Session instead \
                 of deleting it.",
            ));
        }
        let Ok(session) = self.session(id) else {
            // Already gone. Said plainly rather than as an error, so a retry is not a failure.
            tracing::info!(session = %id, "delete asked for a Session that is already gone");
            return Ok(Response::Closed {
                escaped: Vec::new(),
            });
        };
        let workspace_id = session.workspace_id.clone();
        let name = session.name.clone();
        let nodes: Vec<turn_core::ids::NodeId> =
            session.tree.iter().map(|node| node.id.clone()).collect();

        // Stops the processes, detaches every client and removes the scratch directory. What
        // it could not stop it names, and that report is this one too: a Session being
        // forgotten is the last moment anything can tell the user about a process of its
        // that is still alive, because afterwards nothing in Turn names it.
        let escaped = match self.close_session(id, disposition, now_ms)? {
            Response::Closed { escaped } => escaped,
            _ => Vec::new(),
        };

        self.store.sessions().delete(id).map_err(store)?;
        if let Err(error) = self
            .store
            .settings()
            .remove(&crate::core::attention::mute_setting_key(id))
        {
            tracing::warn!(%error, session = %id, "could not remove an obsolete Attention mute");
        }
        if let Err(error) = self
            .store
            .setting_layers()
            .forget_owner(turn_core::settings::Scope::Session, id.as_str())
        {
            tracing::warn!(%error, session = %id, "could not remove obsolete Session settings");
        }
        self.sessions.remove(id);
        self.attention.forget_session(id);
        for node in &nodes {
            self.screens.remove(node);
            self.processes.remove(node);
        }
        tracing::info!(session = %id, %name, "deleted");

        self.push_all(ServerEvent::SessionRemoved {
            session_id: id.clone(),
            workspace_id,
        });
        self.bump_hierarchy();
        self.push_hierarchy_all(now_ms);
        Ok(Response::Closed { escaped })
    }

    /// Captures a session's current arrangement as a template.
    pub(super) fn get_template(&self, id: &TemplateId) -> Answer {
        let template = self
            .templates
            .get(id)
            .ok_or_else(|| ProtoError::not_found("template", id.as_str()))?
            .clone();
        Ok(Response::TemplateDetails {
            template: Box::new(template),
        })
    }

    /// Creates a Template from the visual editor before a Session exists.
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
        let summary = self.template_summary(&template);
        self.templates.insert(template.id.clone(), template);
        Ok(Response::Template { template: summary })
    }

    pub(super) fn create_template(&mut self, draft: Template, now_ms: i64) -> Answer {
        let name = check_name(&draft.name)?;
        self.require_unique_template_name(&name, None)?;
        validate_template_layout(&draft.layout)?;
        let id = TemplateId::new();
        let template = editable_template(draft, id, name, now_ms);
        self.store.templates().save(&template).map_err(store)?;
        let summary = self.template_summary(&template);
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
        self.require_unique_template_name(&name, None)?;
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
        let summary = self.template_summary(&template);
        self.templates.insert(template.id.clone(), template);
        Ok(Response::Template { template: summary })
    }

    /// Replaces one user-owned Template with the complete editor draft.
    pub(super) fn update_template(&mut self, id: &TemplateId, draft: Template) -> Answer {
        let existing = self
            .templates
            .get(id)
            .ok_or_else(|| ProtoError::not_found("template", id.as_str()))?
            .clone();
        if existing.built_in {
            return Err(ProtoError::refused(
                "Built-in Templates are read-only; duplicate this one to customise it",
            ));
        }
        let name = check_name(&draft.name)?;
        self.require_unique_template_name(&name, Some(id))?;
        validate_template_layout(&draft.layout)?;

        // Construct from the draft Layout to strip any runtime binding supplied by a client,
        // then copy only editable metadata. Identity, creation time and built-in ownership are
        // daemon facts and cannot be changed through the editor.
        let template = editable_template(draft, existing.id.clone(), name, existing.created_ms);
        self.store.templates().save(&template).map_err(store)?;
        let summary = self.template_summary(&template);
        self.templates.insert(id.clone(), template);
        Ok(Response::Template { template: summary })
    }

    /// Copies a Template into a new user-owned definition.
    pub(super) fn duplicate_template(
        &mut self,
        id: &TemplateId,
        name: String,
        now_ms: i64,
    ) -> Answer {
        let source = self
            .templates
            .get(id)
            .ok_or_else(|| ProtoError::not_found("template", id.as_str()))?
            .clone();
        let name = check_name(&name)?;
        self.require_unique_template_name(&name, None)?;
        let mut copy = Template::from_layout(name, &source.layout, now_ms);
        copy.description = source.description;
        copy.icon = source.icon;
        copy.attention = source.attention;
        copy.init_commands = source.init_commands;
        copy.name_pattern = source.name_pattern;
        copy.hotkey = source.hotkey;
        copy.env = source.env;
        copy.tmux = source.tmux;
        self.store.templates().save(&copy).map_err(store)?;
        let summary = self.template_summary(&copy);
        self.templates.insert(copy.id.clone(), copy);
        Ok(Response::Template { template: summary })
    }

    /// Deletes a user-owned Template without changing the independent Session configuration
    /// instantiated from it.
    pub(super) fn delete_template(&mut self, id: &TemplateId, now_ms: i64) -> Answer {
        let template = self
            .templates
            .get(id)
            .ok_or_else(|| ProtoError::not_found("template", id.as_str()))?;
        if template.built_in {
            return Err(ProtoError::refused(
                "The portable built-in Template cannot be deleted",
            ));
        }

        // Defaults and `template_id` are pointers, not ownership. Clear only pointers to the
        // deleted row; every Session retains the independent Layout/configuration it was
        // instantiated with.
        let changed_workspaces: Vec<_> = self
            .workspaces
            .values()
            .filter(|workspace| workspace.default_template.as_ref() == Some(id))
            .cloned()
            .map(|mut workspace| {
                workspace.default_template = None;
                workspace
            })
            .collect();
        for workspace in &changed_workspaces {
            self.store.workspaces().save(workspace).map_err(store)?;
        }
        let changed_sessions: Vec<_> = self
            .sessions
            .values()
            .filter(|session| session.template_id.as_ref() == Some(id))
            .cloned()
            .map(|mut session| {
                session.template_id = None;
                session
            })
            .collect();
        for session in &changed_sessions {
            self.store.sessions().save(session).map_err(store)?;
        }
        if self
            .setting_for(None, "templates.default")
            .as_str()
            .is_some_and(|configured| configured == id.as_str())
        {
            self.store
                .setting_layers()
                .clear(turn_core::settings::Scope::Global, "", "templates.default")
                .map_err(store)?;
        }
        self.store
            .setting_layers()
            .forget_owner(turn_core::settings::Scope::Template, id.as_str())
            .map_err(store)?;
        self.store.templates().delete(id).map_err(store)?;
        for workspace in changed_workspaces {
            self.workspaces.insert(workspace.id.clone(), workspace);
        }
        let changed_session_ids: Vec<_> = changed_sessions
            .iter()
            .map(|session| session.id.clone())
            .collect();
        for session in changed_sessions {
            self.sessions.insert(session.id.clone(), session);
        }
        self.templates.remove(id);
        for session_id in changed_session_ids {
            self.push_session_state(&session_id, now_ms);
        }
        Ok(Response::Templates {
            templates: self.template_summaries(),
        })
    }

    pub(super) fn set_workspace_default_template(
        &mut self,
        workspace_id: &WorkspaceId,
        template_id: Option<TemplateId>,
        now_ms: i64,
    ) -> Answer {
        if let Some(id) = &template_id {
            if !self.templates.contains_key(id) {
                return Err(ProtoError::not_found("template", id.as_str()));
            }
        }
        let mut workspace = self.workspace(workspace_id)?.clone();
        workspace.default_template = template_id;
        workspace.touch(now_ms);
        self.store.workspaces().save(&workspace).map_err(store)?;
        let summary = WorkspaceSummary::from_workspace(
            &workspace,
            &self.session_summaries(Some(workspace_id), true, now_ms),
        );
        self.workspaces.insert(workspace_id.clone(), workspace);
        Ok(Response::Workspace { workspace: summary })
    }

    /// Applies a Template only when doing so cannot stop or orphan a running process.
    pub(super) fn apply_template_to_session(
        &mut self,
        session_id: &SessionId,
        template_id: &TemplateId,
        now_ms: i64,
    ) -> Answer {
        if !self.still_running(session_id).is_empty() {
            return Err(ProtoError::refused(
                "Stop the Session's processes before replacing its layout with a Template",
            ));
        }
        let workspace_id = self.session(session_id)?.workspace_id.clone();
        let workspace = self.workspace(&workspace_id)?.clone();
        let template = self
            .templates
            .get(template_id)
            .ok_or_else(|| ProtoError::not_found("template", template_id.as_str()))?
            .clone();
        let mut updated = self.session(session_id)?.clone();
        updated.layout = template.instantiate();
        updated.template_id = Some(template.id.clone());
        updated.attention = template
            .attention
            .clone()
            .unwrap_or_else(|| workspace.attention.clone());
        updated.env = workspace
            .env
            .iter()
            .cloned()
            .chain(template.env.iter().cloned())
            .collect();
        updated.tmux = template.tmux;
        updated.restore_state = turn_core::model::RestoreState::Live;
        updated.touch(now_ms);
        updated.cwd = self.validate_session_definition_cwds(&updated)?;
        self.store.sessions().save(&updated).map_err(store)?;
        self.sessions.insert(session_id.clone(), updated);

        let init: Vec<String> = workspace
            .init_commands
            .iter()
            .cloned()
            .chain(template.init_commands.iter().cloned())
            .collect();
        self.run_init_commands(session_id, &init, now_ms);
        self.materialise_session(session_id, now_ms);
        self.persist_session(session_id)?;
        self.push_layout(session_id, None);
        self.push_session_state(session_id, now_ms);
        self.answer_session(session_id, now_ms)
    }

    fn require_unique_template_name(
        &self,
        name: &str,
        except: Option<&TemplateId>,
    ) -> Result<(), ProtoError> {
        if self.templates.values().any(|template| {
            Some(&template.id) != except && template.name.eq_ignore_ascii_case(name)
        }) {
            return Err(ProtoError::new(
                turn_proto::ErrorCode::Conflict,
                "A layout with that name already exists",
            ));
        }
        Ok(())
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

/// Rebuilds an editable Template while keeping daemon-owned identity separate from the client draft.
fn editable_template(draft: Template, id: TemplateId, name: String, created_ms: i64) -> Template {
    let mut template = Template::from_layout(name, &draft.layout, created_ms);
    template.id = id;
    template.description = draft.description;
    template.icon = draft.icon;
    template.attention = draft.attention;
    template.init_commands = draft.init_commands;
    template.name_pattern = draft.name_pattern;
    template.hotkey = draft.hotkey;
    template.env = draft.env;
    template.tmux = draft.tmux;
    template
}

/// Adds processes to an escaped list without repeating a node already in it.
///
/// The two ways a process survives an end overlap: one that was already out of reach
/// before the attempt is also, necessarily, still running after it. Naming it twice in the
/// sentence the user reads would make Turn look like it does not know what it is talking
/// about.
pub(crate) fn merge_escaped(into: &mut Vec<EscapedProcess>, more: Vec<EscapedProcess>) {
    for process in more {
        if !into
            .iter()
            .any(|known| known.node_id == process.node_id && known.session_id == process.session_id)
        {
            into.push(process);
        }
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

    /// Ending a Session with a process from a previous daemon in it ends the Session.
    ///
    /// Reported from the window: the red banner said "Turn cannot safely stop processes that
    /// survived the previous daemon" and the Session could not be got rid of at all. The
    /// safety being protected was real but the trade was not — the survivor kept running
    /// whether Turn refused or not, so the only thing the refusal preserved was a row its
    /// owner had finished with, and the user was told to go and fix the daemon's problem
    /// before they were allowed to close their own task.
    ///
    /// So the act goes through, and the two halves of the honesty it owes are both kept:
    /// nothing claims the orphan died, and the answer names it.
    #[tokio::test]
    async fn ending_a_session_with_a_process_from_a_previous_daemon_still_ends_it() {
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

        let answer = harness
            .core
            .close_session(&session_id, CloseDisposition::Terminate, 11)
            .expect("a process Turn cannot reach does not veto the user's own decision");

        // Named, with its pid, because the user's next step is a process list in another
        // terminal and the pid is what they will search for.
        let Response::Closed { escaped } = answer else {
            panic!("ending answers with what it could not stop: {answer:?}");
        };
        assert_eq!(escaped.len(), 1, "exactly the survivor: {escaped:?}");
        assert_eq!(escaped[0].node_id, orphan_id);
        assert_eq!(escaped[0].pid, Some(424_242));

        // And still not claimed dead. Fabricating the exit is the one thing that was never
        // on the table: it would release checkout authority while the real process may
        // still be writing to the very files this Session was working on.
        assert_eq!(
            harness.core.sessions[&session_id]
                .tree
                .get(&orphan_id)
                .unwrap()
                .lifecycle,
            Lifecycle::Orphaned
        );
        // The row leaves the tree, which is what the user asked for and what the refusal
        // used to prevent.
        assert_eq!(
            harness.core.sessions[&session_id].status,
            SessionStatus::Archived
        );
    }

    /// The same thing with a process that is genuinely running, and genuinely out of reach.
    ///
    /// The test above builds the orphan by hand, which proves the branch and not the claim.
    /// This one spawns a real process on a real pty and then drops the handle to it, which is
    /// exactly the state a daemon restart leaves behind: the process is in the process table,
    /// Turn knows its pid, and Turn cannot signal it. Ending the Session has to work, and it
    /// has to be honest — the process is still alive afterwards, and Turn says so.
    #[tokio::test]
    async fn a_process_that_outlived_its_daemon_is_named_rather_than_claimed_dead() {
        let mut harness = Harness::new().await;
        let session_id = SessionId::from_stored("sess_real_survivor");
        let pane_id = PaneId::from_stored("pane_real_survivor");
        harness.add_session(session_id.clone(), pane_id.clone(), 10);
        let node_id = harness.spawn_process(&session_id, &pane_id, 10).await;
        let pid = harness.core.sessions[&session_id]
            .tree
            .get(&node_id)
            .unwrap()
            .pid
            .expect("a spawned process has a pid");

        // Losing the handle while the process lives on is what a restart does. Held in a
        // local rather than dropped, and that detail is the whole fidelity of the test: the
        // pty's file descriptor has to stay open somewhere, because dropping it closes the
        // terminal and the process dies of that instead of surviving. After a real restart
        // it stays open in the kernel's books alone. Here it stays open out of the Core's
        // reach, which is the property being tested — `processes` has no entry for this
        // node, so nothing `close_session` does can signal it.
        let _survivor = harness.core.processes.remove(&node_id);
        harness
            .core
            .sessions
            .get_mut(&session_id)
            .unwrap()
            .tree
            .get_mut(&node_id)
            .unwrap()
            .lifecycle = Lifecycle::Orphaned;
        assert!(
            alive(pid),
            "the survivor is running before we end its Session"
        );

        let answer = harness
            .core
            .close_session(&session_id, CloseDisposition::Terminate, 11)
            .expect("ending is the user's decision, not the daemon's");

        let Response::Closed { escaped } = answer else {
            panic!("ending answers with what it could not stop: {answer:?}");
        };
        assert_eq!(escaped.len(), 1, "the survivor, named: {escaped:?}");
        assert_eq!(escaped[0].pid, Some(pid));
        assert_eq!(
            harness.core.sessions[&session_id].status,
            SessionStatus::Archived,
            "the Session ends whether or not its runaway process does"
        );
        assert!(
            alive(pid),
            "and nothing pretended otherwise: pid {pid} is still there, which is why the \
             answer names it"
        );

        // Left running would be a test that leaks a process per run.
        unsafe { kill(pid as i32, 9) };
    }

    /// A write that will not land does not cancel the end either.
    ///
    /// The other half of the same principle, and the one that is easier to get wrong because
    /// it reads as diligence: `persist_session` returns an error when the Session has an
    /// event checkpoint stuck behind a failed disk write, and `close_session` used to pass
    /// that up with a `?`. The user would then be refused the act *and* left with the row,
    /// on account of a storage problem they cannot see and did not cause. Ending is not
    /// conditional on the record of it succeeding.
    #[tokio::test]
    async fn a_store_that_will_not_take_the_write_does_not_cancel_the_end() {
        let mut harness = Harness::new().await;
        let session_id = SessionId::from_stored("sess_unwritable");
        let pane_id = PaneId::from_stored("pane_unwritable");
        harness.add_session(session_id.clone(), pane_id, 10);

        // The daemon's own "this Session cannot be written right now" state, put there the
        // way a failed ingest would: every later write for it is deferred and refused.
        harness
            .core
            .failed_ingest_checkpoints
            .push_back(crate::core::FailedIngestCheckpoint {
                event: turn_core::event::TurnEvent::new(
                    session_id.clone(),
                    turn_core::event::EventKind::ProcessExited { code: 0 },
                    turn_core::event::EventSource::Supervisor,
                    turn_core::event::Confidence::Explicit,
                    10,
                ),
                effects: Vec::new(),
            });
        assert!(
            harness.core.persist_session(&session_id).is_err(),
            "the premise of the test: this Session cannot be persisted"
        );

        harness
            .core
            .close_session(&session_id, CloseDisposition::Terminate, 11)
            .expect("a Session that will not persist has still ended");
        assert_eq!(
            harness.core.sessions[&session_id].status,
            SessionStatus::Archived,
            "and the window is told so, rather than being handed back the row"
        );
    }

    /// Signal 0 asks the kernel whether a process exists without touching it.
    fn alive(pid: u32) -> bool {
        unsafe { kill(pid as i32, 0) == 0 }
    }

    extern "C" {
        fn kill(pid: i32, signal: i32) -> i32;
    }

    /// And it stays gone across a restart of the daemon.
    ///
    /// The acceptance criterion of the report, and not implied by the assertion above: a
    /// Session archived only in memory would come back the next time `turnd` started, with
    /// the same orphan in it and the same banner on top of it.
    #[tokio::test]
    async fn a_session_ended_with_an_orphan_in_it_does_not_come_back() {
        let mut harness = Harness::new().await;
        let session_id = SessionId::from_stored("sess_orphan_close_durable");
        let pane_id = PaneId::from_stored("pane_orphan_close_durable");
        harness.add_session(session_id.clone(), pane_id, 10);
        let mut orphan = ProcessNode::process(
            session_id.clone(),
            turn_core::model::NodeKind::Shell,
            "sh",
            "/tmp",
            10,
        );
        orphan.lifecycle = Lifecycle::Orphaned;
        orphan.pid = Some(424_243);
        harness
            .core
            .sessions
            .get_mut(&session_id)
            .unwrap()
            .tree
            .insert(orphan);

        harness
            .core
            .close_session(&session_id, CloseDisposition::Terminate, 11)
            .expect("ending is authoritative");

        let stored = harness
            .core
            .store
            .sessions()
            .get(&session_id)
            .expect("the store can be read")
            .expect("the Session is still on disk, archived rather than forgotten");
        assert_eq!(
            stored.status,
            SessionStatus::Archived,
            "an end that only happened in memory is an end that undoes itself on restart"
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
        assert_eq!(
            answer,
            Response::Closed {
                escaped: Vec::new()
            },
            "everything here was reachable, so nothing is reported as surviving"
        );
        assert!(!harness.core.processes.contains_key(&parent_id));
        let session = &harness.core.sessions[&session_id];
        // Ending takes the row out of the tree. It used to leave it `Paused` — stopped, but
        // still listed as though it were work in progress — which is the whole reason the
        // verb was reported as not doing anything.
        assert_eq!(session.status, SessionStatus::Archived);
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
    async fn template_lifecycle_preserves_configuration_and_never_leaves_dangling_sessions() {
        let mut harness = Harness::new().await;
        let session_id = SessionId::from_stored("sess_template_lifecycle");
        harness.add_session(
            session_id.clone(),
            PaneId::from_stored("pane_before_template"),
            1,
        );
        let workspace_id = harness.core.sessions[&session_id].workspace_id.clone();

        let mut draft = Template::two_shells(2);
        draft.built_in = false;
        draft.name = "Complete development".into();
        draft.description = Some("All persisted fields".into());
        draft.icon = Some("terminal".into());
        draft.name_pattern = Some("work-{n}".into());
        draft.hotkey = Some("cmd+shift+7".into());
        draft.env = vec![("FROM_TEMPLATE".into(), "yes".into())];
        draft.attention = Some(turn_core::attention::AttentionPolicy::silent());
        draft.tmux = true;
        let pane_ids: Vec<_> = draft
            .layout
            .panes()
            .iter()
            .map(|pane| pane.id.clone())
            .collect();
        let first = draft.layout.get_mut(&pane_ids[0]).unwrap();
        first.cwd = Some(".".into());
        first.env = vec![("CELL".into(), "left".into())];
        first.restore = turn_core::model::RestoreBehaviour::Relaunch;
        let missing = "__turn_acceptance_missing_template_tool__";
        let second = draft.layout.get_mut(&pane_ids[1]).unwrap();
        second.kind = PaneKind::Terminal;
        second.command = Some(missing.into());
        second.args = vec!["--preserved".into()];
        second.restore = turn_core::model::RestoreBehaviour::Skip;

        let created = match harness.core.create_template(draft, 3).unwrap() {
            Response::Template { template } => template,
            other => panic!("unexpected {other:?}"),
        };
        assert_eq!(created.missing_commands, [missing]);
        let template_id = created.id.clone();

        let mut edited = match harness.core.get_template(&template_id).unwrap() {
            Response::TemplateDetails { template } => *template,
            other => panic!("unexpected {other:?}"),
        };
        edited.description = Some("Edited without JSON".into());
        let updated = match harness.core.update_template(&template_id, edited).unwrap() {
            Response::Template { template } => template,
            other => panic!("unexpected {other:?}"),
        };
        assert_eq!(updated.id, template_id);
        assert_eq!(updated.description.as_deref(), Some("Edited without JSON"));

        let duplicate = match harness
            .core
            .duplicate_template(&template_id, "Complete development copy".into(), 4)
            .unwrap()
        {
            Response::Template { template } => template,
            other => panic!("unexpected {other:?}"),
        };
        assert_ne!(duplicate.id, template_id);
        assert!(!duplicate.built_in);
        assert_eq!(duplicate.missing_commands, [missing]);

        harness
            .core
            .set_workspace_default_template(&workspace_id, Some(template_id.clone()), 5)
            .unwrap();
        harness
            .core
            .store
            .setting_layers()
            .set(
                turn_core::settings::Scope::Global,
                "",
                "templates.default",
                &serde_json::Value::String(template_id.as_str().into()),
                5,
            )
            .unwrap();

        harness
            .core
            .apply_template_to_session(&session_id, &template_id, 6)
            .unwrap();
        let applied = &harness.core.sessions[&session_id];
        assert_eq!(applied.template_id.as_ref(), Some(&template_id));
        assert_eq!(applied.env, [("FROM_TEMPLATE".into(), "yes".into())]);
        assert_eq!(
            applied.attention,
            turn_core::attention::AttentionPolicy::silent()
        );
        assert!(applied.tmux);
        assert_eq!(applied.layout.pane_count(), 2);
        let applied_panes = applied.layout.panes();
        assert_eq!(applied_panes[0].cwd.as_deref(), Some("."));
        assert_eq!(applied_panes[0].env, [("CELL".into(), "left".into())]);
        assert_eq!(applied_panes[1].command.as_deref(), Some(missing));
        assert_eq!(applied_panes[1].args, ["--preserved"]);
        let turn_core::model::LayoutNode::Split(split) = &applied.layout.root else {
            panic!("the two-column structure must survive instantiation");
        };
        assert_eq!(split.direction, Direction::Horizontal);
        assert!(split
            .children
            .iter()
            .all(|child| (child.size - 0.5).abs() < 0.001));

        let running_error = harness
            .core
            .apply_template_to_session(&session_id, &duplicate.id, 7)
            .expect_err("applying must never terminate a running Session implicitly");
        assert_eq!(running_error.code, ErrorCode::Refused);

        harness.core.delete_template(&template_id, 8).unwrap();
        assert!(!harness.core.templates.contains_key(&template_id));
        assert!(harness.core.templates.contains_key(&duplicate.id));
        assert_eq!(
            harness.core.workspaces[&workspace_id].default_template,
            None
        );
        let preserved = &harness.core.sessions[&session_id];
        assert_eq!(preserved.template_id, None);
        assert_eq!(preserved.layout.pane_count(), 2);
        assert_eq!(
            preserved.layout.panes()[1].command.as_deref(),
            Some(missing)
        );
        let stored = harness
            .core
            .store
            .sessions()
            .get(&session_id)
            .unwrap()
            .expect("the Session remains independently persisted");
        assert_eq!(stored.template_id, None);
        assert_eq!(stored.layout.pane_count(), 2);
        let global = harness
            .core
            .store
            .setting_layers()
            .layer(turn_core::settings::Scope::Global, "")
            .unwrap();
        assert!(global.get("templates.default").is_none());
    }

    #[tokio::test]
    async fn the_safe_template_builtin_is_read_only_but_can_be_duplicated() {
        let mut harness = Harness::new().await;
        let built_in = Template::two_shells(1);
        let id = built_in.id.clone();
        harness.core.templates.insert(id.clone(), built_in.clone());

        assert_eq!(
            harness
                .core
                .update_template(&id, built_in)
                .expect_err("built-ins are daemon-owned")
                .code,
            ErrorCode::Refused
        );
        assert_eq!(
            harness
                .core
                .delete_template(&id, 2)
                .expect_err("built-ins are portable recovery state")
                .code,
            ErrorCode::Refused
        );
        let copy = match harness
            .core
            .duplicate_template(&id, "My Two Shells".into(), 3)
            .unwrap()
        {
            Response::Template { template } => template,
            other => panic!("unexpected {other:?}"),
        };
        assert!(!copy.built_in);
        assert_eq!(copy.pane_count, 2);
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
    async fn favourite_and_pin_are_durable_independent_session_choices() {
        let mut harness = Harness::new().await;
        let session_id = SessionId::from_stored("sess_shortcuts");
        harness.add_session(
            session_id.clone(),
            PaneId::from_stored("pane_shortcuts"),
            10,
        );

        let favourite = harness
            .core
            .set_session_favourite(&session_id, true, 11)
            .unwrap();
        let Response::Session { session } = favourite else {
            panic!("favorite must return the changed Session")
        };
        assert!(session.favourite);
        assert!(!session.pinned);

        let pinned = harness
            .core
            .set_session_pinned(&session_id, true, 12)
            .unwrap();
        let Response::Session { session } = pinned else {
            panic!("pin must return the changed Session")
        };
        assert!(session.favourite && session.pinned);

        let stored = harness
            .core
            .store
            .sessions()
            .get(&session_id)
            .unwrap()
            .expect("the Session stays durable");
        assert!(stored.favourite && stored.pinned);
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
