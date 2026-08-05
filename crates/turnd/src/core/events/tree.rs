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
    #[allow(clippy::too_many_arguments)]
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
                if let Some(node) = session.tree.get_mut(&existing) {
                    if let Some(agent) = node.agent.as_mut() {
                        if let Some(name) = declared_name.filter(|name| !name.trim().is_empty()) {
                            agent.name.declared_name = Some(name.clone());
                            agent.name.source = NameSource::ExplicitParentEvent;
                            agent.name.confidence = Confidence::Explicit;
                            if !agent.name.user_renamed {
                                agent.name.display_name = name.clone();
                                node.title = name;
                            }
                        }
                        if let Some(kind) = agent_type.filter(|kind| !kind.trim().is_empty()) {
                            agent.agent_type = Some(kind);
                        }
                        if let Some(task) = task.filter(|task| !task.trim().is_empty()) {
                            agent.current_task = Some(task);
                        }
                        agent.agent.external_id = Some(id.clone());
                    }
                }
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
            agent.external_id = agent_id.clone();
            agent.agent.external_id = agent_id;
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
        ppid: Option<u32>,
        command: String,
        observed_cwd: Option<String>,
        confirmed_parent: bool,
        now_ms: i64,
    ) -> Changed {
        let Some(session) = self.sessions.get_mut(session_id) else {
            return Changed::default();
        };
        if session.tree.get(&child).is_some() || session.tree.find_by_pid(pid).is_some() {
            return Changed::default();
        }
        let cwd = observed_cwd.unwrap_or_else(|| {
            session
                .tree
                .get(parent)
                .map(|node| node.cwd.clone())
                .unwrap_or_else(|| session.cwd.clone())
        });
        let kind = turn_pty::classify(&command);
        let mut node = ProcessNode::process(session_id.clone(), kind, command, cwd, now_ms);
        node.id = child;
        node.pid = Some(pid);
        node.ppid = ppid;
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
    use turn_core::event::{AgentRef, Confidence, EventKind, EventSource, Risk, TurnEvent};
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

    #[tokio::test]
    async fn a_later_declaration_enriches_one_worker_without_losing_a_user_rename() {
        let mut harness = Harness::new().await;
        let session_id = SessionId::from_stored("sess_enrich");
        harness.add_session(session_id.clone(), PaneId::from_stored("pane_enrich"), NOW);
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

        harness.core.insert_subagent(
            &session_id,
            &parent_id,
            None,
            Some("Explore".into()),
            Some("worker-1".into()),
            None,
            NOW + 1,
        );
        let worker_id = harness.core.sessions[&session_id]
            .tree
            .find_by_external_id("worker-1")
            .unwrap()
            .id
            .clone();
        harness
            .core
            .sessions
            .get_mut(&session_id)
            .unwrap()
            .tree
            .get_mut(&worker_id)
            .unwrap()
            .agent
            .as_mut()
            .unwrap()
            .name
            .rename("My reviewer");
        harness.core.insert_subagent(
            &session_id,
            &parent_id,
            Some("Reviewer".into()),
            Some("code-review".into()),
            Some("worker-1".into()),
            Some("Review the climbing diff".into()),
            NOW + 2,
        );

        let tree = &harness.core.sessions[&session_id].tree;
        assert_eq!(tree.subagent_count(), 1);
        let worker = tree.get(&worker_id).unwrap().agent.as_ref().unwrap();
        assert_eq!(worker.name.declared_name.as_deref(), Some("Reviewer"));
        assert_eq!(worker.name.display_name, "My reviewer");
        assert_eq!(
            worker.current_task.as_deref(),
            Some("Review the climbing diff")
        );
    }

    #[tokio::test]
    async fn a_worker_id_routes_permission_and_attention_state_to_the_subagent() {
        let mut harness = Harness::new().await;
        let session_id = SessionId::from_stored("sess_worker_attention");
        harness.add_session(
            session_id.clone(),
            PaneId::from_stored("pane_worker_attention"),
            NOW,
        );
        let mut parent = ProcessNode::agent(session_id.clone(), "claude", "/tmp", NOW);
        parent.lifecycle = Lifecycle::Alive;
        parent.turn = Some(Turn::Active);
        let parent_id = parent.id.clone();
        harness
            .core
            .sessions
            .get_mut(&session_id)
            .unwrap()
            .tree
            .insert(parent);
        harness.core.insert_subagent(
            &session_id,
            &parent_id,
            Some("Reviewer".into()),
            Some("Explore".into()),
            Some("worker-permission".into()),
            None,
            NOW + 1,
        );
        let worker_id = harness.core.sessions[&session_id]
            .tree
            .find_by_external_id("worker-permission")
            .unwrap()
            .id
            .clone();

        let permission = TurnEvent::new(
            session_id.clone(),
            EventKind::AgentPermissionRequired {
                summary: "Reviewer wants to run tests".into(),
                command: Some("cargo test".into()),
                tool_name: Some("Bash".into()),
                risk: Risk::Low,
            },
            EventSource::Hook {
                tool: "claude-code".into(),
                event_name: "Notification".into(),
            },
            Confidence::Explicit,
            NOW + 2,
        )
        .with_node(parent_id.clone())
        .with_agent(AgentRef {
            provider: Some("anthropic".into()),
            tool: Some("claude-code".into()),
            model: None,
            external_id: Some("worker-permission".into()),
        });
        harness.core.ingest(permission, NOW + 2);

        let tree = &harness.core.sessions[&session_id].tree;
        assert!(matches!(
            tree.get(&worker_id).unwrap().turn,
            Some(Turn::AwaitingUser {
                reason: turn_core::state::AwaitingReason::Permission
            })
        ));
        assert_eq!(tree.get(&parent_id).unwrap().turn, Some(Turn::Active));
    }

    #[tokio::test]
    async fn one_child_correlates_an_idless_worker_demand_and_resume_without_lying() {
        let mut harness = Harness::new().await;
        let session_id = SessionId::from_stored("sess_idless_worker");
        harness.add_session(
            session_id.clone(),
            PaneId::from_stored("pane_idless_worker"),
            NOW,
        );
        let mut parent = ProcessNode::agent(session_id.clone(), "claude", "/tmp", NOW);
        parent.lifecycle = Lifecycle::Alive;
        parent.turn = Some(Turn::Active);
        let parent_id = parent.id.clone();
        harness
            .core
            .sessions
            .get_mut(&session_id)
            .unwrap()
            .tree
            .insert(parent);
        harness.core.insert_subagent(
            &session_id,
            &parent_id,
            Some("Reviewer".into()),
            Some("Explore".into()),
            Some("worker-reviewer".into()),
            None,
            NOW + 1,
        );
        let reviewer_id = harness.core.sessions[&session_id]
            .tree
            .find_by_external_id("worker-reviewer")
            .unwrap()
            .id
            .clone();

        let permission = TurnEvent::new(
            session_id.clone(),
            EventKind::AgentPermissionRequired {
                summary: "Reviewer needs permission".into(),
                command: None,
                tool_name: Some("Bash".into()),
                risk: Risk::Medium,
            },
            EventSource::Hook {
                tool: "claude-code".into(),
                event_name: "Notification".into(),
            },
            Confidence::Explicit,
            NOW + 2,
        )
        .with_parent(parent_id.clone());
        harness.core.ingest(permission, NOW + 2);

        let tree = &harness.core.sessions[&session_id].tree;
        assert!(matches!(
            tree.get(&reviewer_id).unwrap().turn,
            Some(Turn::AwaitingUser {
                reason: turn_core::state::AwaitingReason::Permission
            })
        ));
        assert_eq!(tree.get(&parent_id).unwrap().turn, Some(Turn::Active));
        let queued = harness.core.attention.queue().iter().next().unwrap();
        assert_eq!(queued.node_id.as_ref(), Some(&reviewer_id));
        assert_eq!(queued.confidence, Confidence::InferredHigh);

        let resumed = TurnEvent::new(
            session_id.clone(),
            EventKind::AgentTurnStarted {
                prompt_excerpt: Some("allow".into()),
            },
            EventSource::Hook {
                tool: "claude-code".into(),
                event_name: "UserPromptSubmit".into(),
            },
            Confidence::Explicit,
            NOW + 3,
        )
        .with_parent(parent_id.clone());
        harness.core.ingest(resumed, NOW + 3);

        let tree = &harness.core.sessions[&session_id].tree;
        assert_eq!(tree.get(&reviewer_id).unwrap().turn, Some(Turn::Active));
        assert_eq!(tree.get(&parent_id).unwrap().turn, Some(Turn::Active));
        assert!(harness.core.attention.queue().is_empty());
        assert_eq!(
            harness.core.turn_authority.get(&reviewer_id),
            Some(&Confidence::InferredHigh),
            "the hook fact is explicit, but the child attribution remains inferred"
        );
    }

    #[tokio::test]
    async fn multiple_children_keep_idless_attention_unassigned_and_preserve_sibling_demands() {
        let mut harness = Harness::new().await;
        let session_id = SessionId::from_stored("sess_ambiguous_workers");
        harness.add_session(
            session_id.clone(),
            PaneId::from_stored("pane_ambiguous_workers"),
            NOW,
        );
        let mut parent = ProcessNode::agent(session_id.clone(), "claude", "/tmp", NOW);
        parent.lifecycle = Lifecycle::Alive;
        parent.turn = Some(Turn::Active);
        let parent_id = parent.id.clone();
        harness
            .core
            .sessions
            .get_mut(&session_id)
            .unwrap()
            .tree
            .insert(parent);
        for (name, id) in [("Reviewer", "worker-reviewer"), ("Tests", "worker-tests")] {
            harness.core.insert_subagent(
                &session_id,
                &parent_id,
                Some(name.into()),
                Some("Explore".into()),
                Some(id.into()),
                None,
                NOW + 1,
            );
        }
        let reviewer_id = harness.core.sessions[&session_id]
            .tree
            .find_by_external_id("worker-reviewer")
            .unwrap()
            .id
            .clone();
        let tests_id = harness.core.sessions[&session_id]
            .tree
            .find_by_external_id("worker-tests")
            .unwrap()
            .id
            .clone();

        let ambiguous = TurnEvent::new(
            session_id.clone(),
            EventKind::AgentWaitingForUser {
                reason: turn_core::state::AwaitingReason::Input,
                summary: Some("A worker needs input".into()),
            },
            EventSource::Hook {
                tool: "claude-code".into(),
                event_name: "Notification".into(),
            },
            Confidence::Explicit,
            NOW + 2,
        )
        .with_parent(parent_id.clone());
        harness.core.ingest(ambiguous, NOW + 2);

        assert_eq!(
            harness.core.sessions[&session_id]
                .tree
                .get(&parent_id)
                .unwrap()
                .turn,
            Some(Turn::Active)
        );
        assert_eq!(
            harness.core.sessions[&session_id]
                .tree
                .get(&reviewer_id)
                .unwrap()
                .turn,
            Some(Turn::Active)
        );
        let unassigned = harness.core.attention.queue().iter().next().unwrap();
        assert_eq!(unassigned.node_id, None);
        assert_eq!(unassigned.confidence, Confidence::Unknown);

        let reviewer_permission = TurnEvent::new(
            session_id.clone(),
            EventKind::AgentPermissionRequired {
                summary: "Reviewer needs permission".into(),
                command: None,
                tool_name: Some("Bash".into()),
                risk: Risk::Medium,
            },
            EventSource::Hook {
                tool: "claude-code".into(),
                event_name: "Notification".into(),
            },
            Confidence::Explicit,
            NOW + 3,
        )
        .with_node(reviewer_id.clone());
        harness.core.ingest(reviewer_permission, NOW + 3);
        assert_eq!(harness.core.attention.queue().len(), 2);

        let resumed = TurnEvent::new(
            session_id.clone(),
            EventKind::AgentTurnStarted {
                prompt_excerpt: Some("continue".into()),
            },
            EventSource::Hook {
                tool: "claude-code".into(),
                event_name: "UserPromptSubmit".into(),
            },
            Confidence::Explicit,
            NOW + 4,
        )
        .with_parent(parent_id.clone());
        harness.core.ingest(resumed, NOW + 4);

        let remaining: Vec<_> = harness.core.attention.queue().iter().collect();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].node_id.as_ref(), Some(&reviewer_id));
        assert!(matches!(
            harness.core.sessions[&session_id]
                .tree
                .get(&reviewer_id)
                .unwrap()
                .turn,
            Some(Turn::AwaitingUser {
                reason: turn_core::state::AwaitingReason::Permission
            })
        ));
        assert_eq!(
            harness.core.sessions[&session_id]
                .tree
                .get(&tests_id)
                .unwrap()
                .turn,
            Some(Turn::Active)
        );
    }

    #[tokio::test]
    async fn an_unknown_explicit_worker_id_never_falls_through_to_unique_child_inference() {
        let mut harness = Harness::new().await;
        let session_id = SessionId::from_stored("sess_out_of_order_worker");
        harness.add_session(
            session_id.clone(),
            PaneId::from_stored("pane_out_of_order_worker"),
            NOW,
        );
        let mut parent = ProcessNode::agent(session_id.clone(), "claude", "/tmp", NOW);
        parent.lifecycle = Lifecycle::Alive;
        parent.turn = Some(Turn::Active);
        let parent_id = parent.id.clone();
        harness
            .core
            .sessions
            .get_mut(&session_id)
            .unwrap()
            .tree
            .insert(parent);
        harness.core.insert_subagent(
            &session_id,
            &parent_id,
            Some("Existing tests worker".into()),
            Some("Explore".into()),
            Some("worker-existing".into()),
            None,
            NOW + 1,
        );
        let existing_id = harness.core.sessions[&session_id]
            .tree
            .find_by_external_id("worker-existing")
            .unwrap()
            .id
            .clone();

        let out_of_order = TurnEvent::new(
            session_id.clone(),
            EventKind::AgentPermissionRequired {
                summary: "Future Reviewer needs permission".into(),
                command: None,
                tool_name: Some("Bash".into()),
                risk: Risk::Medium,
            },
            EventSource::Hook {
                tool: "claude-code".into(),
                event_name: "Notification".into(),
            },
            Confidence::Explicit,
            NOW + 2,
        )
        .with_parent(parent_id.clone())
        .with_agent(AgentRef {
            provider: Some("anthropic".into()),
            tool: Some("claude-code".into()),
            model: None,
            external_id: Some("worker-future-reviewer".into()),
        });
        harness.core.ingest(out_of_order, NOW + 2);

        let tree = &harness.core.sessions[&session_id].tree;
        assert_eq!(tree.get(&existing_id).unwrap().turn, Some(Turn::Active));
        assert_eq!(tree.get(&parent_id).unwrap().turn, Some(Turn::Active));
        let queued = harness.core.attention.queue().iter().next().unwrap();
        assert_eq!(queued.node_id, None);
        assert_eq!(queued.parent_node_id.as_ref(), Some(&parent_id));
        assert_eq!(
            queued.subject_external_id.as_deref(),
            Some("worker-future-reviewer")
        );
        assert_eq!(queued.confidence, Confidence::Unknown);
        let persisted = harness
            .core
            .store
            .events()
            .list_for_session(&session_id, 1)
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(persisted.node_id, None);
        assert_eq!(persisted.parent_node_id.as_ref(), Some(&parent_id));
        assert_eq!(
            persisted.agent.external_id.as_deref(),
            Some("worker-future-reviewer")
        );
        assert_eq!(persisted.confidence, Confidence::Unknown);

        // The declaration arrives after the demand. A later callback carrying
        // that same identity now resolves exactly to the newly known node and
        // closes the earlier external-id scope without touching Existing.
        harness.core.insert_subagent(
            &session_id,
            &parent_id,
            Some("Future Reviewer".into()),
            Some("Explore".into()),
            Some("worker-future-reviewer".into()),
            None,
            NOW + 3,
        );
        let future_id = harness.core.sessions[&session_id]
            .tree
            .find_by_external_id("worker-future-reviewer")
            .unwrap()
            .id
            .clone();
        let resumed = TurnEvent::new(
            session_id.clone(),
            EventKind::AgentTurnStarted {
                prompt_excerpt: Some("continue".into()),
            },
            EventSource::Hook {
                tool: "claude-code".into(),
                event_name: "UserPromptSubmit".into(),
            },
            Confidence::Explicit,
            NOW + 4,
        )
        .with_parent(parent_id)
        .with_agent(AgentRef {
            provider: Some("anthropic".into()),
            tool: Some("claude-code".into()),
            model: None,
            external_id: Some("worker-future-reviewer".into()),
        });
        harness.core.ingest(resumed, NOW + 4);

        assert!(harness.core.attention.queue().is_empty());
        assert_eq!(
            harness.core.sessions[&session_id]
                .tree
                .get(&future_id)
                .unwrap()
                .turn,
            Some(Turn::Active)
        );
        assert_eq!(
            harness.core.sessions[&session_id]
                .tree
                .get(&existing_id)
                .unwrap()
                .turn,
            Some(Turn::Active)
        );
    }

    #[tokio::test]
    async fn idless_resumes_are_scoped_to_one_of_two_parents_in_the_same_session() {
        let mut harness = Harness::new().await;
        let session_id = SessionId::from_stored("sess_two_agent_parents");
        harness.add_session(
            session_id.clone(),
            PaneId::from_stored("pane_two_agent_parents"),
            NOW,
        );
        let mut parents = Vec::new();
        for title in ["Claude A", "Claude B"] {
            let mut parent = ProcessNode::agent(session_id.clone(), "claude", "/tmp", NOW);
            parent.title = title.into();
            parent.lifecycle = Lifecycle::Alive;
            parent.turn = Some(Turn::Active);
            let parent_id = parent.id.clone();
            harness
                .core
                .sessions
                .get_mut(&session_id)
                .unwrap()
                .tree
                .insert(parent);
            for suffix in ["reviewer", "tests"] {
                harness.core.insert_subagent(
                    &session_id,
                    &parent_id,
                    Some(format!("{title} {suffix}")),
                    Some("Explore".into()),
                    Some(format!(
                        "{}-{suffix}",
                        title.replace(' ', "-").to_lowercase()
                    )),
                    None,
                    NOW + 1,
                );
            }
            parents.push(parent_id);
        }

        for (offset, parent) in parents.iter().enumerate() {
            let demand = TurnEvent::new(
                session_id.clone(),
                EventKind::AgentWaitingForUser {
                    reason: turn_core::state::AwaitingReason::Input,
                    summary: Some(format!("worker under {parent} needs input")),
                },
                EventSource::Hook {
                    tool: "claude-code".into(),
                    event_name: "Notification".into(),
                },
                Confidence::Explicit,
                NOW + 10 + offset as i64,
            )
            .with_parent(parent.clone());
            harness.core.ingest(demand, NOW + 10 + offset as i64);
        }
        assert_eq!(harness.core.attention.queue().len(), 2);
        assert!(parents.iter().all(
            |parent| harness
                .core
                .attention
                .queue()
                .has_unresolved_scope(&session_id, parent, None)
        ));

        let resumed_a = TurnEvent::new(
            session_id.clone(),
            EventKind::AgentTurnStarted {
                prompt_excerpt: Some("answer A".into()),
            },
            EventSource::Hook {
                tool: "claude-code".into(),
                event_name: "UserPromptSubmit".into(),
            },
            Confidence::Explicit,
            NOW + 20,
        )
        .with_parent(parents[0].clone());
        harness.core.ingest(resumed_a, NOW + 20);

        let remaining: Vec<_> = harness.core.attention.queue().iter().collect();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].node_id, None);
        assert_eq!(remaining[0].parent_node_id.as_ref(), Some(&parents[1]));
        assert_eq!(remaining[0].confidence, Confidence::Unknown);
    }
}
