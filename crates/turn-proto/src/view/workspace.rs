//! Workspace and template projections.

use serde::{Deserialize, Serialize};
use turn_core::ids::{TemplateId, WorkspaceId};
use turn_core::model::{Template, Workspace};

use super::session::SessionSummary;

/// One row of the workspace switcher.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct WorkspaceSummary {
    pub id: WorkspaceId,
    pub name: String,
    pub root: String,
    pub git_remote: Option<String>,
    pub colour: Option<String>,
    pub icon: Option<String>,
    pub archived: bool,

    pub session_count: usize,
    /// Sessions inside this workspace that are blocked on the human. This is what
    /// puts a dot on a workspace the user is not currently looking at.
    pub sessions_needing_user: usize,
    /// Total outstanding attention demands across the workspace's sessions.
    pub badge_count: usize,

    pub default_agent: Option<String>,
    pub default_shell: Option<String>,
    pub default_template: Option<TemplateId>,
    pub tmux_enabled: bool,
    pub created_ms: i64,
    pub last_used_ms: i64,
}

impl WorkspaceSummary {
    /// Projects a workspace, deriving its counts from the session summaries the
    /// daemon has already built.
    ///
    /// Taking summaries rather than sessions is deliberate: it guarantees the
    /// workspace badge and the session badges can never disagree, because they
    /// are the same numbers added up.
    pub fn from_workspace(workspace: &Workspace, sessions: &[SessionSummary]) -> Self {
        let mine: Vec<&SessionSummary> = sessions
            .iter()
            .filter(|s| s.workspace_id == workspace.id)
            .collect();
        Self {
            id: workspace.id.clone(),
            name: workspace.name.clone(),
            root: workspace.root.clone(),
            git_remote: workspace.git_remote.clone(),
            colour: workspace.colour.clone(),
            icon: workspace.icon.clone(),
            archived: workspace.archived,
            session_count: mine.len(),
            sessions_needing_user: mine.iter().filter(|s| s.needs_user).count(),
            badge_count: mine.iter().map(|s| s.badge_count).sum(),
            default_agent: workspace.default_agent.clone(),
            default_shell: workspace.default_shell.clone(),
            default_template: workspace.default_template.clone(),
            tmux_enabled: workspace.tmux_enabled,
            created_ms: workspace.created_ms,
            last_used_ms: workspace.last_used_ms,
        }
    }
}

/// One entry in the new-session template picker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct TemplateSummary {
    pub id: TemplateId,
    pub name: String,
    pub description: Option<String>,
    pub icon: Option<String>,
    pub hotkey: Option<String>,
    /// Whether Turn ships this template. Built-ins cannot be deleted.
    pub built_in: bool,
    pub pane_count: usize,
    /// The commands this template would start, in pane order. Shown in the picker
    /// so choosing a template is an informed decision rather than a surprise —
    /// materialising one launches processes.
    pub commands: Vec<String>,
    pub name_pattern: Option<String>,
    pub tmux: bool,
    pub created_ms: i64,
}

impl TemplateSummary {
    pub fn from_template(template: &Template) -> Self {
        Self {
            id: template.id.clone(),
            name: template.name.clone(),
            description: template.description.clone(),
            icon: template.icon.clone(),
            hotkey: template.hotkey.clone(),
            built_in: template.built_in,
            pane_count: template.layout.pane_count(),
            commands: template
                .layout
                .panes()
                .iter()
                .filter_map(|p| p.command.clone())
                .collect(),
            name_pattern: template.name_pattern.clone(),
            tmux: template.tmux,
            created_ms: template.created_ms,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use turn_core::model::{Layout, Pane, PaneKind, ProcessNode, Session};
    use turn_core::state::{AwaitingReason, Lifecycle, Turn};

    const T0: i64 = 1_700_000_000_000;

    fn workspace() -> Workspace {
        Workspace::new("turn", "/Users/x/turn", T0)
    }

    fn session_in(workspace: &Workspace, name: &str) -> Session {
        Session::new(
            workspace.id.clone(),
            name,
            "/Users/x/turn",
            Layout::single(Pane::new(PaneKind::Agent).with_command("claude")),
            T0,
        )
    }

    #[test]
    fn workspace_counts_are_the_sum_of_its_session_summaries() {
        let ws = workspace();
        let quiet = session_in(&ws, "quiet");

        let mut blocked = session_in(&ws, "blocked");
        let mut agent = ProcessNode::agent(blocked.id.clone(), "claude", "/", T0);
        agent.lifecycle = Lifecycle::Alive;
        agent.turn = Some(Turn::AwaitingUser {
            reason: AwaitingReason::Permission,
        });
        blocked.tree.insert(agent);

        let summaries = vec![
            SessionSummary::from_session(&quiet, 0, false, T0),
            SessionSummary::from_session(&blocked, 2, false, T0),
        ];
        let view = WorkspaceSummary::from_workspace(&ws, &summaries);

        assert_eq!(view.session_count, 2);
        assert_eq!(view.sessions_needing_user, 1);
        assert_eq!(view.badge_count, 2);
    }

    #[test]
    fn sessions_from_another_workspace_are_not_counted() {
        let mine = workspace();
        let other = Workspace::new("elsewhere", "/tmp/elsewhere", T0);
        let summaries = vec![
            SessionSummary::from_session(&session_in(&mine, "a"), 1, false, T0),
            SessionSummary::from_session(&session_in(&other, "b"), 5, false, T0),
        ];
        let view = WorkspaceSummary::from_workspace(&mine, &summaries);
        assert_eq!(view.session_count, 1);
        assert_eq!(view.badge_count, 1, "another project's noise stays there");
    }

    #[test]
    fn a_workspace_with_no_sessions_summarises_to_zeroes() {
        let view = WorkspaceSummary::from_workspace(&workspace(), &[]);
        assert_eq!(view.session_count, 0);
        assert_eq!(view.badge_count, 0);
        assert_eq!(view.sessions_needing_user, 0);
        assert!(!view.archived);
    }

    #[test]
    fn a_template_summary_lists_the_commands_it_would_start() {
        let view = TemplateSummary::from_template(&Template::pair_of_agents(T0));
        assert_eq!(view.pane_count, 2);
        assert_eq!(view.commands, vec!["claude", "codex"]);
        assert!(view.built_in);
    }

    #[test]
    fn a_template_pane_with_no_command_contributes_no_command() {
        // The Coding template has an agent, a shell with no command, and one of
        // Turn's own views which never has a process.
        let view = TemplateSummary::from_template(&Template::coding(T0));
        assert_eq!(view.pane_count, 3);
        assert_eq!(view.commands, vec!["claude"]);
        assert_eq!(view.hotkey.as_deref(), Some("cmd+shift+1"));
    }

    #[test]
    fn workspace_and_template_summaries_round_trip() {
        let ws = WorkspaceSummary::from_workspace(&workspace(), &[]);
        let json = serde_json::to_string(&ws).unwrap();
        assert!(json.contains("\"session_count\":0"), "got {json}");
        assert_eq!(serde_json::from_str::<WorkspaceSummary>(&json).unwrap(), ws);

        let template = TemplateSummary::from_template(&Template::pr_review(T0));
        let json = serde_json::to_string(&template).unwrap();
        assert_eq!(
            serde_json::from_str::<TemplateSummary>(&json).unwrap(),
            template
        );
    }
}
