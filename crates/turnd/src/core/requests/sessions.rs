//! Session and template operations.

use super::workspaces::store;
use super::{check_name, Answer};
use crate::core::Core;
use crate::paths;
use turn_core::ids::{SessionId, TemplateId, WorkspaceId};
use turn_core::model::{Direction, Layout, Pane, PaneKind, Session, SessionStatus, Template};
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
        let id = session.id.clone();
        self.sessions.insert(id.clone(), session);
        self.persist_session(&id)?;

        self.run_init_commands(&id, &workspace.init_commands.clone(), now_ms);
        self.materialise_session(&id, now_ms);
        self.touch_workspace(workspace_id, now_ms);
        self.persist_session(&id)?;
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
        let id = session.id.clone();
        self.sessions.insert(id.clone(), session);
        self.persist_session(&id)?;

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
