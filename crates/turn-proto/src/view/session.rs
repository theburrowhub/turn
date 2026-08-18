//! Session projections: what the sidebar needs, and what the details panel needs.

use serde::{Deserialize, Serialize};
use turn_core::attention::AttentionPolicy;
use turn_core::event::AgentRef;
use turn_core::ids::{CheckoutId, NodeId, SessionId, TemplateId, WorkspaceId};
use turn_core::model::{
    AgentName, AgentRuntimeMetadata, Layout, PendingPermission, ProcessNode, RestoreState, Session,
    SessionMode, SessionStatus,
};
use turn_core::state::{DisplayState, Turn};

use super::tree::TreeNodeView;

/// The agent detail a session row and a tree row both want.
///
/// `PartialEq` but not `Eq`: `cost_usd` is a float, exactly as in
/// [`AgentInfo`](turn_core::model::AgentInfo).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AgentSummary {
    pub node_id: NodeId,
    pub agent: AgentRef,
    /// Lossless naming metadata. A user rename changes `display_name` without
    /// destroying a name explicitly declared by the parent agent/integration.
    pub name: AgentName,
    /// The agent's own session/thread id, which is what a resume needs.
    pub external_id: Option<String>,
    /// Subagent type as the tool reported it ("Explore", "code-reviewer").
    pub agent_type: Option<String>,
    pub turn: Turn,
    pub current_task: Option<String>,
    pub last_message: Option<String>,
    /// A permission the user has not answered. Carried in full — including the
    /// `cwd` the command would run in — because approving something in the wrong
    /// repository is the mistake this field exists to prevent.
    pub pending_permission: Option<PendingPermission>,
    pub pending_question: Option<String>,
    pub tokens_used: Option<u64>,
    pub cost_usd: Option<f64>,
    pub permission_mode: Option<String>,
    /// Structured launch, context, and provider-quota observations. This is a
    /// safe inspector projection, not raw argv or provider output.
    #[serde(default)]
    pub runtime: AgentRuntimeMetadata,
    pub git_branch: Option<String>,
    /// Whether this agent can be resumed once its process ends. Drives whether
    /// the UI offers to bring it back — and it only ever *offers*.
    pub resumable: bool,
}

impl AgentSummary {
    /// Projects the agent detail of a node at `now_ms`, aging elapsed provider
    /// observations to stale, or returns `None` for a plain process.
    pub fn from_node(node: &ProcessNode, now_ms: i64) -> Option<Self> {
        let info = node.agent.as_ref()?;
        Some(Self {
            node_id: node.id.clone(),
            agent: info.agent.clone(),
            name: info.name.clone(),
            external_id: info.external_id.clone(),
            agent_type: info.agent_type.clone(),
            turn: node.turn.clone().unwrap_or(Turn::Unknown),
            current_task: info.current_task.clone(),
            last_message: info.last_message.clone(),
            pending_permission: info.pending_permission.clone(),
            pending_question: info.pending_question.clone(),
            tokens_used: info.tokens_used,
            cost_usd: info.cost_usd,
            permission_mode: info.permission_mode.clone(),
            runtime: info.runtime.clone().stale_if_expired(now_ms),
            git_branch: info.git_branch.clone(),
            resumable: info.resumable,
        })
    }
}

/// One row of the session sidebar, with every product rule already applied.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SessionSummary {
    pub id: SessionId,
    pub workspace_id: WorkspaceId,
    pub name: String,
    pub note: Option<String>,
    pub cwd: String,
    pub status: SessionStatus,

    /// Checkout safety is visible product state. A client must never infer write
    /// authority from the path or from whether a terminal happens to be focused.
    pub mode: SessionMode,
    pub checkout_id: CheckoutId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_path: Option<String>,
    pub read_only_enforced: bool,

    /// The flattened state, from [`DisplayState::derive`] over the session's
    /// process tree. The UI renders this; it never computes it.
    pub display_state: DisplayState,
    /// The terse sidebar label — `YOUR TURN`, `running`, `failed`.
    pub state_label: String,
    /// Ranking weight of `display_state`, so a client sorting locally sorts the
    /// same way the daemon would.
    pub severity: u8,
    /// Whether anything in this session is blocked on the human.
    pub needs_user: bool,

    /// Live subagents and processes, counted separately because "the agent
    /// finished its turn" and "nothing is running any more" are different claims.
    pub subagent_count: usize,
    pub running_count: usize,
    /// Of the running ones, how many survived the daemon that started them.
    ///
    /// A subset of `running_count`, and the part of it Turn cannot stop. It travels with
    /// the summary so that a confirmation dialog for any row in the tree can say what
    /// ending it will and will not achieve, without holding that Session's whole tree.
    /// Defaults to zero for an older peer's payload, which reads as "nothing escaped" —
    /// the same thing the field said before it existed.
    #[serde(default)]
    pub orphaned_count: usize,
    pub node_count: usize,
    pub pane_count: usize,

    /// Milliseconds since anything happened here.
    pub idle_ms: i64,
    pub last_activity_ms: i64,
    pub created_ms: i64,

    pub restore_state: RestoreState,
    /// Whether the restore state is one the user must be told about, rather than
    /// left to infer from a quiet pane.
    pub restore_needs_explanation: bool,

    /// Outstanding attention demands, for the sidebar badge.
    pub badge_count: usize,
    /// Whether this session is currently silenced.
    pub muted: bool,

    pub pinned: bool,
    pub favourite: bool,
    pub tags: Vec<String>,
    pub git_branch: Option<String>,
    pub linked_ref: Option<String>,
    pub template_id: Option<TemplateId>,
    pub parent_session: Option<SessionId>,
    pub tmux: bool,

    /// The session's headline agent, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_agent: Option<AgentSummary>,
}

impl SessionSummary {
    /// Projects a session.
    ///
    /// `badge_count` and `muted` come from the attention manager rather than the
    /// session, which is why they are parameters: the session does not know how
    /// many demands it has raised, and it should not have to.
    pub fn from_session(session: &Session, badge_count: usize, muted: bool, now_ms: i64) -> Self {
        let display_state = session.display_state();
        let session_needs_attention = display_state.demands_user() || badge_count > 0;
        Self {
            id: session.id.clone(),
            workspace_id: session.workspace_id.clone(),
            name: session.name.clone(),
            note: session.note.clone(),
            cwd: session.cwd.clone(),
            status: session.status,
            mode: session.mode,
            checkout_id: session.checkout_id.clone(),
            worktree_path: session.worktree_path.clone(),
            read_only_enforced: session.read_only_enforced,
            display_state,
            state_label: if session_needs_attention {
                "YOUR TURN".to_string()
            } else {
                display_state.label().to_string()
            },
            severity: display_state.severity(),
            needs_user: session.needs_user(),
            subagent_count: session.tree.subagent_count(),
            running_count: session.tree.running_count(),
            orphaned_count: session.tree.orphaned_count(),
            node_count: session.tree.len(),
            pane_count: session.layout.pane_count(),
            idle_ms: session.idle_for_ms(now_ms),
            last_activity_ms: session.last_activity_ms,
            created_ms: session.created_ms,
            restore_state: session.restore_state,
            restore_needs_explanation: session.restore_state.needs_explanation(),
            badge_count,
            muted,
            pinned: session.pinned,
            favourite: session.favourite,
            tags: session.tags.clone(),
            git_branch: session.git_branch.clone(),
            linked_ref: session.linked_ref.clone(),
            template_id: session.template_id.clone(),
            parent_session: session.parent_session.clone(),
            tmux: session.tmux,
            primary_agent: session
                .tree
                .primary_agent()
                .and_then(|node| AgentSummary::from_node(node, now_ms)),
        }
    }

    /// Sidebar ordering key, mirroring
    /// [`Session::sidebar_rank`](turn_core::model::Session::sidebar_rank).
    ///
    /// Provided so a client can re-sort a list it already holds — after a state
    /// change push, say — without a round trip and without inventing its own
    /// ordering. Higher sorts first.
    pub fn sidebar_rank(&self) -> (bool, bool, u8, i64) {
        (
            self.pinned,
            self.needs_user,
            self.severity,
            self.last_activity_ms,
        )
    }
}

/// Everything the session detail view and the terminal grid need.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SessionDetails {
    pub summary: SessionSummary,
    /// The pane tree, sent as the domain type: the UI's job is to lay it out, and
    /// re-describing it here would mean two definitions of a split.
    pub layout: Layout,
    /// The process tree in draw order.
    pub tree: Vec<TreeNodeView>,
    /// The policy in force, so the details panel can show and edit it.
    pub attention: AttentionPolicy,
    pub env: Vec<(String, String)>,
}

impl SessionDetails {
    pub fn from_session(session: &Session, badge_count: usize, muted: bool, now_ms: i64) -> Self {
        Self {
            summary: SessionSummary::from_session(session, badge_count, muted, now_ms),
            layout: session.layout.clone(),
            tree: TreeNodeView::for_session(session, now_ms),
            attention: session.attention.clone(),
            env: session.env.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use turn_core::event::Risk;
    use turn_core::ids::WorkspaceId;
    use turn_core::model::{Layout, NodeKind, Pane, PaneKind, Relation};
    use turn_core::state::{AwaitingReason, Lifecycle};

    const T0: i64 = 1_700_000_000_000;

    fn session() -> Session {
        Session::new(
            WorkspaceId::from_stored("ws_proto0001"),
            "Fix the flaky test",
            "/repo",
            Layout::single(Pane::new(PaneKind::Agent).with_command("claude")),
            T0,
        )
    }

    #[test]
    fn a_fresh_session_summarises_as_idle_and_asks_for_nothing() {
        let summary = SessionSummary::from_session(&session(), 0, false, T0);
        assert_eq!(summary.display_state, DisplayState::Idle);
        assert_eq!(summary.state_label, "idle");
        assert!(!summary.needs_user);
        assert_eq!(summary.badge_count, 0);
        assert_eq!(summary.pane_count, 1);
        assert_eq!(summary.node_count, 0);
        assert_eq!(summary.restore_state, RestoreState::Live);
        assert!(!summary.restore_needs_explanation);
    }

    /// The headline product rule, seen from the UI's side: the agent's turn ended
    /// but the tests it started are still going, and the summary says both.
    #[test]
    fn a_finished_turn_with_work_still_running_reports_both_facts() {
        let mut s = session();
        let mut agent = ProcessNode::agent(s.id.clone(), "claude", "/repo", T0);
        agent.lifecycle = Lifecycle::Alive;
        agent.turn = Some(Turn::Done);
        let agent_id = s.tree.insert(agent);

        let mut tests = ProcessNode::process(
            s.id.clone(),
            NodeKind::TestRunner,
            "cargo test",
            "/repo",
            T0,
        );
        tests.lifecycle = Lifecycle::Alive;
        tests.link_to(agent_id, Relation::Confirmed);
        s.tree.insert(tests);

        let summary = SessionSummary::from_session(&s, 1, false, T0 + 30_000);
        assert_eq!(summary.display_state, DisplayState::CompletedTurn);
        assert_eq!(
            summary.running_count, 2,
            "the UI must be able to say work is still running"
        );
        assert!(!summary.needs_user);
        assert_eq!(summary.idle_ms, 30_000);
    }

    #[test]
    fn a_blocked_permission_surfaces_the_command_and_the_directory() {
        let mut s = session();
        let mut agent = ProcessNode::agent(s.id.clone(), "claude", "/repo", T0);
        agent.lifecycle = Lifecycle::Alive;
        agent.turn = Some(Turn::AwaitingUser {
            reason: AwaitingReason::Permission,
        });
        agent.agent.as_mut().unwrap().pending_permission = Some(PendingPermission {
            summary: "run rm -rf build".into(),
            command: Some("rm -rf build".into()),
            tool_name: Some("Bash".into()),
            risk: Risk::High,
            requested_ms: T0,
            cwd: Some("/repo".into()),
        });
        s.tree.insert(agent);

        let summary = SessionSummary::from_session(&s, 1, false, T0);
        assert_eq!(summary.display_state, DisplayState::NeedsPermission);
        assert_eq!(summary.state_label, "YOUR TURN");
        assert!(summary.needs_user);

        let agent = summary.primary_agent.expect("the agent is the headline");
        let pending = agent.pending_permission.expect("the permission is carried");
        assert_eq!(pending.cwd.as_deref(), Some("/repo"));
        assert_eq!(pending.risk, Risk::High);
    }

    #[test]
    fn agent_summary_carries_runtime_observations_without_reinterpreting_them() {
        use turn_core::model::{
            ContextUsageSnapshot, Observable, ObservationSource, ObservationSourceKind,
            UsageMeasurement, UsageMeasurementKind, UsageUnit,
        };

        let mut node = ProcessNode::agent(session().id, "codex", "/repo", T0);
        node.agent.as_mut().unwrap().runtime.context = Observable::stale(
            ContextUsageSnapshot {
                scope_id: Some("thread-1".into()),
                measurement: UsageMeasurement {
                    kind: UsageMeasurementKind::Remaining,
                    amount: 81_000.0,
                    unit: UsageUnit::Tokens,
                    total: None,
                },
                effective_window: None,
                window_size_tokens: None,
                used_percentage: None,
                remaining_percentage: None,
                current_usage: None,
            },
            ObservationSource::new(ObservationSourceKind::Provider, "codex app server"),
            T0,
            Some(T0 + 1),
        );

        let summary = AgentSummary::from_node(&node, T0).unwrap();
        assert_eq!(summary.runtime, node.agent.unwrap().runtime);
        assert!(matches!(summary.runtime.context, Observable::Stale { .. }));
    }

    #[test]
    fn agent_summary_ages_provider_context_and_quota_at_now_ms() {
        use turn_core::model::{
            ContextUsageSnapshot, Observable, ObservationSource, ObservationSourceKind,
            QuotaSnapshot, UsageMeasurement, UsageMeasurementKind, UsageUnit,
        };

        let source = ObservationSource::new(ObservationSourceKind::Provider, "provider status");
        let mut node = ProcessNode::agent(session().id, "provider", "/repo", T0);
        let runtime = &mut node.agent.as_mut().unwrap().runtime;
        runtime.context = Observable::observed(
            ContextUsageSnapshot {
                scope_id: Some("conversation".into()),
                measurement: UsageMeasurement {
                    kind: UsageMeasurementKind::Used,
                    amount: 42.0,
                    unit: UsageUnit::Tokens,
                    total: None,
                },
                effective_window: None,
                window_size_tokens: None,
                used_percentage: None,
                remaining_percentage: None,
                current_usage: None,
            },
            source.clone(),
            T0,
            Some(T0 + 10),
        );
        runtime.quota = Observable::observed(
            QuotaSnapshot {
                scope_id: Some("account".into()),
                scope_label: None,
                windows: Vec::new(),
            },
            source,
            T0,
            Some(T0 + 10),
        );

        let before = AgentSummary::from_node(&node, T0 + 9).unwrap();
        assert!(matches!(
            before.runtime.context,
            Observable::Observed { .. }
        ));
        assert!(matches!(before.runtime.quota, Observable::Observed { .. }));

        let expired = AgentSummary::from_node(&node, T0 + 10).unwrap();
        assert!(matches!(expired.runtime.context, Observable::Stale { .. }));
        assert!(matches!(expired.runtime.quota, Observable::Stale { .. }));
    }

    #[test]
    fn subagents_are_counted_separately_from_running_processes() {
        let mut s = session();
        let root = s
            .tree
            .insert(ProcessNode::agent(s.id.clone(), "claude", "/", T0));
        for name in ["Explore", "code-reviewer"] {
            let mut sub = ProcessNode::agent(s.id.clone(), name, "/", T0);
            sub.kind = NodeKind::Subagent;
            sub.lifecycle = Lifecycle::Alive;
            sub.link_to(root.clone(), Relation::Confirmed);
            s.tree.insert(sub);
        }
        let summary = SessionSummary::from_session(&s, 0, false, T0);
        assert_eq!(summary.subagent_count, 2);
        assert_eq!(summary.node_count, 3);
        assert_eq!(summary.running_count, 3, "the root is still spawning");
    }

    #[test]
    fn a_partially_restored_session_says_it_needs_explaining() {
        let mut s = session();
        s.restore_state = RestoreState::PartiallyRestored;
        let summary = SessionSummary::from_session(&s, 0, false, T0);
        assert!(
            summary.restore_needs_explanation,
            "the user must never be left guessing whether their work survived"
        );
    }

    #[test]
    fn a_muted_session_still_reports_its_badge_count() {
        let summary = SessionSummary::from_session(&session(), 3, true, T0);
        assert!(summary.muted);
        assert_eq!(
            summary.badge_count, 3,
            "muting silences the interruption, not the evidence"
        );
    }

    #[test]
    fn the_client_side_sidebar_rank_matches_the_domains() {
        let mut blocked = session();
        let mut agent = ProcessNode::agent(blocked.id.clone(), "claude", "/", T0);
        agent.lifecycle = Lifecycle::Alive;
        agent.turn = Some(Turn::AwaitingUser {
            reason: AwaitingReason::Question,
        });
        blocked.tree.insert(agent);

        let mut pinned = session();
        pinned.pinned = true;

        for s in [&blocked, &pinned] {
            let summary = SessionSummary::from_session(s, 0, false, T0);
            assert_eq!(
                summary.sidebar_rank(),
                s.sidebar_rank(),
                "a client sorting locally must match the daemon"
            );
        }
    }

    #[test]
    fn details_carry_the_layout_and_the_tree_together() {
        let mut s = session();
        s.env = vec![("RUST_LOG".into(), "debug".into())];
        s.tree
            .insert(ProcessNode::agent(s.id.clone(), "claude", "/", T0));

        let details = SessionDetails::from_session(&s, 0, false, T0);
        assert_eq!(details.layout.pane_count(), 1);
        assert_eq!(details.tree.len(), 1);
        assert_eq!(details.attention, AttentionPolicy::default());
        assert_eq!(details.env.len(), 1);
    }

    #[test]
    fn a_session_summary_round_trips_with_explicit_wire_names() {
        let mut s = session();
        let mut agent = ProcessNode::agent(s.id.clone(), "claude", "/repo", T0);
        agent.lifecycle = Lifecycle::Alive;
        agent.turn = Some(Turn::AwaitingUser {
            reason: AwaitingReason::Credentials,
        });
        agent.agent.as_mut().unwrap().cost_usd = Some(0.42);
        s.tree.insert(agent);

        let summary = SessionSummary::from_session(&s, 2, false, T0);
        let json = serde_json::to_string(&summary).unwrap();
        assert!(
            json.contains("\"display_state\":\"waiting_for_user\""),
            "got {json}"
        );
        assert!(json.contains("\"state_label\":\"YOUR TURN\""));
        assert!(json.contains("\"badge_count\":2"));
        let back: SessionSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(back, summary);
    }

    #[test]
    fn details_round_trip_including_a_nested_layout() {
        let mut s = session();
        let first = s.layout.panes()[0].id.clone();
        s.layout.split(
            &first,
            turn_core::model::Direction::Horizontal,
            Pane::new(PaneKind::Shell).with_command("zsh"),
        );
        let details = SessionDetails::from_session(&s, 0, false, T0);
        let json = serde_json::to_string(&details).unwrap();
        let back: SessionDetails = serde_json::from_str(&json).unwrap();
        assert_eq!(back, details);
        assert_eq!(back.layout.pane_count(), 2);
    }
}
