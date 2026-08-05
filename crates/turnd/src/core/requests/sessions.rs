//! Session and template operations.

use super::workspaces::store;
use super::{check_name, Answer};
use crate::core::Core;
use crate::paths;
use std::path::{Path, PathBuf};
use std::process::Command as SystemCommand;
use turn_core::ids::{CheckoutId, SessionId, TemplateId, WorkspaceId};
use turn_core::model::{
    Direction, Layout, Pane, PaneKind, Session, SessionMode, SessionStatus, Template,
    WorkspaceCheckout,
};
use turn_proto::{CloseDisposition, NewPane, ProtoError, Response, ServerEvent, TemplateSummary};

impl Core {
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
    /// Until a platform process sandbox is available Turn persists the Session
    /// but deliberately launches no configured process. `read_only_enforced`
    /// remains false, so the UI cannot imply that an agent is safely confined.
    /// This degraded mode is useful for organising the task and is strictly safer
    /// than launching an unguarded shell that could write silently.
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
        session.read_only_enforced = false;
        session.cwd = self.validate_session_definition_cwds(&session)?;
        let id = session.id.clone();
        self.store
            .hierarchy()
            .create_read_only_session(&session, false)
            .map_err(store)?;
        self.sessions.insert(id.clone(), session);
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
    /// Processes are untouched either way: archiving is about the sidebar. A session
    /// with an agent mid-turn that the user files away keeps working, and comes back
    /// exactly as it was.
    pub(super) fn archive_session(
        &mut self,
        id: &SessionId,
        archived: bool,
        now_ms: i64,
    ) -> Answer {
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
        } else {
            self.push_session_state(id, now_ms);
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
            .filter(|node| node.is_running())
            .map(|node| node.id.clone())
            .collect();

        // Detach every client from this session's panes whatever the disposition: the
        // session is being closed on screen in all three cases.
        for pane in &panes {
            self.detach_everyone(id, pane);
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
                if let Ok(session) = self.session_mut(id) {
                    session.status = SessionStatus::Paused;
                }
                // The injected agent configuration goes with the processes it was
                // written for. Nothing will read it again, and a settings file naming a
                // hook URL that no longer answers is worse than no file.
                paths::remove_session_scratch(&self.data_dir, id);
                self.persist_session(id)?;
                self.push_session_state(id, now_ms);
            }
        }
        Ok(Response::Ack)
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
