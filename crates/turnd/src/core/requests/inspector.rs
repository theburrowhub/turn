//! On-demand contextual inspectors for the unified hierarchy.

use turn_core::event::{event_name, Confidence, EventKind, EventSource, TurnEvent};
use turn_core::ids::{NodeId, SessionId};
use turn_core::model::{ContextHandoffOutcome, ProcessNode};
use turn_core::state::Lifecycle;
use turn_proto::{
    HierarchyKey, InspectorDetails, InspectorEventView, InspectorHandoffView, InspectorOriginView,
    InspectorParentView, ProtoError, Response, SessionSummary, TreeNodeView, WorkspaceSummary,
};

use super::workspaces::store;
use super::Answer;
use crate::core::Core;

const SESSION_HISTORY_LIMIT: usize = 12;
const NODE_HISTORY_SCAN_LIMIT: usize = 80;
const NODE_HISTORY_LIMIT: usize = 8;
const HANDOFF_HISTORY_LIMIT: usize = 20;

impl Core {
    pub(super) fn get_inspector(&self, key: HierarchyKey, now_ms: i64) -> Answer {
        let details = match key {
            HierarchyKey::Workspace { workspace_id } => {
                let workspace = self
                    .workspaces
                    .get(&workspace_id)
                    .ok_or_else(|| ProtoError::not_found("workspace", workspace_id.as_str()))?;
                let safe = turn_store::redact::workspace_for_inspection(workspace);
                let summaries = self.session_summaries(Some(&workspace_id), true, now_ms);
                let mut environment_keys = safe
                    .env
                    .iter()
                    .map(|(key, _)| key.clone())
                    .collect::<Vec<_>>();
                environment_keys.sort();
                environment_keys.dedup();
                InspectorDetails::Workspace {
                    workspace: Box::new(WorkspaceSummary::from_workspace(&safe, &summaries)),
                    checkouts: self
                        .store
                        .hierarchy()
                        .checkouts_for_workspace(&workspace_id)
                        .map_err(store)?,
                    write_lease: self
                        .store
                        .hierarchy()
                        .active_lease(&workspace_id)
                        .map_err(store)?,
                    environment_keys,
                    init_commands: safe.init_commands,
                    attention: safe.attention,
                }
            }
            HierarchyKey::Session { session_id } => {
                let session = self.session(&session_id)?;
                let workspace = self.workspaces.get(&session.workspace_id).ok_or_else(|| {
                    ProtoError::not_found("workspace", session.workspace_id.as_str())
                })?;
                let safe = turn_store::redact::session_for_inspection(session);
                let mut environment_keys = safe
                    .env
                    .iter()
                    .map(|(key, _)| key.clone())
                    .collect::<Vec<_>>();
                environment_keys.sort();
                environment_keys.dedup();
                let history = self
                    .store
                    .events()
                    .list_for_session(&session_id, SESSION_HISTORY_LIMIT)
                    .map_err(store)?
                    .iter()
                    .map(|event| inspector_event(event, &safe))
                    .collect();
                let checkout = self
                    .store
                    .hierarchy()
                    .checkouts_for_workspace(&session.workspace_id)
                    .map_err(store)?
                    .into_iter()
                    .find(|checkout| checkout.id == safe.checkout_id);
                InspectorDetails::Session {
                    workspace_name: turn_store::redact::redact_secrets(&workspace.name),
                    session: Box::new(SessionSummary::from_session(
                        &safe,
                        self.attention
                            .queue()
                            .count_for_session(&session_id, now_ms),
                        self.attention.is_muted(&session_id, now_ms),
                        now_ms,
                    )),
                    checkout,
                    attention: safe.attention,
                    environment_keys,
                    history,
                }
            }
            HierarchyKey::Process { node_id } => self.process_inspector(&node_id, now_ms)?,
        };
        Ok(Response::Inspector {
            details: Box::new(details),
        })
    }

    fn process_inspector(
        &self,
        node_id: &NodeId,
        now_ms: i64,
    ) -> Result<InspectorDetails, ProtoError> {
        let (session, node) = self
            .sessions
            .values()
            .find_map(|session| session.tree.get(node_id).map(|node| (session, node)))
            .ok_or_else(|| ProtoError::not_found("process node", node_id.as_str()))?;
        let safe_session = turn_store::redact::session_for_inspection(session);
        let safe_node = safe_session
            .tree
            .get(node_id)
            .expect("redaction preserves Process identities");
        let bindings = self
            .store
            .hierarchy()
            .bindings_for_session(&session.id)
            .map_err(store)?;
        let capabilities = safe_session
            .tree
            .iter()
            .map(|candidate| {
                (
                    candidate.id.clone(),
                    self.node_pane_capability(&candidate.id),
                )
            })
            .collect();
        let row =
            TreeNodeView::for_session_with_panes(&safe_session, &bindings, &capabilities, now_ms)
                .into_iter()
                .find(|candidate| candidate.node_id == *node_id)
                .expect("the selected Process remains in its redacted Session");
        let parent = safe_node.parent.as_ref().and_then(|parent_id| {
            safe_session.tree.get(parent_id).map(|parent| {
                let (name, _) = parent.resolved_title();
                InspectorParentView {
                    key: HierarchyKey::process(parent_id.clone()),
                    name,
                    relationship: safe_node.relationship,
                    provisional: safe_node.relationship.confidence.is_provisional(),
                }
            })
        });
        let events = self
            .store
            .events()
            .list_for_session(&session.id, NODE_HISTORY_SCAN_LIMIT)
            .map_err(store)?;
        let history = events
            .iter()
            .filter(|event| event.node_id.as_ref() == Some(node_id))
            .take(NODE_HISTORY_LIMIT)
            .map(|event| inspector_event(event, &safe_session))
            .collect::<Vec<_>>();
        let origin = self.inspector_origin(node);
        let process_group = self.inspector_process_group(node);

        if row.is_agentic {
            let handoffs = self.inspector_handoffs(&session.id, node_id, &safe_session)?;
            Ok(InspectorDetails::Agent {
                session_name: safe_session.name,
                node: Box::new(row),
                parent,
                process_group,
                origin,
                history,
                handoffs,
            })
        } else {
            Ok(InspectorDetails::Process {
                session_name: safe_session.name,
                node: Box::new(row),
                parent,
                process_group,
                origin,
                history,
            })
        }
    }

    fn inspector_origin(&self, node: &ProcessNode) -> InspectorOriginView {
        if matches!(node.lifecycle, Lifecycle::Orphaned | Lifecycle::Lost) {
            return InspectorOriginView {
                label: "restored runtime record".into(),
                confidence: Confidence::Integrated,
            };
        }
        if self.processes.contains_key(&node.id) {
            return InspectorOriginView {
                label: "Turn-managed PTY".into(),
                confidence: Confidence::Integrated,
            };
        }
        if node.kind.is_agentic()
            && matches!(
                node.relationship.confidence,
                Confidence::Integrated | Confidence::Explicit
            )
        {
            return InspectorOriginView {
                label: "Agent integration".into(),
                confidence: node.relationship.confidence,
            };
        }
        if node.parent.is_some() && node.relationship.confidence.is_provisional() {
            return InspectorOriginView {
                label: "process supervisor".into(),
                confidence: node.relationship.confidence,
            };
        }
        InspectorOriginView {
            label: "unknown".into(),
            confidence: Confidence::Unknown,
        }
    }

    /// PID reuse makes an arbitrary `getpgid` unsafe even for a read: it could label
    /// a stranger as this Process. Query only a runtime this daemon still owns.
    fn inspector_process_group(&self, node: &ProcessNode) -> Option<u32> {
        if !self.processes.contains_key(&node.id) || !node.lifecycle.is_running() {
            return None;
        }
        process_group(node.pid?)
    }

    fn inspector_handoffs(
        &self,
        session_id: &SessionId,
        node_id: &NodeId,
        safe_session: &turn_core::model::Session,
    ) -> Result<Vec<InspectorHandoffView>, ProtoError> {
        let events = self
            .store
            .events()
            .list_of_kind(
                session_id,
                "context_handoff.finished",
                HANDOFF_HISTORY_LIMIT,
            )
            .map_err(store)?;
        Ok(events
            .into_iter()
            .filter_map(|event| {
                let EventKind::ContextHandoffFinished {
                    target_node_id,
                    mode,
                    outcome,
                    ..
                } = event.kind
                else {
                    return None;
                };
                let source_node_id = event.node_id?;
                let (direction, peer_node_id) = if &source_node_id == node_id {
                    ("sent", target_node_id)
                } else if &target_node_id == node_id {
                    ("received", source_node_id)
                } else {
                    return None;
                };
                let peer_name = safe_session
                    .tree
                    .get(&peer_node_id)
                    .map(|peer| peer.resolved_title().0)
                    .unwrap_or_else(|| "unknown Agent".into());
                Some(InspectorHandoffView {
                    timestamp_ms: event.timestamp_ms,
                    direction: direction.into(),
                    peer_node_id,
                    peer_name,
                    mode: mode.label().into(),
                    outcome: handoff_outcome_label(outcome).into(),
                })
            })
            .collect())
    }
}

fn inspector_event(event: &TurnEvent, session: &turn_core::model::Session) -> InspectorEventView {
    let subject = event.node_id.as_ref().and_then(|node_id| {
        session
            .tree
            .get(node_id)
            .map(|node| node.resolved_title().0)
    });
    InspectorEventView {
        timestamp_ms: event.timestamp_ms,
        name: event_name(&event.kind),
        subject,
        summary: event_summary(&event.kind).map(|text| turn_store::redact::redact_secrets(&text)),
        source: source_label(&event.source),
        confidence: event.confidence,
        severity: event.severity,
    }
}

fn event_summary(kind: &EventKind) -> Option<String> {
    match kind {
        EventKind::ProcessStarted { pid, command } => Some(format!("pid {pid} · {command}")),
        EventKind::ProcessExited { code } => Some(format!("exit {code}")),
        EventKind::ProcessFailed { code, signal } => {
            Some(format!("code {code:?} · signal {signal:?}"))
        }
        EventKind::ProcessSpawnedChild { pid, command, .. } => {
            Some(format!("pid {pid} · {command}"))
        }
        EventKind::AgentStarted { tool, model, .. } => Some(format!(
            "{tool}{}",
            model
                .as_ref()
                .map(|m| format!(" · {m}"))
                .unwrap_or_default()
        )),
        EventKind::AgentTurnCompleted { last_message, .. } => last_message.clone(),
        EventKind::AgentWaitingForUser { summary, .. } => summary.clone(),
        EventKind::AgentQuestionAsked { question } => Some(question.clone()),
        EventKind::AgentPermissionRequired { summary, .. } => Some(summary.clone()),
        EventKind::AgentPermissionResolved { allowed } => Some(if *allowed {
            "allowed".into()
        } else {
            "denied".into()
        }),
        EventKind::AgentTaskCompleted { summary } => summary.clone(),
        EventKind::AgentFailed { reason } => Some(reason.clone()),
        EventKind::AgentRuntimeObserved { runtime } => {
            if let Some(context) = runtime.context.value() {
                let used = context.measurement.amount;
                Some(context.measurement.total.map_or_else(
                    || format!("context · {used:.0} tokens used"),
                    |total| format!("context · {used:.0} / {total:.0} tokens"),
                ))
            } else if let Some(current) = runtime.launch.current.value() {
                current
                    .model
                    .as_ref()
                    .map(|model| format!("runtime model · {model}"))
                    .or_else(|| Some("runtime metadata refreshed".into()))
            } else {
                Some("runtime metadata refreshed".into())
            }
        }
        EventKind::AgentSpawned {
            declared_name,
            task,
            ..
        } => declared_name.clone().or_else(|| task.clone()),
        EventKind::AgentSubagentStopped { agent_id } => agent_id.clone(),
        EventKind::AgentRenamed { display_name, .. } => Some(display_name.clone()),
        EventKind::AgentRelationshipCorrected { .. } => Some("relationship corrected".into()),
        EventKind::ContextHandoffFinished { mode, outcome, .. } => Some(format!(
            "{} · {}",
            mode.label(),
            handoff_outcome_label(*outcome)
        )),
        EventKind::SessionNeedsAttention { reason } => Some(format!("{reason:?}")),
        EventKind::AgentTurnStarted { .. }
        | EventKind::AgentIdle
        | EventKind::SessionAttentionResolved => None,
    }
}

fn source_label(source: &EventSource) -> String {
    match source {
        EventSource::Hook { tool, event_name } => format!("{tool} hook · {event_name}"),
        EventSource::SideChannel { tool, channel } => format!("{tool} · {channel}"),
        EventSource::PtyHeuristic { rule } => format!("PTY heuristic · {rule}"),
        EventSource::Supervisor => "process supervisor".into(),
        EventSource::UserCorrection => "user correction".into(),
        EventSource::UserAction => "user action".into(),
    }
}

fn handoff_outcome_label(outcome: ContextHandoffOutcome) -> &'static str {
    match outcome {
        ContextHandoffOutcome::Submitted => "submitted",
        ContextHandoffOutcome::Uncertain => "uncertain",
    }
}

#[cfg(unix)]
fn process_group(pid: u32) -> Option<u32> {
    let pid = i32::try_from(pid).ok()?;
    // SAFETY: `getpgid` reads kernel metadata for the trusted, still-owned pid.
    let group = unsafe { libc::getpgid(pid) };
    (group >= 0).then_some(group as u32)
}

#[cfg(not(unix))]
fn process_group(_pid: u32) -> Option<u32> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::testing::Harness;
    use turn_core::event::{AgentRef, EventSource};
    use turn_core::ids::{HandoffId, PaneId};
    use turn_core::model::{
        AgentRuntimeMetadata, ContextHandoffMode, ContextUsageSnapshot, Observable,
        ObservationSource, ObservationSourceKind, PendingPermission, Relation, Relationship,
        RelationshipKind, UsageMeasurement, UsageMeasurementKind, UsageUnit,
    };
    use turn_core::state::Turn;

    const NOW: i64 = 1_700_000_000_000;
    const SECRET: &str = "ghp_abcdefghijklmnopqrstuvwxyz1234567890";

    #[test]
    fn runtime_observation_history_summarises_typed_usage_without_ingress_data() {
        let runtime = AgentRuntimeMetadata {
            context: Observable::observed(
                ContextUsageSnapshot {
                    scope_id: Some("conversation-42".into()),
                    measurement: UsageMeasurement {
                        kind: UsageMeasurementKind::Used,
                        amount: 42_000.0,
                        unit: UsageUnit::Tokens,
                        total: Some(200_000.0),
                    },
                    effective_window: None,
                },
                ObservationSource::new(ObservationSourceKind::Provider, "codex transcript"),
                NOW,
                None,
            ),
            ..AgentRuntimeMetadata::default()
        };

        assert_eq!(
            event_summary(&EventKind::AgentRuntimeObserved {
                runtime: Box::new(runtime),
            })
            .as_deref(),
            Some("context · 42000 / 200000 tokens")
        );
    }

    #[tokio::test]
    async fn every_inspector_kind_is_complete_redacted_and_honest() {
        let mut harness = Harness::new().await;
        let session_id = SessionId::from_stored("sess_inspector_acceptance");
        harness.add_session(session_id.clone(), PaneId::new(), NOW);
        let workspace_id = harness.core.sessions[&session_id].workspace_id.clone();
        {
            let workspace = harness.core.workspaces.get_mut(&workspace_id).unwrap();
            workspace.default_agent = Some("claude".into());
            workspace.default_shell = Some("zsh".into());
            workspace.env = vec![("GITHUB_TOKEN".into(), SECRET.into())];
            workspace.init_commands = vec![format!("tool --token {SECRET}")];
        }
        {
            let session = harness.core.sessions.get_mut(&session_id).unwrap();
            session.name = "Inspect the release".into();
            session.env = vec![("ANTHROPIC_API_KEY".into(), SECRET.into())];

            let mut parent = ProcessNode::agent(session_id.clone(), "claude", "/repo", NOW);
            parent.lifecycle = Lifecycle::Alive;
            parent.agent.as_mut().unwrap().name.display_name = "Claude".into();
            let parent_id = parent.id.clone();
            session.tree.insert(parent);

            let mut reviewer = ProcessNode::agent(session_id.clone(), "reviewer", "/repo", NOW + 1);
            reviewer.kind = turn_core::model::NodeKind::Subagent;
            reviewer.lifecycle = Lifecycle::Alive;
            reviewer.turn = Some(Turn::AwaitingUser {
                reason: turn_core::state::AwaitingReason::Permission,
            });
            reviewer.link_to(parent_id.clone(), Relation::Confirmed);
            reviewer.relationship = Relationship {
                kind: RelationshipKind::SpawnedBy,
                confidence: Confidence::Explicit,
            };
            let info = reviewer.agent.as_mut().unwrap();
            info.name.display_name = "Reviewer".into();
            info.name.declared_name = Some("Reviewer".into());
            info.agent = AgentRef {
                provider: Some("Anthropic".into()),
                tool: Some("Claude Code".into()),
                model: Some("Opus".into()),
                external_id: Some("reviewer-1".into()),
            };
            info.current_task = Some(format!("Review auth with {SECRET}"));
            info.last_message = Some(format!("Found credential {SECRET}"));
            info.tokens_used = Some(12_345);
            info.cost_usd = Some(0.42);
            info.pending_permission = Some(PendingPermission {
                summary: format!("Run deploy with {SECRET}"),
                command: Some(format!("deploy --token {SECRET}")),
                tool_name: Some("Bash".into()),
                risk: turn_core::event::Risk::High,
                requested_ms: NOW + 2,
                cwd: Some("/repo".into()),
            });
            let reviewer_id = reviewer.id.clone();
            session.tree.insert(reviewer);

            let mut observed = ProcessNode::process(
                session_id.clone(),
                turn_core::model::NodeKind::TestRunner,
                "cargo test",
                "/repo",
                NOW + 3,
            );
            observed.args = vec!["--format".into(), "json".into()];
            observed.pid = Some(7_654);
            observed.ppid = Some(4_321);
            observed.lifecycle = Lifecycle::Exited { code: 17 };
            observed.exit_code = Some(17);
            observed.ended_ms = Some(NOW + 8);
            observed.parent = Some(reviewer_id.clone());
            observed.relationship = Relationship {
                kind: RelationshipKind::SpawnedBy,
                confidence: Confidence::InferredHigh,
            };
            session.tree.insert(observed);
        }
        harness.core.persist_session(&session_id).unwrap();

        let reviewer_id = harness.core.sessions[&session_id]
            .tree
            .iter()
            .find(|node| node.resolved_title().0 == "Reviewer")
            .unwrap()
            .id
            .clone();
        let parent_id = harness.core.sessions[&session_id]
            .tree
            .get(&reviewer_id)
            .unwrap()
            .parent
            .clone()
            .unwrap();
        let process_id = harness.core.sessions[&session_id]
            .tree
            .iter()
            .find(|node| node.command == "cargo test")
            .unwrap()
            .id
            .clone();

        let started = TurnEvent::new(
            session_id.clone(),
            EventKind::AgentStarted {
                tool: "Claude Code".into(),
                model: Some("Opus".into()),
                external_id: Some("reviewer-1".into()),
            },
            EventSource::Hook {
                tool: "claude-code".into(),
                event_name: "SessionStart".into(),
            },
            Confidence::Explicit,
            NOW + 4,
        )
        .with_node(reviewer_id.clone());
        let handoff = TurnEvent::new(
            session_id.clone(),
            EventKind::ContextHandoffFinished {
                handoff_id: HandoffId::from_stored("handoff_inspector"),
                target_node_id: parent_id,
                mode: ContextHandoffMode::ReviewHandoff,
                outcome: ContextHandoffOutcome::Submitted,
            },
            EventSource::UserAction,
            Confidence::Explicit,
            NOW + 5,
        )
        .with_node(reviewer_id.clone());
        let process_started = TurnEvent::new(
            session_id.clone(),
            EventKind::ProcessStarted {
                pid: 7_654,
                command: "cargo test --format json".into(),
            },
            EventSource::Supervisor,
            Confidence::Explicit,
            NOW + 6,
        )
        .with_node(process_id.clone());
        let process_exited = TurnEvent::new(
            session_id.clone(),
            EventKind::ProcessExited { code: 17 },
            EventSource::Supervisor,
            Confidence::Explicit,
            NOW + 7,
        )
        .with_node(process_id.clone());
        harness
            .core
            .store
            .events()
            .append_all(&[started, handoff, process_started, process_exited])
            .unwrap();

        for key in [
            HierarchyKey::workspace(workspace_id),
            HierarchyKey::session(session_id.clone()),
            HierarchyKey::process(reviewer_id.clone()),
            HierarchyKey::process(process_id.clone()),
        ] {
            let Response::Inspector { details } =
                harness.core.get_inspector(key.clone(), NOW + 10).unwrap()
            else {
                panic!("expected inspector details")
            };
            assert_eq!(details.key(), key);
            let json = serde_json::to_string(&details).unwrap();
            assert!(!json.contains(SECRET), "secret leaked through {json}");
        }

        let Response::Inspector { details } = harness
            .core
            .get_inspector(HierarchyKey::process(reviewer_id), NOW + 10)
            .unwrap()
        else {
            unreachable!()
        };
        let InspectorDetails::Agent {
            node,
            parent,
            history,
            handoffs,
            ..
        } = *details
        else {
            panic!("the Reviewer must have an Agent inspector")
        };
        assert_eq!(node.agent.as_ref().unwrap().tokens_used, Some(12_345));
        assert_eq!(parent.unwrap().name, "Claude");
        assert!(!history.is_empty());
        assert_eq!(handoffs.len(), 1);

        let Response::Inspector { details } = harness
            .core
            .get_inspector(HierarchyKey::process(process_id), NOW + 10)
            .unwrap()
        else {
            unreachable!()
        };
        let InspectorDetails::Process {
            node,
            parent,
            process_group,
            origin,
            history,
            ..
        } = *details
        else {
            panic!("cargo test must have a Process inspector")
        };
        assert_eq!(node.pid, Some(7_654));
        assert_eq!(node.ppid, Some(4_321));
        assert_eq!(node.args, ["--format", "json"]);
        assert_eq!(node.exit_code, Some(17));
        assert_eq!(node.cwd, "/repo");
        assert_eq!(
            process_group, None,
            "no daemon-owned runtime means no guessed pgid"
        );
        assert_eq!(
            history.len(),
            2,
            "typed event-log rows stand in for unsafe raw output"
        );
        assert!(parent.unwrap().provisional);
        assert!(origin.confidence.is_provisional());
    }

    #[tokio::test]
    async fn handoff_history_follows_a_canonicalised_agent_identity() {
        let mut harness = Harness::new().await;
        let session_id = SessionId::from_stored("sess_inspector_identity_remap");
        harness.add_session(session_id.clone(), PaneId::new(), NOW);

        let (parent_id, survivor_id, retired_id) = {
            let session = harness.core.sessions.get_mut(&session_id).unwrap();
            let parent = ProcessNode::agent(session_id.clone(), "claude", "/repo", NOW);
            let parent_id = parent.id.clone();

            let mut survivor = ProcessNode::agent(session_id.clone(), "reviewer", "/repo", NOW + 1);
            survivor.kind = turn_core::model::NodeKind::Subagent;
            survivor.link_to(parent_id.clone(), Relation::Confirmed);
            let survivor_id = survivor.id.clone();

            let mut retired = ProcessNode::agent(session_id.clone(), "reviewer", "/repo", NOW + 2);
            retired.kind = turn_core::model::NodeKind::Subagent;
            retired.link_to(parent_id.clone(), Relation::Confirmed);
            let retired_id = retired.id.clone();

            session.tree.insert(parent);
            session.tree.insert(survivor);
            session.tree.insert(retired);
            (parent_id, survivor_id, retired_id)
        };
        harness.core.persist_session(&session_id).unwrap();

        let sent = TurnEvent::new(
            session_id.clone(),
            EventKind::ContextHandoffFinished {
                handoff_id: HandoffId::from_stored("handoff_identity_sent"),
                target_node_id: parent_id.clone(),
                mode: ContextHandoffMode::ReviewHandoff,
                outcome: ContextHandoffOutcome::Submitted,
            },
            EventSource::UserAction,
            Confidence::Explicit,
            NOW + 3,
        )
        .with_node(retired_id.clone());
        let received = TurnEvent::new(
            session_id.clone(),
            EventKind::ContextHandoffFinished {
                handoff_id: HandoffId::from_stored("handoff_identity_received"),
                target_node_id: retired_id.clone(),
                mode: ContextHandoffMode::SecondOpinion,
                outcome: ContextHandoffOutcome::Submitted,
            },
            EventSource::UserAction,
            Confidence::Explicit,
            NOW + 4,
        )
        .with_node(parent_id.clone());
        harness
            .core
            .store
            .events()
            .append_all(&[sent, received])
            .unwrap();

        harness
            .core
            .sessions
            .get_mut(&session_id)
            .unwrap()
            .tree
            .remove(&retired_id);
        let canonical = harness.core.sessions[&session_id].clone();
        harness
            .core
            .store
            .sessions()
            .save_after_node_remaps(&canonical, &[(retired_id.clone(), survivor_id.clone())])
            .unwrap();

        let Response::Inspector { details } = harness
            .core
            .get_inspector(HierarchyKey::process(survivor_id), NOW + 5)
            .unwrap()
        else {
            unreachable!()
        };
        let InspectorDetails::Agent { handoffs, .. } = *details else {
            panic!("the canonical Reviewer must retain an Agent inspector")
        };
        assert_eq!(handoffs.len(), 2);
        assert!(handoffs
            .iter()
            .any(|handoff| { handoff.direction == "sent" && handoff.peer_node_id == parent_id }));
        assert!(handoffs.iter().any(|handoff| {
            handoff.direction == "received" && handoff.peer_node_id == parent_id
        }));
    }
}
