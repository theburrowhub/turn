//! Templates: a reusable session shape.

use crate::attention::AttentionPolicy;
use crate::ids::TemplateId;
use crate::model::layout::{Direction, Layout, LayoutNode, Pane, PaneKind};
use serde::{Deserialize, Serialize};

/// A saved session shape: panes, commands and policy, without any live process.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Template {
    pub id: TemplateId,
    pub name: String,
    pub description: Option<String>,
    pub icon: Option<String>,
    /// The pane tree. Panes carry commands and cwds relative to the session root.
    pub layout: Layout,
    /// Overrides the workspace's attention policy when set.
    pub attention: Option<AttentionPolicy>,
    /// Commands run before any pane starts.
    pub init_commands: Vec<String>,
    /// Pattern for auto-naming sessions, e.g. `"Review {branch}"`.
    pub name_pattern: Option<String>,
    pub hotkey: Option<String>,
    pub env: Vec<(String, String)>,
    pub tmux: bool,
    pub built_in: bool,
    pub created_ms: i64,
}

impl Template {
    /// Captures a live layout as a reusable template.
    ///
    /// Node bindings are stripped: a template describes what to start, never
    /// which process instance it was cloned from.
    pub fn from_layout(name: impl Into<String>, layout: &Layout, now_ms: i64) -> Self {
        let mut layout = layout.clone();
        strip_runtime(&mut layout.root);
        layout.zoomed = None;
        Self {
            id: TemplateId::new(),
            name: name.into(),
            description: None,
            icon: None,
            layout,
            attention: None,
            init_commands: Vec::new(),
            name_pattern: None,
            hotkey: None,
            env: Vec::new(),
            tmux: false,
            built_in: false,
            created_ms: now_ms,
        }
    }

    /// A fresh layout for a new session, with new pane ids so two sessions from
    /// one template never share identity.
    pub fn instantiate(&self) -> Layout {
        let mut layout = self.layout.reidentified();
        layout.normalise();
        layout
    }

    /// The built-in set the app ships with.
    pub fn built_ins(now_ms: i64) -> Vec<Template> {
        vec![
            Self::blank(now_ms),
            Self::coding(now_ms),
            Self::pr_review(now_ms),
            Self::pair_of_agents(now_ms),
        ]
    }

    /// One shell. The escape hatch when no template fits.
    pub fn blank(now_ms: i64) -> Template {
        let pane = Pane::new(PaneKind::Shell)
            .with_title("shell")
            .with_restore(crate::model::layout::RestoreBehaviour::Relaunch);
        let mut t = Self::from_layout("Blank", &Layout::single(pane), now_ms);
        t.built_in = true;
        t.description = Some("A single shell.".into());
        t
    }

    /// Agent on the left, shell and file TUI stacked on the right. Navigation is
    /// never a Pane: the unified workspace tree already owns that responsibility.
    pub fn coding(now_ms: i64) -> Template {
        let agent = Pane::new(PaneKind::Agent)
            .with_command("claude")
            .with_title("claude");
        let shell = Pane::new(PaneKind::Shell)
            .with_title("shell")
            .with_restore(crate::model::layout::RestoreBehaviour::Relaunch);
        let files = Pane::new(PaneKind::Tui)
            .with_command("fang")
            .with_title("fang (files)")
            .with_restore(crate::model::layout::RestoreBehaviour::Relaunch);

        let mut layout = Layout::single(agent);
        let agent_id = layout.panes()[0].id.clone();
        layout.split(&agent_id, Direction::Horizontal, shell);
        let shell_id = layout.active.clone().unwrap();
        layout.split(&shell_id, Direction::Vertical, files);
        // The agent deserves the larger share.
        layout.resize(&agent_id, 0.15);
        layout.active = Some(agent_id);

        let mut t = Self::from_layout("Coding", &layout, now_ms);
        t.built_in = true;
        t.description = Some("Agent, shell and Fang file browser.".into());
        t.hotkey = Some("cmd+shift+1".into());
        t
    }

    /// An agent primed for review plus a git TUI.
    pub fn pr_review(now_ms: i64) -> Template {
        let agent = Pane::new(PaneKind::Agent)
            .with_command("claude")
            .with_title("review");
        let git = Pane::new(PaneKind::Tui)
            .with_command("lazygit")
            .with_title("lazygit")
            .with_restore(crate::model::layout::RestoreBehaviour::Relaunch);

        let mut layout = Layout::single(agent);
        let agent_id = layout.panes()[0].id.clone();
        layout.split(&agent_id, Direction::Horizontal, git);
        layout.active = Some(agent_id);

        let mut t = Self::from_layout("PR Review", &layout, now_ms);
        t.built_in = true;
        t.description = Some("Review agent alongside lazygit.".into());
        t.name_pattern = Some("Review {branch}".into());
        t
    }

    /// Two agents side by side, for the parallel case the product is named after.
    pub fn pair_of_agents(now_ms: i64) -> Template {
        let left = Pane::new(PaneKind::Agent)
            .with_command("claude")
            .with_title("claude");
        let right = Pane::new(PaneKind::Agent)
            .with_command("codex")
            .with_title("codex");

        let mut layout = Layout::single(left);
        let left_id = layout.panes()[0].id.clone();
        layout.split(&left_id, Direction::Horizontal, right);
        layout.active = Some(left_id);

        let mut t = Self::from_layout("Pair of Agents", &layout, now_ms);
        t.built_in = true;
        t.description = Some("Claude Code and Codex, side by side.".into());
        t
    }

    /// Fills a name pattern. Unknown placeholders are left alone rather than
    /// blanked, so a typo is visible instead of silently swallowed.
    pub fn render_name(&self, branch: Option<&str>, task: Option<&str>) -> Option<String> {
        let pattern = self.name_pattern.as_ref()?;
        let mut out = pattern.clone();
        if let Some(branch) = branch {
            out = out.replace("{branch}", branch);
        }
        if let Some(task) = task {
            out = out.replace("{task}", task);
        }
        Some(out)
    }
}

/// Clears live process bindings from a captured layout.
fn strip_runtime(node: &mut LayoutNode) {
    match node {
        LayoutNode::Leaf(pane) => pane.node_id = None,
        LayoutNode::Split(split) => {
            for child in split.children.iter_mut() {
                strip_runtime(&mut child.node);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::NodeId;

    #[test]
    fn the_built_in_set_is_present_and_valid() {
        let templates = Template::built_ins(0);
        let names: Vec<_> = templates.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["Blank", "Coding", "PR Review", "Pair of Agents"]
        );
        for t in &templates {
            assert!(t.built_in);
            assert!(
                t.layout.sizes_are_normalised(),
                "{} has broken geometry",
                t.name
            );
            assert!(t.layout.pane_count() >= 1);
        }
    }

    #[test]
    fn the_coding_template_gives_the_agent_the_larger_share() {
        let t = Template::coding(0);
        assert_eq!(t.layout.pane_count(), 3);
        match &t.layout.root {
            LayoutNode::Split(split) => {
                assert_eq!(split.direction, Direction::Horizontal);
                assert!(
                    split.children[0].size > split.children[1].size,
                    "agent pane should be widest"
                );
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn saving_a_live_layout_as_a_template_drops_process_bindings() {
        let mut layout = Layout::single(Pane::new(PaneKind::Agent).with_command("claude"));
        let pane_id = layout.panes()[0].id.clone();
        layout.get_mut(&pane_id).unwrap().node_id = Some(NodeId::from_stored("proc_live001"));

        let t = Template::from_layout("Captured", &layout, 0);
        assert!(
            t.layout.panes().iter().all(|p| p.node_id.is_none()),
            "a template must not remember which process it was cloned from"
        );
        assert_eq!(t.layout.panes()[0].command.as_deref(), Some("claude"));
    }

    /// The last step of the first vertical: a second session from the same
    /// template must be independent of the first.
    #[test]
    fn two_sessions_from_one_template_share_no_pane_ids() {
        let t = Template::coding(0);
        let a = t.instantiate();
        let b = t.instantiate();

        let ids_a: Vec<_> = a.panes().iter().map(|p| p.id.clone()).collect();
        let ids_b: Vec<_> = b.panes().iter().map(|p| p.id.clone()).collect();

        assert_eq!(ids_a.len(), ids_b.len());
        for id in &ids_a {
            assert!(!ids_b.contains(id), "pane id {id} leaked between sessions");
        }
        // But the shape and commands are identical.
        let cmds_a: Vec<_> = a.panes().iter().map(|p| p.command.clone()).collect();
        let cmds_b: Vec<_> = b.panes().iter().map(|p| p.command.clone()).collect();
        assert_eq!(cmds_a, cmds_b);
        assert!(a.sizes_are_normalised() && b.sizes_are_normalised());
    }

    #[test]
    fn instantiating_focuses_a_real_pane_and_clears_zoom() {
        let t = Template::coding(0);
        let layout = t.instantiate();
        let active = layout.active.clone().expect("something must be focused");
        assert!(layout.get(&active).is_some());
        assert_eq!(layout.zoomed, None);
    }

    #[test]
    fn name_patterns_fill_known_placeholders_and_leave_unknown_ones_visible() {
        let t = Template::pr_review(0);
        assert_eq!(
            t.render_name(Some("feat/attention"), None).as_deref(),
            Some("Review feat/attention")
        );

        let mut odd = t.clone();
        odd.name_pattern = Some("{task} on {mystery}".into());
        assert_eq!(
            odd.render_name(None, Some("Fix bug")).as_deref(),
            Some("Fix bug on {mystery}"),
            "an unknown placeholder stays visible instead of vanishing"
        );
    }

    #[test]
    fn a_template_with_no_pattern_yields_no_name() {
        assert!(Template::blank(0).render_name(Some("main"), None).is_none());
    }

    #[test]
    fn templates_round_trip_through_json() {
        let t = Template::pair_of_agents(0);
        let json = serde_json::to_string(&t).unwrap();
        let back: Template = serde_json::from_str(&json).unwrap();
        assert_eq!(t, back);
    }
}
