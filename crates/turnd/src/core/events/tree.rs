//! Nodes that arrive rather than change: subagents and children.
//!
//! Both are additions to the tree, and both are governed by the same rule: the edge
//! carries the confidence of whoever reported it. A tool saying "I started this" is
//! [`Relation::Confirmed`]; a pid whose parent happens to match is
//! [`Relation::Inferred`]; nothing else is allowed to become an edge at all.

use super::{
    safe_untrusted_args, safe_untrusted_label, Changed, MAX_AGENT_TASK_CHARS,
    MAX_DISCOVERED_COMMAND_CHARS, MAX_DISCOVERED_CWD_CHARS, UNPRINTABLE_LABEL,
};
use crate::core::Core;
use turn_core::event::Confidence;
use turn_core::ids::{NodeId, SessionId};
use turn_core::model::{
    AgentIdentitySource, AgentName, NameSource, NodeKind, ProcessNode, Relation,
};
use turn_core::state::{Lifecycle, Turn};

impl Core {
    /// Adds a subagent a tool told us about.
    ///
    /// `Relation::Confirmed`, because this is not a guess: Claude Code's
    /// `SubagentStart` hook is the tool saying it started this itself. A subagent has
    /// no pty of its own — it runs inside its parent's process — so it has no pid, and
    /// pretending otherwise would put a number in the UI that matches nothing.
    #[allow(clippy::too_many_arguments)]
    #[cfg(test)]
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
        self.insert_subagent_from(
            session_id,
            parent,
            declared_name,
            agent_type,
            agent_id,
            task,
            None,
            now_ms,
        )
    }

    /// Adds or enriches a subagent while retaining the structured identity channel.
    ///
    /// Claude Agent Teams currently declares one logical worker twice: the parent-side
    /// Agent tool result uses `name@session-*`, while `SubagentStart`/`Stop` use an
    /// `a<name>-*` lifecycle id. Exact ids always win. Cross-channel alias correlation
    /// is allowed only under the same parent and only when it has one unambiguous
    /// opposite-channel candidate.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn insert_subagent_from(
        &mut self,
        session_id: &SessionId,
        parent: &NodeId,
        declared_name: Option<String>,
        agent_type: Option<String>,
        agent_id: Option<String>,
        task: Option<String>,
        identity_source: Option<AgentIdentitySource>,
        now_ms: i64,
    ) -> Changed {
        // A declared name is authored by an agent which may itself be processing
        // adversarial repository content. Preserve its semantic value, but never
        // its ability to inject terminal controls, bidi state or unbounded text
        // into the persistent tree. `ingest` performs the same projection on the
        // event; this is defence for direct/internal callers.
        let declared_name =
            declared_name.and_then(|name| safe_untrusted_label(&name, turn_pty::MAX_TITLE_CHARS));
        let agent_type =
            agent_type.and_then(|kind| safe_untrusted_label(&kind, turn_pty::MAX_TITLE_CHARS));
        let task = task.and_then(|task| safe_untrusted_label(&task, MAX_AGENT_TASK_CHARS));
        let Some(session) = self.sessions.get_mut(session_id) else {
            return Changed::default();
        };
        if session.tree.get(parent).is_none() {
            return Changed::default();
        }
        // An exact id is authoritative. If it misses, a name can pair two different
        // structured channels only when exactly one opposite-channel sibling has the
        // same alias. Two same-named siblings remain distinct rather than guessed.
        let exact = agent_id.as_deref().and_then(|id| {
            session
                .tree
                .children(parent)
                .into_iter()
                .find(|node| {
                    node.agent
                        .as_ref()
                        .is_some_and(|agent| agent.matches_external_id(id))
                })
                .map(|node| node.id.clone())
        });
        let correlated = match (
            exact.as_ref(),
            identity_source,
            correlation_alias(
                identity_source,
                &declared_name,
                &agent_type,
                agent_id.as_deref(),
            ),
        ) {
            (None, Some(source), Some(alias)) => {
                let opposite = opposite_identity_source(source);
                let candidates: Vec<_> = session
                    .tree
                    .children(parent)
                    .into_iter()
                    .filter(|node| node.kind == NodeKind::Subagent)
                    .filter(|node| {
                        node.agent.as_ref().is_some_and(|agent| {
                            agent.has_identity_source(opposite)
                                && !agent.has_identity_source(source)
                        })
                    })
                    .filter(|node| node_correlation_alias(node, opposite) == Some(alias.as_str()))
                    .map(|node| node.id.clone())
                    .collect();
                match candidates.as_slice() {
                    [only] => Some(only.clone()),
                    _ => None,
                }
            }
            _ => None,
        };
        if let Some(existing) = exact.or(correlated) {
            let terminal = session
                .tree
                .get(&existing)
                .is_some_and(|node| node.lifecycle.is_terminal());
            if let Some(node) = session.tree.get_mut(&existing) {
                enrich_subagent(
                    node,
                    declared_name,
                    agent_type,
                    agent_id.as_deref(),
                    task,
                    identity_source,
                );
            }
            return Changed {
                node: Some(existing),
                structure: true,
                // A delayed declaration may enrich a terminal tombstone, but
                // it cannot start a new lifecycle for the same tool identity.
                refused: terminal,
            };
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
            agent.agent.external_id = agent_id.clone();
            if let (Some(source), Some(id)) = (identity_source, agent_id) {
                agent.record_identity_alias(source, id);
            }
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

    /// Materialises a durable terminal identity when Stop wins the race against
    /// Start. A later declaration enriches this node through `insert_subagent`
    /// but deliberately cannot revive it.
    pub(super) fn insert_stopped_subagent(
        &mut self,
        session_id: &SessionId,
        parent: &NodeId,
        agent_id: String,
        identity_source: Option<AgentIdentitySource>,
        now_ms: i64,
    ) -> Changed {
        let Some(session) = self.sessions.get_mut(session_id) else {
            return Changed::default();
        };
        if session.tree.get(parent).is_none() {
            return Changed::default();
        }
        if let Some(existing) = session.tree.children(parent).into_iter().find(|node| {
            node.agent
                .as_ref()
                .is_some_and(|agent| agent.matches_external_id(&agent_id))
        }) {
            let existing = existing.id.clone();
            if let Some(node) = session.tree.get_mut(&existing) {
                if let (Some(agent), Some(source)) = (node.agent.as_mut(), identity_source) {
                    agent.record_identity_alias(source, agent_id.clone());
                }
                node.lifecycle = Lifecycle::Exited { code: 0 };
                node.turn = Some(Turn::Done);
                node.ended_ms = Some(now_ms);
                super::clear_interaction_state(node);
            }
            return Changed {
                node: Some(existing),
                structure: true,
                refused: false,
            };
        }

        let title = format!("Subagent {}", session.tree.subagent_count() + 1);
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
        node.lifecycle = Lifecycle::Exited { code: 0 };
        node.turn = Some(Turn::Done);
        node.ended_ms = Some(now_ms);
        if let Some(agent) = node.agent.as_mut() {
            agent.external_id = Some(agent_id.clone());
            agent.agent.external_id = Some(agent_id.clone());
            if let Some(source) = identity_source {
                agent.record_identity_alias(source, agent_id);
            }
            agent.name = AgentName {
                declared_name: None,
                display_name: title,
                source: NameSource::Fallback,
                confidence: Confidence::Unknown,
                user_renamed: false,
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
        target: &NodeId,
        now_ms: i64,
    ) -> Changed {
        let Some(session) = self.sessions.get_mut(session_id) else {
            return Changed::default();
        };
        if session
            .tree
            .get(target)
            .is_none_or(|node| node.kind != NodeKind::Subagent)
        {
            return Changed::default();
        }
        if let Some(node) = session.tree.get_mut(target) {
            node.lifecycle = Lifecycle::Exited { code: 0 };
            node.turn = Some(Turn::Done);
            node.ended_ms = Some(now_ms);
            super::clear_interaction_state(node);
        }
        Changed {
            node: Some(target.clone()),
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
        args: Vec<String>,
        observed_cwd: Option<String>,
        confirmed_parent: bool,
        now_ms: i64,
    ) -> Changed {
        // ProcessSupervisor keeps the raw OS values long enough to classify and
        // traverse them. A child node is the human-facing, durable projection and
        // therefore must not retain hostile argv/path text. Repeat the ingress
        // normalisation here so no internal caller can bypass it.
        let command = safe_untrusted_label(&command, MAX_DISCOVERED_COMMAND_CHARS)
            .unwrap_or_else(|| "process".into());
        let args = safe_untrusted_args(args);
        let observed_cwd = observed_cwd.map(|cwd| {
            safe_untrusted_label(&cwd, MAX_DISCOVERED_CWD_CHARS)
                .unwrap_or_else(|| UNPRINTABLE_LABEL.into())
        });
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
        node.args = args;
        node.lifecycle = Lifecycle::Alive;
        let executable = node
            .command
            .split_whitespace()
            .next()
            .unwrap_or(&node.command)
            .rsplit('/')
            .next()
            .unwrap_or("process");
        node.title = safe_untrusted_label(executable, turn_pty::MAX_TITLE_CHARS)
            .unwrap_or_else(|| "process".into());
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

fn opposite_identity_source(source: AgentIdentitySource) -> AgentIdentitySource {
    match source {
        AgentIdentitySource::Lifecycle => AgentIdentitySource::ParentSpawn,
        AgentIdentitySource::ParentSpawn => AgentIdentitySource::Lifecycle,
    }
}

fn parent_spawn_id_alias(external_id: &str) -> Option<&str> {
    let (name, session) = external_id.split_once('@')?;
    (!name.is_empty() && session.starts_with("session-")).then_some(name)
}

fn correlation_alias(
    source: Option<AgentIdentitySource>,
    declared_name: &Option<String>,
    agent_type: &Option<String>,
    external_id: Option<&str>,
) -> Option<String> {
    let alias = match source? {
        AgentIdentitySource::Lifecycle => declared_name.as_deref().or(agent_type.as_deref()),
        AgentIdentitySource::ParentSpawn => declared_name
            .as_deref()
            .or_else(|| external_id.and_then(parent_spawn_id_alias)),
    }?;
    (!alias.trim().is_empty()).then(|| alias.to_string())
}

fn node_correlation_alias(node: &ProcessNode, source: AgentIdentitySource) -> Option<&str> {
    let agent = node.agent.as_ref()?;
    match source {
        AgentIdentitySource::Lifecycle => agent
            .name
            .declared_name
            .as_deref()
            .or(agent.agent_type.as_deref()),
        AgentIdentitySource::ParentSpawn => agent.name.declared_name.as_deref().or_else(|| {
            agent
                .identity_aliases
                .iter()
                .find(|alias| alias.source == AgentIdentitySource::ParentSpawn)
                .and_then(|alias| parent_spawn_id_alias(&alias.external_id))
        }),
    }
}

fn enrich_subagent(
    node: &mut ProcessNode,
    declared_name: Option<String>,
    agent_type: Option<String>,
    agent_id: Option<&str>,
    task: Option<String>,
    identity_source: Option<AgentIdentitySource>,
) {
    let Some(agent) = node.agent.as_mut() else {
        return;
    };
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
        // The parent's structured spawn result knows both the human name and the
        // reusable agent type. A lifecycle hook may put the name in `agent_type`,
        // so it only fills a missing type and never downgrades the richer result.
        if identity_source == Some(AgentIdentitySource::ParentSpawn)
            || identity_source.is_none()
            || agent.agent_type.is_none()
        {
            agent.agent_type = Some(kind);
        }
    }
    if let Some(task) = task.filter(|task| !task.trim().is_empty()) {
        agent.current_task = Some(task);
    }
    if let Some(id) = agent_id {
        let had_lifecycle = agent.has_identity_source(AgentIdentitySource::Lifecycle);
        if let Some(source) = identity_source {
            agent.record_identity_alias(source, id.to_string());
        }
        match identity_source {
            // The lifecycle id is the worker's own runtime identity and therefore
            // the preferred indexed/resume identity once it is available.
            Some(AgentIdentitySource::Lifecycle) => {
                agent.external_id = Some(id.to_string());
                agent.agent.external_id = Some(id.to_string());
            }
            Some(AgentIdentitySource::ParentSpawn) if had_lifecycle => {}
            _ => {
                agent.external_id = Some(id.to_string());
                agent.agent.external_id = Some(id.to_string());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{MAX_DISCOVERED_ARGS, MAX_DISCOVERED_ARGV_CHARS, MAX_DISCOVERED_ARG_CHARS};
    use super::*;
    use crate::core::testing::Harness;
    use turn_agents::{IntegrationLevel, OutputHeuristic};
    use turn_core::event::{AgentRef, Confidence, EventKind, EventSource, Risk, TurnEvent};
    use turn_core::ids::PaneId;

    const NOW: i64 = 1_775_000_000_000;

    fn assert_safe_and_bounded(value: &str, max_chars: usize) {
        assert!(
            value.chars().all(turn_pty::is_display_safe),
            "unsafe display character survived in {value:?}"
        );
        assert!(
            value.chars().count() <= max_chars,
            "{} characters exceeded the {max_chars}-character bound",
            value.chars().count()
        );
    }

    fn add_parent(harness: &mut Harness, session_id: &SessionId, suffix: &str) -> NodeId {
        let mut parent =
            ProcessNode::agent(session_id.clone(), format!("claude-{suffix}"), "/tmp", NOW);
        parent.lifecycle = Lifecycle::Alive;
        let parent_id = parent.id.clone();
        harness
            .core
            .sessions
            .get_mut(session_id)
            .unwrap()
            .tree
            .insert(parent);
        parent_id
    }

    fn team_spawn(
        session_id: &SessionId,
        parent: &NodeId,
        name: &str,
        external_id: &str,
        at: i64,
    ) -> TurnEvent {
        TurnEvent::new(
            session_id.clone(),
            EventKind::AgentSpawned {
                declared_name: Some(name.into()),
                agent_type: Some("general-purpose".into()),
                agent_id: Some(external_id.into()),
                task: Some(format!("Task for {name}")),
            },
            EventSource::Hook {
                tool: "claude-code".into(),
                event_name: "PostToolUse".into(),
            },
            Confidence::Explicit,
            at,
        )
        .with_node(parent.clone())
    }

    fn lifecycle_start(
        session_id: &SessionId,
        parent: &NodeId,
        name: &str,
        external_id: &str,
        at: i64,
    ) -> TurnEvent {
        TurnEvent::new(
            session_id.clone(),
            EventKind::AgentSpawned {
                declared_name: None,
                agent_type: Some(name.into()),
                agent_id: Some(external_id.into()),
                task: None,
            },
            EventSource::Hook {
                tool: "claude-code".into(),
                event_name: "SubagentStart".into(),
            },
            Confidence::Explicit,
            at,
        )
        .with_node(parent.clone())
    }

    fn lifecycle_stop(
        session_id: &SessionId,
        parent: &NodeId,
        external_id: &str,
        at: i64,
    ) -> TurnEvent {
        TurnEvent::new(
            session_id.clone(),
            EventKind::AgentSubagentStopped {
                agent_id: Some(external_id.into()),
            },
            EventSource::Hook {
                tool: "claude-code".into(),
                event_name: "SubagentStop".into(),
            },
            Confidence::Explicit,
            at,
        )
        .with_parent(parent.clone())
    }

    #[tokio::test]
    async fn claude_team_and_lifecycle_declarations_merge_in_both_orders_and_round_trip() {
        for (suffix, lifecycle_first) in [("team-first", false), ("lifecycle-first", true)] {
            let mut harness = Harness::new().await;
            let session_id = SessionId::from_stored(format!("sess_alias_{suffix}"));
            harness.add_session(
                session_id.clone(),
                PaneId::from_stored(format!("pane_alias_{suffix}")),
                NOW,
            );
            let parent = add_parent(&mut harness, &session_id, suffix);
            let team_id = format!("frase-1@session-{suffix}");
            let lifecycle_id = format!("afrase-1-{suffix}");
            let team = team_spawn(&session_id, &parent, "frase-1", &team_id, NOW + 1);
            let lifecycle =
                lifecycle_start(&session_id, &parent, "frase-1", &lifecycle_id, NOW + 2);
            if lifecycle_first {
                harness.core.ingest(lifecycle, NOW + 1);
                harness.core.ingest(team, NOW + 2);
            } else {
                harness.core.ingest(team, NOW + 1);
                harness.core.ingest(lifecycle, NOW + 2);
            }

            let tree = &harness.core.sessions[&session_id].tree;
            assert_eq!(tree.children(&parent).len(), 1, "{suffix}");
            let by_team = tree.find_by_external_id(&team_id).unwrap().id.clone();
            let by_lifecycle = tree.find_by_external_id(&lifecycle_id).unwrap().id.clone();
            assert_eq!(by_team, by_lifecycle, "both ids address one AgentNode");
            let worker = tree.get(&by_team).unwrap().agent.as_ref().unwrap();
            assert_eq!(worker.name.display_name, "frase-1");
            assert_eq!(worker.agent_type.as_deref(), Some("general-purpose"));
            assert_eq!(worker.identity_aliases.len(), 2);

            let restored = harness
                .core
                .store
                .sessions()
                .get(&session_id)
                .unwrap()
                .unwrap();
            assert_eq!(
                restored.tree.find_by_external_id(&team_id).unwrap().id,
                restored.tree.find_by_external_id(&lifecycle_id).unwrap().id,
                "aliases survive the SQLite/serde round trip"
            );
        }
    }

    #[tokio::test]
    async fn either_claude_alias_stops_the_same_merged_subagent() {
        for (suffix, stop_with_team_id) in [("team-stop", true), ("lifecycle-stop", false)] {
            let mut harness = Harness::new().await;
            let session_id = SessionId::from_stored(format!("sess_{suffix}"));
            harness.add_session(
                session_id.clone(),
                PaneId::from_stored(format!("pane_{suffix}")),
                NOW,
            );
            let parent = add_parent(&mut harness, &session_id, suffix);
            let team_id = format!("worker@session-{suffix}");
            let lifecycle_id = format!("aworker-{suffix}");
            harness.core.ingest(
                team_spawn(&session_id, &parent, "worker", &team_id, NOW + 1),
                NOW + 1,
            );
            harness.core.ingest(
                lifecycle_start(&session_id, &parent, "worker", &lifecycle_id, NOW + 2),
                NOW + 2,
            );
            let stop_id = if stop_with_team_id {
                &team_id
            } else {
                &lifecycle_id
            };
            harness.core.ingest(
                lifecycle_stop(&session_id, &parent, stop_id, NOW + 3),
                NOW + 3,
            );

            let tree = &harness.core.sessions[&session_id].tree;
            assert_eq!(tree.children(&parent).len(), 1);
            let worker = tree.find_by_external_id(&team_id).unwrap();
            assert!(worker.lifecycle.is_terminal(), "stop through {stop_id}");
            assert_eq!(
                worker.id,
                tree.find_by_external_id(&lifecycle_id).unwrap().id
            );
        }
    }

    #[tokio::test]
    async fn simultaneous_same_named_siblings_are_never_paired_by_guess() {
        let mut harness = Harness::new().await;
        let session_id = SessionId::from_stored("sess_ambiguous_aliases");
        harness.add_session(
            session_id.clone(),
            PaneId::from_stored("pane_ambiguous_aliases"),
            NOW,
        );
        let parent = add_parent(&mut harness, &session_id, "ambiguous");
        for (id, at) in [
            ("worker@session-one", NOW + 1),
            ("worker@session-two", NOW + 2),
        ] {
            harness
                .core
                .ingest(team_spawn(&session_id, &parent, "worker", id, at), at);
        }
        harness.core.ingest(
            lifecycle_start(&session_id, &parent, "worker", "aworker-one", NOW + 3),
            NOW + 3,
        );

        let tree = &harness.core.sessions[&session_id].tree;
        assert_eq!(tree.children(&parent).len(), 3);
        let lifecycle = tree.find_by_external_id("aworker-one").unwrap();
        assert_ne!(
            lifecycle.id,
            tree.find_by_external_id("worker@session-one").unwrap().id
        );
        assert_ne!(
            lifecycle.id,
            tree.find_by_external_id("worker@session-two").unwrap().id
        );
    }

    #[tokio::test]
    async fn identical_aliases_under_different_parents_never_cross_parent_boundaries() {
        let mut harness = Harness::new().await;
        let session_id = SessionId::from_stored("sess_parent_scoped_aliases");
        harness.add_session(
            session_id.clone(),
            PaneId::from_stored("pane_parent_scoped_aliases"),
            NOW,
        );
        let first_parent = add_parent(&mut harness, &session_id, "first");
        let second_parent = add_parent(&mut harness, &session_id, "second");
        harness.core.ingest(
            team_spawn(
                &session_id,
                &first_parent,
                "worker",
                "worker@session-first",
                NOW + 1,
            ),
            NOW + 1,
        );
        harness.core.ingest(
            team_spawn(
                &session_id,
                &second_parent,
                "worker",
                "worker@session-second",
                NOW + 2,
            ),
            NOW + 2,
        );
        harness.core.ingest(
            lifecycle_start(
                &session_id,
                &first_parent,
                "worker",
                "aworker-first",
                NOW + 3,
            ),
            NOW + 3,
        );

        let tree = &harness.core.sessions[&session_id].tree;
        assert_eq!(tree.children(&first_parent).len(), 1);
        assert_eq!(tree.children(&second_parent).len(), 1);
        let first = tree.find_by_external_id("aworker-first").unwrap();
        assert_eq!(first.parent.as_ref(), Some(&first_parent));
        assert_eq!(
            tree.find_by_external_id("worker@session-second")
                .unwrap()
                .parent
                .as_ref(),
            Some(&second_parent)
        );
    }

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
    async fn an_agent_declaration_is_safe_and_bounded_in_event_tree_and_inspector() {
        let mut harness = Harness::new().await;
        let session_id = SessionId::from_stored("sess_hostile_declaration");
        harness.add_session(
            session_id.clone(),
            PaneId::from_stored("pane_hostile_declaration"),
            NOW,
        );
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

        let declared_name = format!(
            "Rev\x1b[31miew\u{009b}2Jer\u{202e}\u{200b}\u{200d}\n{}",
            "n".repeat(turn_pty::MAX_TITLE_CHARS * 8)
        );
        let agent_type = "Expl\u{202e}ore\u{200b}".to_string();
        let task = format!(
            "Review\ncurrent\u{200d} diff\x1b]0;forged\x07 {}",
            "t".repeat(MAX_AGENT_TASK_CHARS * 8)
        );
        harness.core.ingest(
            TurnEvent::new(
                session_id.clone(),
                EventKind::AgentSpawned {
                    declared_name: Some(declared_name),
                    agent_type: Some(agent_type),
                    agent_id: Some("hostile-reviewer-1".into()),
                    task: Some(task),
                },
                EventSource::Hook {
                    tool: "claude-code".into(),
                    event_name: "SubagentStart".into(),
                },
                Confidence::Explicit,
                NOW + 1,
            )
            .with_node(parent_id.clone()),
            NOW + 1,
        );

        let worker = harness.core.sessions[&session_id]
            .tree
            .children(&parent_id)
            .into_iter()
            .next()
            .expect("the declaration still creates its AgentNode");
        let info = worker.agent.as_ref().unwrap();
        let declared = info.name.declared_name.as_deref().unwrap();
        assert_safe_and_bounded(declared, turn_pty::MAX_TITLE_CHARS);
        assert_safe_and_bounded(&info.name.display_name, turn_pty::MAX_TITLE_CHARS);
        assert_safe_and_bounded(&worker.title, turn_pty::MAX_TITLE_CHARS);
        assert_safe_and_bounded(
            info.agent_type.as_deref().unwrap(),
            turn_pty::MAX_TITLE_CHARS,
        );
        assert_safe_and_bounded(info.current_task.as_deref().unwrap(), MAX_AGENT_TASK_CHARS);

        let inspector = harness
            .core
            .tree_views(&session_id, NOW + 2)
            .into_iter()
            .find(|view| view.node_id == worker.id)
            .expect("the inspector projection contains the worker");
        assert_safe_and_bounded(&inspector.title, turn_pty::MAX_TITLE_CHARS);
        let inspector_agent = inspector.agent.unwrap();
        assert_safe_and_bounded(
            inspector_agent.name.declared_name.as_deref().unwrap(),
            turn_pty::MAX_TITLE_CHARS,
        );
        assert_safe_and_bounded(
            inspector_agent.current_task.as_deref().unwrap(),
            MAX_AGENT_TASK_CHARS,
        );

        let persisted = harness
            .core
            .store
            .events()
            .list_for_session(&session_id, 1)
            .unwrap()
            .pop()
            .expect("the normalised event is durable");
        let EventKind::AgentSpawned {
            declared_name,
            agent_type,
            task,
            ..
        } = persisted.kind
        else {
            panic!("unexpected event kind")
        };
        assert_safe_and_bounded(declared_name.as_deref().unwrap(), turn_pty::MAX_TITLE_CHARS);
        assert_safe_and_bounded(agent_type.as_deref().unwrap(), turn_pty::MAX_TITLE_CHARS);
        assert_safe_and_bounded(task.as_deref().unwrap(), MAX_AGENT_TASK_CHARS);

        let restored = harness
            .core
            .store
            .sessions()
            .get(&session_id)
            .unwrap()
            .unwrap();
        let restored_worker = restored.tree.get(&worker.id).unwrap();
        assert_safe_and_bounded(&restored_worker.title, turn_pty::MAX_TITLE_CHARS);
        assert_safe_and_bounded(
            restored_worker
                .agent
                .as_ref()
                .unwrap()
                .name
                .declared_name
                .as_deref()
                .unwrap(),
            turn_pty::MAX_TITLE_CHARS,
        );
    }

    #[tokio::test]
    async fn enormous_hostile_supervisor_argv_is_only_projected_as_bounded_safe_text() {
        let mut harness = Harness::new().await;
        let session_id = SessionId::from_stored("sess_hostile_process");
        harness.add_session(
            session_id.clone(),
            PaneId::from_stored("pane_hostile_process"),
            NOW,
        );
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

        let child_id = NodeId::from_stored("proc_hostile_discovered");
        let huge_argv = format!(
            "/opt/ru\u{202e}n\u{200b}ner\x1b[31m --payload={}\n--forged-row",
            "A".repeat(MAX_DISCOVERED_COMMAND_CHARS * 64)
        );
        let hostile_cwd = format!(
            "/tmp/work\u{200d}space\u{009b}2J/{}",
            "B".repeat(MAX_DISCOVERED_CWD_CHARS * 8)
        );
        let mut hostile_args = vec![
            "/opt/runner".to_string(),
            "--normal".to_string(),
            format!(
                "evil\n\x1b[31m\u{202e}\u{200b}\u{200d}{}",
                "C".repeat(MAX_DISCOVERED_ARG_CHARS * 8)
            ),
        ];
        hostile_args.extend((0..MAX_DISCOVERED_ARGS * 2).map(|index| format!("--extra-{index}")));
        harness.core.ingest(
            TurnEvent::new(
                session_id.clone(),
                EventKind::ProcessSpawnedChild {
                    child: child_id.clone(),
                    pid: 42_424,
                    ppid: Some(42_000),
                    command: huge_argv,
                    args: hostile_args,
                    cwd: Some(hostile_cwd),
                    confirmed_parent: false,
                },
                EventSource::Supervisor,
                Confidence::InferredHigh,
                NOW + 1,
            )
            .with_node(parent_id),
            NOW + 1,
        );

        let node = harness.core.sessions[&session_id]
            .tree
            .get(&child_id)
            .expect("the discovered process is represented");
        assert_safe_and_bounded(&node.command, MAX_DISCOVERED_COMMAND_CHARS);
        assert_safe_and_bounded(&node.cwd, MAX_DISCOVERED_CWD_CHARS);
        assert_safe_and_bounded(&node.title, turn_pty::MAX_TITLE_CHARS);
        assert!(node.args.len() <= MAX_DISCOVERED_ARGS);
        assert!(
            node.args
                .iter()
                .map(|arg| arg.chars().count())
                .sum::<usize>()
                <= MAX_DISCOVERED_ARGV_CHARS
        );
        for arg in &node.args {
            assert_safe_and_bounded(arg, MAX_DISCOVERED_ARG_CHARS);
        }

        let inspector = harness
            .core
            .tree_views(&session_id, NOW + 2)
            .into_iter()
            .find(|view| view.node_id == child_id)
            .expect("the inspector receives the process projection");
        assert_safe_and_bounded(&inspector.command, MAX_DISCOVERED_COMMAND_CHARS);
        assert_safe_and_bounded(&inspector.cwd, MAX_DISCOVERED_CWD_CHARS);
        assert_safe_and_bounded(&inspector.title, turn_pty::MAX_TITLE_CHARS);
        assert_eq!(inspector.args, node.args);

        let persisted_event = harness
            .core
            .store
            .events()
            .list_for_session(&session_id, 1)
            .unwrap()
            .pop()
            .expect("the discovery event is durable");
        let EventKind::ProcessSpawnedChild {
            command, args, cwd, ..
        } = persisted_event.kind
        else {
            panic!("unexpected event kind")
        };
        assert_safe_and_bounded(&command, MAX_DISCOVERED_COMMAND_CHARS);
        assert_safe_and_bounded(cwd.as_deref().unwrap(), MAX_DISCOVERED_CWD_CHARS);
        assert!(args.len() <= MAX_DISCOVERED_ARGS);
        assert!(
            args.iter().map(|arg| arg.chars().count()).sum::<usize>() <= MAX_DISCOVERED_ARGV_CHARS
        );
        for arg in &args {
            assert_safe_and_bounded(arg, MAX_DISCOVERED_ARG_CHARS);
        }

        let restored = harness
            .core
            .store
            .sessions()
            .get(&session_id)
            .unwrap()
            .unwrap();
        let restored_node = restored.tree.get(&child_id).unwrap();
        assert_safe_and_bounded(&restored_node.command, MAX_DISCOVERED_COMMAND_CHARS);
        assert_safe_and_bounded(&restored_node.cwd, MAX_DISCOVERED_CWD_CHARS);
        assert_safe_and_bounded(&restored_node.title, turn_pty::MAX_TITLE_CHARS);
        assert_eq!(restored_node.args, args);
    }

    #[tokio::test]
    async fn a_discovered_graphical_app_stays_under_its_parent_without_a_pane() {
        let mut harness = Harness::new().await;
        let session_id = SessionId::from_stored("sess_external_app");
        harness.add_session(
            session_id.clone(),
            PaneId::from_stored("pane_external_app"),
            NOW,
        );
        let mut parent = ProcessNode::agent(session_id.clone(), "gemini", "/tmp", NOW);
        parent.lifecycle = Lifecycle::Alive;
        parent.pid = Some(42_000);
        let parent_id = parent.id.clone();
        harness
            .core
            .sessions
            .get_mut(&session_id)
            .unwrap()
            .tree
            .insert(parent);

        let child_id = NodeId::from_stored("proc_external_godot");
        harness.core.ingest(
            TurnEvent::new(
                session_id.clone(),
                EventKind::ProcessSpawnedChild {
                    child: child_id.clone(),
                    pid: 42_001,
                    ppid: Some(42_000),
                    command: "/Applications/Godot.app/Godot --editor project.godot".into(),
                    args: vec!["--editor".into(), "project.godot".into()],
                    cwd: Some("/tmp/game".into()),
                    confirmed_parent: false,
                },
                EventSource::Supervisor,
                Confidence::InferredHigh,
                NOW + 1,
            )
            .with_node(parent_id.clone()),
            NOW + 1,
        );

        let child = harness.core.sessions[&session_id]
            .tree
            .get(&child_id)
            .unwrap();
        assert_eq!(child.kind, NodeKind::ExternalApp);
        assert_eq!(child.parent.as_ref(), Some(&parent_id));
        assert_eq!(child.relation, Relation::Inferred);
        assert!(
            harness.core.sessions[&session_id]
                .layout
                .panes()
                .iter()
                .all(|pane| pane.node_id.as_ref() != Some(&child_id)),
            "discovering desktop UI must not create or focus a Turn pane"
        );
        assert!(!harness.core.processes.contains_key(&child_id));
    }

    #[tokio::test]
    async fn an_authenticated_callback_promotes_inference_without_resetting_the_turn() {
        let mut harness = Harness::new().await;
        let session_id = SessionId::from_stored("sess_live_adapter_promotion");
        let pane_id = PaneId::from_stored("pane_live_adapter_promotion");
        harness.add_session(session_id.clone(), pane_id.clone(), NOW);
        let node_id = harness.spawn_process(&session_id, &pane_id, NOW).await;
        {
            let process = harness.core.processes.get_mut(&node_id).unwrap();
            process.level = IntegrationLevel::Heuristic;
            process.heuristic = Some(OutputHeuristic::new());
            process.adapter_id = "gemini-cli".into();
        }
        {
            let node = harness
                .core
                .sessions
                .get_mut(&session_id)
                .unwrap()
                .tree
                .get_mut(&node_id)
                .unwrap();
            node.kind = NodeKind::Agent;
            node.turn = Some(Turn::Active);
            node.agent = Some(Default::default());
            node.env_highlights
                .insert("TURN_INTEGRATION".into(), "inferred".into());
        }

        harness.core.ingest(
            TurnEvent::new(
                session_id.clone(),
                EventKind::AgentStarted {
                    tool: "gemini-cli".into(),
                    model: Some("gemini-2.5-pro".into()),
                    external_id: Some("gemini-session-1".into()),
                },
                EventSource::Hook {
                    tool: "gemini-cli".into(),
                    event_name: "BeforeModel".into(),
                },
                Confidence::Explicit,
                NOW + 1,
            )
            .with_node(node_id.clone()),
            NOW + 1,
        );

        let process = &harness.core.processes[&node_id];
        assert_eq!(process.level, IntegrationLevel::Structured);
        assert!(process.heuristic.is_none());
        let node = harness.core.sessions[&session_id]
            .tree
            .get(&node_id)
            .unwrap();
        assert_eq!(node.turn, Some(Turn::Active));
        assert_eq!(
            node.agent.as_ref().unwrap().agent.model.as_deref(),
            Some("gemini-2.5-pro")
        );
        assert_eq!(
            node.env_highlights
                .get("TURN_INTEGRATION")
                .map(String::as_str),
            Some("native")
        );
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

    #[tokio::test]
    async fn subagent_stop_clears_predeclaration_attention_in_memory_and_sqlite() {
        let mut harness = Harness::new().await;
        let session_id = SessionId::from_stored("sess_stop_out_of_order");
        harness.add_session(session_id.clone(), PaneId::new(), NOW);
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
        let reviewer_agent = AgentRef {
            provider: Some("anthropic".into()),
            tool: Some("claude-code".into()),
            model: None,
            external_id: Some("worker-reviewer".into()),
        };

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
            NOW + 1,
        )
        .with_parent(parent_id.clone())
        .with_agent(reviewer_agent.clone());
        harness.core.ingest(permission, NOW + 1);
        assert_eq!(harness.core.attention.queue().len(), 1);
        assert_eq!(harness.core.store.attention().list().unwrap().len(), 1);

        harness.core.insert_subagent(
            &session_id,
            &parent_id,
            Some("Reviewer".into()),
            Some("Explore".into()),
            Some("worker-reviewer".into()),
            None,
            NOW + 2,
        );
        let reviewer_id = harness.core.sessions[&session_id]
            .tree
            .find_by_external_id("worker-reviewer")
            .unwrap()
            .id
            .clone();
        let stopped = TurnEvent::new(
            session_id.clone(),
            EventKind::AgentSubagentStopped {
                agent_id: Some("worker-reviewer".into()),
            },
            EventSource::Hook {
                tool: "claude-code".into(),
                event_name: "SubagentStop".into(),
            },
            Confidence::Explicit,
            NOW + 3,
        )
        .with_parent(parent_id.clone())
        .with_agent(reviewer_agent);
        harness.core.ingest(stopped, NOW + 3);

        assert!(harness.core.attention.queue().is_empty());
        assert!(
            harness.core.store.attention().list().unwrap().is_empty(),
            "Cleared must reach SQLite or restore resurrects the stopped worker"
        );
        assert!(harness.core.sessions[&session_id]
            .tree
            .get(&reviewer_id)
            .unwrap()
            .lifecycle
            .is_terminal());
        let persisted_stop = harness
            .core
            .store
            .events()
            .list_for_session(&session_id, 1)
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(persisted_stop.node_id.as_ref(), Some(&reviewer_id));
        assert_eq!(persisted_stop.parent_node_id.as_ref(), Some(&parent_id));
        assert_eq!(
            persisted_stop.agent.external_id.as_deref(),
            Some("worker-reviewer")
        );
    }

    #[tokio::test]
    async fn stopping_a_parent_publishes_and_persists_nested_only_attention_cleanup() {
        let mut harness = Harness::new().await;
        let session_id = SessionId::from_stored("sess_nested_stop_attention");
        harness.add_session(session_id.clone(), PaneId::new(), NOW);
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
            Some("Coordinator".into()),
            None,
            Some("worker-coordinator".into()),
            None,
            NOW + 1,
        );
        let coordinator_id = harness.core.sessions[&session_id]
            .tree
            .find_by_external_id("worker-coordinator")
            .unwrap()
            .id
            .clone();
        harness.core.insert_subagent(
            &session_id,
            &coordinator_id,
            Some("Reviewer".into()),
            None,
            Some("worker-reviewer".into()),
            None,
            NOW + 2,
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
                summary: "Nested reviewer needs permission".into(),
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
        harness.core.ingest(permission, NOW + 3);
        assert_eq!(harness.core.attention.queue().len(), 1);
        assert_eq!(harness.core.store.attention().list().unwrap().len(), 1);

        let (_client, mut frames) = harness.add_client(64);
        let stopped = TurnEvent::new(
            session_id.clone(),
            EventKind::AgentSubagentStopped {
                agent_id: Some("worker-coordinator".into()),
            },
            EventSource::Hook {
                tool: "claude-code".into(),
                event_name: "SubagentStop".into(),
            },
            Confidence::Explicit,
            NOW + 4,
        )
        .with_parent(parent_id)
        .with_agent(AgentRef {
            external_id: Some("worker-coordinator".into()),
            ..AgentRef::default()
        });
        harness.core.ingest(stopped, NOW + 4);

        assert!(harness.core.attention.queue().is_empty());
        assert!(harness.core.store.attention().list().unwrap().is_empty());
        assert_eq!(
            harness.core.sessions[&session_id]
                .tree
                .get(&reviewer_id)
                .unwrap()
                .lifecycle,
            Lifecycle::Lost
        );
        let mut published_empty_queue = false;
        while let Ok(frame) = frames.try_recv() {
            if let turn_proto::ServerMessage::Event {
                event: turn_proto::ServerEvent::AttentionQueueChanged { entries },
            } = frame.message
            {
                published_empty_queue |= entries.is_empty();
            }
        }
        assert!(
            published_empty_queue,
            "the client must not retain nested attention after SQLite is cleared"
        );
    }

    #[tokio::test]
    async fn stop_before_start_materialises_a_terminal_tombstone_that_cannot_be_revived() {
        let mut harness = Harness::new().await;
        let session_id = SessionId::from_stored("sess_stop_before_start");
        harness.add_session(session_id.clone(), PaneId::new(), NOW);
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
        let worker = AgentRef {
            external_id: Some("worker-reviewer".into()),
            ..AgentRef::default()
        };

        let stopped = TurnEvent::new(
            session_id.clone(),
            EventKind::AgentSubagentStopped {
                agent_id: Some("worker-reviewer".into()),
            },
            EventSource::Hook {
                tool: "claude-code".into(),
                event_name: "SubagentStop".into(),
            },
            Confidence::Explicit,
            NOW + 1,
        )
        .with_parent(parent_id.clone())
        .with_agent(worker.clone());
        harness.core.ingest(stopped, NOW + 1);

        let tombstone_id = harness.core.sessions[&session_id]
            .tree
            .children(&parent_id)
            .into_iter()
            .next()
            .expect("Stop materialises the missing identity")
            .id
            .clone();
        let tombstone = harness.core.sessions[&session_id]
            .tree
            .get(&tombstone_id)
            .unwrap();
        assert_eq!(tombstone.kind, NodeKind::Subagent);
        assert!(tombstone.lifecycle.is_terminal());
        assert_eq!(tombstone.pid, None);
        assert_eq!(
            tombstone.agent.as_ref().unwrap().name.source,
            NameSource::Fallback
        );
        assert_eq!(
            tombstone.agent.as_ref().unwrap().name.confidence,
            Confidence::Unknown
        );
        let persisted_stop = harness
            .core
            .store
            .events()
            .list_for_session(&session_id, 1)
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(persisted_stop.node_id.as_ref(), Some(&tombstone_id));
        assert_eq!(persisted_stop.parent_node_id.as_ref(), Some(&parent_id));
        assert_eq!(
            persisted_stop.agent.external_id.as_deref(),
            Some("worker-reviewer")
        );

        let late_start = TurnEvent::new(
            session_id.clone(),
            EventKind::AgentSpawned {
                declared_name: Some("Reviewer".into()),
                agent_type: Some("Explore".into()),
                agent_id: Some("worker-reviewer".into()),
                task: Some("Review current diff".into()),
            },
            EventSource::Hook {
                tool: "claude-code".into(),
                event_name: "SubagentStart".into(),
            },
            Confidence::Explicit,
            NOW + 2,
        )
        .with_node(parent_id.clone())
        .with_agent(worker.clone());
        harness.core.ingest(late_start, NOW + 2);

        assert_eq!(
            harness.core.sessions[&session_id]
                .tree
                .children(&parent_id)
                .len(),
            1,
            "a repeated Start enriches the tombstone rather than creating a grandchild"
        );
        assert_eq!(
            harness.core.sessions[&session_id]
                .tree
                .get(&tombstone_id)
                .unwrap()
                .parent
                .as_ref(),
            Some(&parent_id)
        );

        let late_permission = TurnEvent::new(
            session_id.clone(),
            EventKind::AgentPermissionRequired {
                summary: "stale permission".into(),
                command: Some("rm stale".into()),
                tool_name: Some("Bash".into()),
                risk: Risk::High,
            },
            EventSource::Hook {
                tool: "claude-code".into(),
                event_name: "Notification".into(),
            },
            Confidence::Explicit,
            NOW + 3,
        )
        .with_parent(parent_id)
        .with_agent(worker);
        harness.core.ingest(late_permission, NOW + 3);

        let tombstone = harness.core.sessions[&session_id]
            .tree
            .get(&tombstone_id)
            .unwrap();
        assert!(tombstone.lifecycle.is_terminal());
        assert_eq!(
            tombstone.title, "Reviewer",
            "late Start may enrich metadata"
        );
        assert_eq!(tombstone.turn, Some(Turn::Done));
        assert!(!tombstone.interaction_pending);
        assert!(tombstone
            .agent
            .as_ref()
            .is_some_and(|agent| agent.pending_permission.is_none()));
        assert_eq!(
            tombstone.activity_preview, None,
            "a refused late state event cannot write an active preview"
        );
        assert!(harness.core.attention.queue().is_empty());

        let original_command = tombstone.command.clone();
        let late_process_start = TurnEvent::new(
            session_id.clone(),
            EventKind::ProcessStarted {
                pid: 4242,
                command: "stale-process".into(),
            },
            EventSource::Supervisor,
            Confidence::Explicit,
            NOW + 4,
        )
        .with_node(tombstone_id.clone());
        harness.core.ingest(late_process_start, NOW + 4);
        let tombstone = harness.core.sessions[&session_id]
            .tree
            .get(&tombstone_id)
            .unwrap();
        assert!(tombstone.lifecycle.is_terminal());
        assert_eq!(tombstone.pid, None);
        assert_eq!(tombstone.command, original_command);
        assert_eq!(tombstone.activity_preview, None);

        let persisted = harness
            .core
            .store
            .sessions()
            .get(&session_id)
            .unwrap()
            .unwrap();
        assert!(persisted
            .tree
            .get(&tombstone_id)
            .is_some_and(|node| node.lifecycle.is_terminal()));
    }

    #[tokio::test]
    async fn subagent_stop_cannot_cross_its_authenticated_parent_boundary() {
        let mut harness = Harness::new().await;
        let session_id = SessionId::from_stored("sess_stop_two_parents");
        harness.add_session(session_id.clone(), PaneId::new(), NOW);
        let mut parents = Vec::new();
        let mut children = Vec::new();
        for (offset, (parent_external, child_external)) in
            [("parent-a", "worker-a"), ("parent-b", "worker-b")]
                .into_iter()
                .enumerate()
        {
            let mut parent = ProcessNode::agent(session_id.clone(), "claude", "/tmp", NOW);
            parent.lifecycle = Lifecycle::Alive;
            parent.turn = Some(Turn::Active);
            let parent_id = parent.id.clone();
            if let Some(agent) = parent.agent.as_mut() {
                agent.external_id = Some(parent_external.into());
                agent.agent.external_id = Some(parent_external.into());
            }
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
                Some(format!("Worker {offset}")),
                Some("Explore".into()),
                Some(child_external.into()),
                None,
                NOW + offset as i64 + 1,
            );
            let child_id = harness.core.sessions[&session_id]
                .tree
                .find_by_external_id(child_external)
                .unwrap()
                .id
                .clone();
            parents.push(parent_id);
            children.push(child_id);
        }

        let demand = TurnEvent::new(
            session_id.clone(),
            EventKind::AgentPermissionRequired {
                summary: "Worker B needs permission".into(),
                command: None,
                tool_name: Some("Bash".into()),
                risk: Risk::Medium,
            },
            EventSource::Hook {
                tool: "claude-code".into(),
                event_name: "Notification".into(),
            },
            Confidence::Explicit,
            NOW + 10,
        )
        .with_node(children[1].clone());
        harness.core.ingest(demand, NOW + 10);

        let cross_parent_stop = TurnEvent::new(
            session_id.clone(),
            EventKind::AgentSubagentStopped {
                agent_id: Some("worker-b".into()),
            },
            EventSource::Hook {
                tool: "claude-code".into(),
                event_name: "SubagentStop".into(),
            },
            Confidence::Explicit,
            NOW + 11,
        )
        .with_parent(parents[0].clone())
        .with_agent(AgentRef {
            external_id: Some("worker-b".into()),
            ..AgentRef::default()
        });
        harness.core.ingest(cross_parent_stop, NOW + 11);

        assert!(harness.core.sessions[&session_id]
            .tree
            .get(&children[1])
            .unwrap()
            .is_running());
        assert_eq!(harness.core.attention.queue().len(), 1);
        assert_eq!(
            harness
                .core
                .attention
                .queue()
                .iter()
                .next()
                .unwrap()
                .node_id
                .as_ref(),
            Some(&children[1])
        );

        let invalid_parent_target = TurnEvent::new(
            session_id.clone(),
            EventKind::AgentSubagentStopped {
                agent_id: Some("parent-a".into()),
            },
            EventSource::Hook {
                tool: "claude-code".into(),
                event_name: "SubagentStop".into(),
            },
            Confidence::Explicit,
            NOW + 12,
        )
        .with_parent(parents[0].clone())
        .with_agent(AgentRef {
            external_id: Some("parent-a".into()),
            ..AgentRef::default()
        });
        harness.core.ingest(invalid_parent_target, NOW + 12);
        assert!(harness.core.sessions[&session_id]
            .tree
            .get(&parents[0])
            .unwrap()
            .is_running());
        assert_eq!(harness.core.attention.queue().len(), 1);
    }

    #[tokio::test]
    async fn idless_subagent_stop_is_recorded_as_inferred_not_explicit() {
        let mut harness = Harness::new().await;
        let session_id = SessionId::from_stored("sess_idless_stop");
        harness.add_session(session_id.clone(), PaneId::new(), NOW);
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
            Some("Reviewer".into()),
            Some("Explore".into()),
            Some("worker-reviewer".into()),
            None,
            NOW + 1,
        );

        let stopped = TurnEvent::new(
            session_id.clone(),
            EventKind::AgentSubagentStopped { agent_id: None },
            EventSource::Hook {
                tool: "claude-code".into(),
                event_name: "SubagentStop".into(),
            },
            Confidence::Explicit,
            NOW + 2,
        )
        .with_parent(parent_id);
        harness.core.ingest(stopped, NOW + 2);

        let persisted = harness
            .core
            .store
            .events()
            .list_for_session(&session_id, 1)
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(persisted.confidence, Confidence::InferredHigh);
        assert!(persisted.node_id.is_some());
    }

    #[tokio::test]
    async fn declared_subagent_process_exit_clears_its_predeclaration_scope() {
        let mut harness = Harness::new().await;
        let session_id = SessionId::from_stored("sess_worker_os_exit");
        harness.add_session(session_id.clone(), PaneId::new(), NOW);
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
        let worker = AgentRef {
            external_id: Some("worker-reviewer".into()),
            ..AgentRef::default()
        };
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
            NOW + 1,
        )
        .with_parent(parent_id.clone())
        .with_agent(worker);
        harness.core.ingest(permission, NOW + 1);
        harness.core.insert_subagent(
            &session_id,
            &parent_id,
            Some("Reviewer".into()),
            Some("Explore".into()),
            Some("worker-reviewer".into()),
            None,
            NOW + 2,
        );
        let reviewer_id = harness.core.sessions[&session_id]
            .tree
            .find_by_external_id("worker-reviewer")
            .unwrap()
            .id
            .clone();

        let exited = TurnEvent::new(
            session_id.clone(),
            EventKind::ProcessExited { code: 0 },
            EventSource::Supervisor,
            Confidence::Explicit,
            NOW + 3,
        )
        .with_node(reviewer_id.clone());
        harness.core.ingest(exited, NOW + 3);

        assert!(harness.core.attention.queue().is_empty());
        let persisted = harness
            .core
            .store
            .events()
            .list_for_session(&session_id, 1)
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(persisted.node_id.as_ref(), Some(&reviewer_id));
        assert_eq!(persisted.parent_node_id.as_ref(), Some(&parent_id));
        assert_eq!(
            persisted.agent.external_id.as_deref(),
            Some("worker-reviewer")
        );
    }
}
