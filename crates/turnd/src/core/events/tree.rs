//! Nodes that arrive rather than change: subagents and children.
//!
//! Both are additions to the tree, and both are governed by the same rule: the edge
//! carries the confidence of whoever reported it. A tool saying "I started this" is
//! [`Relation::Confirmed`]; a pid whose parent happens to match is
//! [`Relation::Inferred`]; nothing else is allowed to become an edge at all.

use super::Changed;
use crate::core::Core;
use turn_core::event::Confidence;
use turn_core::ids::{NodeId, SessionId};
use turn_core::model::{AgentName, NameSource, NodeKind, ProcessNode, Relation};
use turn_core::state::{Lifecycle, Turn};

impl Core {
    /// Adds a subagent a tool told us about.
    ///
    /// `Relation::Confirmed`, because this is not a guess: Claude Code's
    /// `SubagentStart` hook is the tool saying it started this itself. A subagent has
    /// no pty of its own — it runs inside its parent's process — so it has no pid, and
    /// pretending otherwise would put a number in the UI that matches nothing.
    pub(super) fn insert_subagent(
        &mut self,
        session_id: &SessionId,
        parent: &NodeId,
        declared_name: Option<String>,
        agent_type: Option<String>,
        agent_id: Option<String>,
        task: Option<String>,
        now_ms: i64,
    ) -> Changed {
        let Some(session) = self.sessions.get_mut(session_id) else {
            return Changed::default();
        };
        if session.tree.get(parent).is_none() {
            return Changed::default();
        }
        // A repeated SubagentStart for an id we already have is the same subagent.
        if let Some(id) = &agent_id {
            if let Some(existing) = session.tree.find_by_external_id(id) {
                let existing = existing.id.clone();
                return Changed {
                    node: Some(existing),
                    structure: false,
                    refused: false,
                };
            }
        }

        // A tool type is useful metadata, but it is not necessarily the name the
        // parent gave this worker. Prefer the explicit declaration and preserve it
        // independently so a later user rename never destroys the original.
        let title = declared_name
            .clone()
            .or_else(|| agent_type.clone())
            .unwrap_or_else(|| format!("Subagent {}", session.tree.subagent_count() + 1));
        let parent_command = session
            .tree
            .get(parent)
            .map(|node| node.command.clone())
            .unwrap_or_default();
        let cwd = session
            .tree
            .get(parent)
            .map(|node| node.cwd.clone())
            .unwrap_or_else(|| session.cwd.clone());

        let mut node = ProcessNode::agent(session_id.clone(), parent_command, cwd, now_ms);
        node.kind = NodeKind::Subagent;
        node.title = title.clone();
        node.lifecycle = Lifecycle::Alive;
        node.turn = Some(Turn::Active);
        if let Some(agent) = node.agent.as_mut() {
            agent.agent_type = agent_type;
            agent.external_id = agent_id;
            agent.current_task = task;
            agent.name = match declared_name {
                Some(name) => AgentName::declared(name),
                None => AgentName {
                    declared_name: None,
                    display_name: title.clone(),
                    source: if agent.agent_type.is_some() {
                        NameSource::Integration
                    } else {
                        NameSource::Fallback
                    },
                    confidence: if agent.agent_type.is_some() {
                        Confidence::Integrated
                    } else {
                        Confidence::Unknown
                    },
                    user_renamed: false,
                },
            };
        }
        node.link_to(parent.clone(), Relation::Confirmed);
        let id = session.tree.insert(node);
        Changed {
            node: Some(id),
            structure: true,
            refused: false,
        }
    }

    /// Ends a subagent.
    pub(super) fn stop_subagent(
        &mut self,
        session_id: &SessionId,
        parent: &NodeId,
        agent_id: Option<&str>,
        now_ms: i64,
    ) -> Changed {
        let Some(session) = self.sessions.get_mut(session_id) else {
            return Changed::default();
        };
        let target = match agent_id {
            Some(id) => session.tree.find_by_external_id(id).map(|n| n.id.clone()),
            // With no id to match on, the most recently started running subagent of
            // this parent is the only defensible guess, and it is only reached when the
            // tool declines to tell us.
            None => session
                .tree
                .children(parent)
                .into_iter()
                .filter(|n| n.kind == NodeKind::Subagent && n.is_running())
                .max_by_key(|n| n.started_ms)
                .map(|n| n.id.clone()),
        };
        let Some(target) = target else {
            return Changed::default();
        };
        if let Some(node) = session.tree.get_mut(&target) {
            node.lifecycle = Lifecycle::Exited { code: 0 };
            node.turn = Some(Turn::Done);
            node.ended_ms = Some(now_ms);
            node.interaction_pending = false;
        }
        // A subagent that has finished cannot still be waiting for you.
        self.attention.resolve_node(&target);
        Changed {
            node: Some(target),
            structure: true,
            refused: false,
        }
    }

    /// Adds a child process, with the relation the reporter could honestly claim.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn insert_child(
        &mut self,
        session_id: &SessionId,
        parent: &NodeId,
        child: NodeId,
        pid: u32,
        command: String,
        confirmed_parent: bool,
        now_ms: i64,
    ) -> Changed {
        let Some(session) = self.sessions.get_mut(session_id) else {
            return Changed::default();
        };
        if session.tree.get(&child).is_some() || session.tree.find_by_pid(pid).is_some() {
            return Changed::default();
        }
        let cwd = session
            .tree
            .get(parent)
            .map(|node| node.cwd.clone())
            .unwrap_or_else(|| session.cwd.clone());
        let kind = turn_pty::classify(&command);
        let mut node = ProcessNode::process(session_id.clone(), kind, command, cwd, now_ms);
        node.id = child;
        node.pid = Some(pid);
        node.lifecycle = Lifecycle::Alive;
        node.title = node
            .command
            .split_whitespace()
            .next()
            .unwrap_or(&node.command)
            .rsplit('/')
            .next()
            .unwrap_or("process")
            .to_string();
        // Inferred unless a tool said so. The UI draws the difference.
        let relation = if confirmed_parent {
            Relation::Confirmed
        } else {
            Relation::Inferred
        };
        node.link_to(parent.clone(), relation);
        let id = session.tree.insert(node);
        Changed {
            node: Some(id),
            structure: true,
            refused: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::testing::Harness;
    use turn_core::event::{Confidence, EventKind, EventSource, TurnEvent};
    use turn_core::ids::PaneId;

    const NOW: i64 = 1_775_000_000_000;

    #[tokio::test]
    async fn reviewer_is_a_named_background_child_and_never_opens_a_pane() {
        let mut harness = Harness::new().await;
        let session_id = SessionId::from_stored("sess_hierarchy");
        let pane_id = PaneId::from_stored("pane_primary");
        harness.add_session(session_id.clone(), pane_id.clone(), NOW);

        let mut parent = ProcessNode::agent(session_id.clone(), "claude", "/tmp", NOW);
        parent.lifecycle = Lifecycle::Alive;
        let parent_id = parent.id.clone();
        harness
            .core
            .sessions
            .get_mut(&session_id)
            .unwrap()
            .tree
            .insert(parent);

        let event = TurnEvent::new(
            session_id.clone(),
            EventKind::AgentSpawned {
                declared_name: Some("Reviewer".into()),
                agent_type: Some("Explore".into()),
                agent_id: Some("reviewer-1".into()),
                task: Some("Reviewing climb_system.gd…".into()),
            },
            EventSource::Hook {
                tool: "claude-code".into(),
                event_name: "SubagentStart".into(),
            },
            Confidence::Explicit,
            NOW + 1,
        )
        .with_node(parent_id.clone());
        harness.core.ingest(event, NOW + 1);

        let session = &harness.core.sessions[&session_id];
        let reviewer = session
            .tree
            .children(&parent_id)
            .into_iter()
            .next()
            .expect("Reviewer appears below Claude");
        let name = &reviewer.agent.as_ref().unwrap().name;
        assert_eq!(name.declared_name.as_deref(), Some("Reviewer"));
        assert_eq!(name.display_name, "Reviewer");
        assert_eq!(reviewer.relationship.confidence, Confidence::Explicit);
        assert_eq!(
            reviewer
                .activity_preview
                .as_ref()
                .map(|preview| preview.normalized_text.as_str()),
            Some("Reviewing climb_system.gd…")
        );
        assert!(
            session
                .layout
                .panes()
                .iter()
                .all(|pane| pane.node_id.is_none()),
            "discovering a child must not bind or split a pane"
        );
        assert!(
            !harness.core.processes.contains_key(&reviewer.id),
            "a structured child without its own PTY must not invent a process handle"
        );

        let restored = harness
            .core
            .store
            .sessions()
            .get(&session_id)
            .unwrap()
            .expect("the session was persisted");
        let restored_reviewer = restored
            .tree
            .find_by_external_id("reviewer-1")
            .expect("the child relation survives a store round-trip");
        assert_eq!(
            restored_reviewer
                .agent
                .as_ref()
                .unwrap()
                .name
                .declared_name
                .as_deref(),
            Some("Reviewer")
        );
        assert!(restored_reviewer.activity_preview.is_some());
    }
}
