//! Workspaces: the persistent project a session belongs to.

use crate::attention::AttentionPolicy;
use crate::ids::{TemplateId, WorkspaceId};
use serde::{Deserialize, Serialize};

/// A project or environment that outlives any single session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Workspace {
    pub id: WorkspaceId,
    pub name: String,
    /// Absolute path to the project root.
    pub root: String,
    pub git_remote: Option<String>,
    /// Environment applied to every process started in this workspace.
    pub env: Vec<(String, String)>,
    pub default_shell: Option<String>,
    /// The agent `New Session` preselects.
    pub default_agent: Option<String>,
    /// Commands run once when a session starts here.
    pub init_commands: Vec<String>,
    pub default_template: Option<TemplateId>,
    /// Baseline policy; sessions may override it.
    pub attention: AttentionPolicy,
    pub colour: Option<String>,
    pub icon: Option<String>,
    pub created_ms: i64,
    pub last_used_ms: i64,
    /// Whether Turn may route this workspace's sessions through tmux.
    pub tmux_enabled: bool,
    pub archived: bool,
    /// Set only for legacy state where Turn cannot prove which still-live Session
    /// owns the primary checkout. No lease is granted until reconciled.
    #[serde(default)]
    pub lease_reconciliation_required: bool,
}

impl Workspace {
    pub fn new(name: impl Into<String>, root: impl Into<String>, now_ms: i64) -> Self {
        Self {
            id: WorkspaceId::new(),
            name: name.into(),
            root: root.into(),
            git_remote: None,
            env: Vec::new(),
            default_shell: None,
            default_agent: None,
            init_commands: Vec::new(),
            default_template: None,
            attention: AttentionPolicy::default(),
            colour: None,
            icon: None,
            created_ms: now_ms,
            last_used_ms: now_ms,
            tmux_enabled: false,
            archived: false,
            lease_reconciliation_required: false,
        }
    }

    /// The workspace name inferred from a path, for the quick-create flow.
    pub fn name_from_path(path: &str) -> String {
        path.trim_end_matches('/')
            .rsplit('/')
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or("workspace")
            .to_string()
    }

    pub fn touch(&mut self, now_ms: i64) {
        self.last_used_ms = now_ms;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_workspace_starts_with_quiet_defaults() {
        let ws = Workspace::new("turn", "/Users/x/turn", 0);
        assert!(!ws.tmux_enabled, "tmux is opt-in");
        assert!(!ws.archived);
        assert_eq!(ws.attention, AttentionPolicy::default());
    }

    #[test]
    fn names_are_inferred_from_the_last_path_segment() {
        assert_eq!(
            Workspace::name_from_path("/Users/x/personal-workspace/turn"),
            "turn"
        );
        assert_eq!(Workspace::name_from_path("/Users/x/turn/"), "turn");
        assert_eq!(Workspace::name_from_path("/"), "workspace");
        assert_eq!(Workspace::name_from_path(""), "workspace");
    }
}
