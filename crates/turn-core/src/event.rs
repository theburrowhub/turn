//! The common event vocabulary.
//!
//! Every adapter — a Claude Code hook, a Codex `notify` callback, a pty
//! heuristic, the process supervisor — funnels into [`TurnEvent`]. Downstream
//! consumers (state machine, attention manager, store) never learn which tool
//! produced an event, only how much to trust it.

use crate::ids::{EventId, NodeId, SessionId, WorkspaceId};
use crate::state::AwaitingReason;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

/// How much an event can be trusted.
///
/// This exists because Turn mixes reliable hook callbacks with pattern matching
/// on terminal output, and conflating them would be a lie the UI then repeats to
/// the user. Anything below `Integrated` is rendered as provisional.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    /// We could not determine anything.
    Unknown,
    /// A weak guess from output shape. Shown as provisional, never used to
    /// steal focus.
    InferredLow,
    /// A strong pattern match we have test fixtures for.
    InferredHigh,
    /// A wrapper or side channel we control reported it.
    Integrated,
    /// The tool itself told us, through a documented contract (a hook payload,
    /// a JSON-RPC notification, an exit status).
    Explicit,
}

impl Confidence {
    /// Whether this is solid enough to drive a focus change.
    ///
    /// Heuristics may badge and highlight, but they must never yank the user
    /// out of what they are doing — a false positive there is far more costly
    /// than a missed notification.
    pub fn may_steal_focus(&self) -> bool {
        matches!(self, Confidence::Integrated | Confidence::Explicit)
    }

    /// Whether the UI should mark this as a guess.
    pub fn is_provisional(&self) -> bool {
        matches!(
            self,
            Confidence::Unknown | Confidence::InferredLow | Confidence::InferredHigh
        )
    }

    pub fn label(&self) -> &'static str {
        match self {
            Confidence::Unknown => "unknown",
            Confidence::InferredLow => "inferred_low",
            Confidence::InferredHigh => "inferred_high",
            Confidence::Integrated => "integrated",
            Confidence::Explicit => "explicit",
        }
    }
}

/// Where an event came from. Kept for debugging and for the "a heuristic got it
/// wrong" correction flow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventSource {
    /// A hook callback from the agent's own hook engine.
    Hook { tool: String, event_name: String },
    /// A structured side channel (Codex `notify`, app-server JSON-RPC).
    SideChannel { tool: String, channel: String },
    /// Pattern matching over terminal output.
    PtyHeuristic { rule: String },
    /// Our own process supervisor.
    Supervisor,
    /// The user corrected a wrong state by hand.
    UserCorrection,
}

impl EventSource {
    /// The confidence this source can legitimately claim at most.
    ///
    /// Adapters ask for a confidence; this caps it, so a sloppy heuristic
    /// cannot declare itself explicit.
    pub fn max_confidence(&self) -> Confidence {
        match self {
            EventSource::Hook { .. } => Confidence::Explicit,
            EventSource::SideChannel { .. } => Confidence::Explicit,
            EventSource::Supervisor => Confidence::Explicit,
            EventSource::UserCorrection => Confidence::Explicit,
            EventSource::PtyHeuristic { .. } => Confidence::InferredHigh,
        }
    }
}

/// How loud an event is, before session policy is applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Debug,
    Info,
    Notice,
    Warning,
    Error,
}

/// What actually happened. One variant per event in the product brief.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum EventKind {
    #[serde(rename = "process.started")]
    ProcessStarted { pid: u32, command: String },
    #[serde(rename = "process.exited")]
    ProcessExited { code: i32 },
    #[serde(rename = "process.failed")]
    ProcessFailed {
        code: Option<i32>,
        signal: Option<i32>,
    },
    #[serde(rename = "process.spawned_child")]
    ProcessSpawnedChild {
        child: NodeId,
        pid: u32,
        #[serde(default)]
        ppid: Option<u32>,
        command: String,
        #[serde(default)]
        cwd: Option<String>,
        /// False when the parent link was inferred from the process table
        /// rather than reported to us.
        confirmed_parent: bool,
    },

    #[serde(rename = "agent.started")]
    AgentStarted {
        tool: String,
        model: Option<String>,
        /// The agent's own session/thread id, so Turn can resume it later.
        external_id: Option<String>,
    },
    #[serde(rename = "agent.turn_started")]
    AgentTurnStarted { prompt_excerpt: Option<String> },
    #[serde(rename = "agent.turn_completed")]
    AgentTurnCompleted {
        last_message: Option<String>,
        /// Work the agent left running when its turn ended.
        ///
        /// Claude Code reports this in its `Stop` payload, which turns the
        /// brief's Case E from something Turn has to infer into something it is
        /// told: the turn is over, but these are still going.
        #[serde(default)]
        background_tasks: usize,
    },
    #[serde(rename = "agent.waiting_for_user")]
    AgentWaitingForUser {
        reason: AwaitingReason,
        summary: Option<String>,
    },
    #[serde(rename = "agent.question_asked")]
    AgentQuestionAsked { question: String },
    #[serde(rename = "agent.permission_required")]
    AgentPermissionRequired {
        summary: String,
        command: Option<String>,
        tool_name: Option<String>,
        risk: Risk,
    },
    #[serde(rename = "agent.permission_resolved")]
    AgentPermissionResolved { allowed: bool },
    #[serde(rename = "agent.task_completed")]
    AgentTaskCompleted { summary: Option<String> },
    #[serde(rename = "agent.failed")]
    AgentFailed { reason: String },
    #[serde(rename = "agent.idle")]
    AgentIdle,
    /// A child agent was declared by its parent or an integrated runtime.
    ///
    /// `declared_name` is intentionally separate from `agent_type`: tools often
    /// report a generic type such as `default` or `Explore`, while the parent gave
    /// the worker a human name such as `Reviewer`. Losing that distinction makes
    /// restoration unable to reproduce the hierarchy the user saw.
    #[serde(rename = "agent.spawned")]
    AgentSpawned {
        declared_name: Option<String>,
        agent_type: Option<String>,
        agent_id: Option<String>,
        task: Option<String>,
    },
    #[serde(rename = "agent.subagent_stopped")]
    AgentSubagentStopped { agent_id: Option<String> },

    #[serde(rename = "session.needs_attention")]
    SessionNeedsAttention { reason: AwaitingReason },
    #[serde(rename = "session.attention_resolved")]
    SessionAttentionResolved,
}

/// Rough blast radius of a pending permission, used to rank and to colour the
/// approval banner. Assessed by the adapter, never by parsing intent out of
/// free-form agent prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Risk {
    Low,
    Medium,
    High,
}

/// Identity of the agent an event concerns.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRef {
    pub provider: Option<String>,
    pub tool: Option<String>,
    pub model: Option<String>,
    /// Tool-owned identity of the specific worker this event concerns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_id: Option<String>,
}

/// A normalised event, ready for the state machine and the store.
///
/// Deserialisation goes through [`TurnEvent::clamped`] rather than the derived
/// field-by-field path. The confidence cap is the load-bearing safety property of
/// the whole attention system — a heuristic must never be able to claim
/// `Explicit` and so reach the focus channel — and a cap enforced only in the
/// constructor is no cap at all once events travel over a socket or come back out
/// of SQLite. Both of those are paths Turn actually uses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "WireEvent")]
pub struct TurnEvent {
    pub id: EventId,
    /// Milliseconds since the Unix epoch. Chosen over a formatted timestamp so
    /// ordering is trivial in SQLite and in the UI.
    pub timestamp_ms: i64,
    pub workspace_id: Option<WorkspaceId>,
    pub session_id: SessionId,
    pub node_id: Option<NodeId>,
    /// The node this event's subject was spawned by, when known.
    pub parent_node_id: Option<NodeId>,
    pub agent: AgentRef,
    pub kind: EventKind,
    pub confidence: Confidence,
    pub source: EventSource,
    pub severity: Severity,
    /// Events sharing a key within the dedup window collapse into one. Keeps a
    /// chatty agent from producing forty identical "waiting for you" events.
    pub dedup_key: String,
    /// The untouched payload we derived this from, kept for debugging bad
    /// adapters. Never rendered as-is.
    pub raw: Option<String>,
}

/// The on-the-wire shape of a [`TurnEvent`], used only as a deserialisation stage.
///
/// Its one job is to force every inbound event through the confidence cap. Field
/// names and types mirror `TurnEvent` exactly, so the JSON representation is
/// unchanged; adding a field to one without the other is a compile error at the
/// `From` impl below, which is the intended tripwire.
#[derive(Deserialize)]
struct WireEvent {
    id: EventId,
    timestamp_ms: i64,
    workspace_id: Option<WorkspaceId>,
    session_id: SessionId,
    node_id: Option<NodeId>,
    parent_node_id: Option<NodeId>,
    agent: AgentRef,
    kind: EventKind,
    confidence: Confidence,
    source: EventSource,
    severity: Severity,
    dedup_key: String,
    raw: Option<String>,
}

impl From<WireEvent> for TurnEvent {
    fn from(wire: WireEvent) -> Self {
        // The same clamp `TurnEvent::new` applies. A stored or transmitted event
        // that claims more confidence than its source can support is downgraded
        // rather than rejected: dropping it would lose a real event, while
        // trusting it would let a heuristic reach the focus channel.
        let confidence = wire.confidence.min(wire.source.max_confidence());
        Self {
            id: wire.id,
            timestamp_ms: wire.timestamp_ms,
            workspace_id: wire.workspace_id,
            session_id: wire.session_id,
            node_id: wire.node_id,
            parent_node_id: wire.parent_node_id,
            agent: wire.agent,
            kind: wire.kind,
            confidence,
            source: wire.source,
            severity: wire.severity,
            dedup_key: wire.dedup_key,
            raw: wire.raw,
        }
    }
}

impl TurnEvent {
    /// Builds an event, clamping the requested confidence to what the source can
    /// honestly claim and deriving severity and dedup key from the kind.
    pub fn new(
        session_id: SessionId,
        kind: EventKind,
        source: EventSource,
        confidence: Confidence,
        timestamp_ms: i64,
    ) -> Self {
        let capped = confidence.min(source.max_confidence());
        let severity = default_severity(&kind);
        let dedup_key = default_dedup_key(&session_id, &kind);
        Self {
            id: EventId::new(),
            timestamp_ms,
            workspace_id: None,
            session_id,
            node_id: None,
            parent_node_id: None,
            agent: AgentRef::default(),
            kind,
            confidence: capped,
            source,
            severity,
            dedup_key,
            raw: None,
        }
    }

    pub fn with_node(mut self, node: NodeId) -> Self {
        // The dedup key is per-node for anything process-shaped, otherwise two
        // subagents waiting at once would collapse into a single event.
        self.dedup_key = format!("{}|{}", self.dedup_key, node);
        self.node_id = Some(node);
        self
    }

    pub fn with_parent(mut self, parent: NodeId) -> Self {
        self.parent_node_id = Some(parent);
        self
    }

    pub fn with_workspace(mut self, workspace: WorkspaceId) -> Self {
        self.workspace_id = Some(workspace);
        self
    }

    pub fn with_agent(mut self, agent: AgentRef) -> Self {
        self.agent = agent;
        self
    }

    pub fn with_raw(mut self, raw: impl Into<String>) -> Self {
        self.raw = Some(raw.into());
        self
    }

    /// The attention reason this event implies, if any.
    ///
    /// This is the single place where "an event happened" becomes "the user is
    /// needed", so the attention manager never has to pattern-match event kinds
    /// itself.
    pub fn attention_reason(&self) -> Option<AwaitingReason> {
        match &self.kind {
            EventKind::AgentPermissionRequired { .. } => Some(AwaitingReason::Permission),
            EventKind::AgentQuestionAsked { .. } => Some(AwaitingReason::Question),
            EventKind::AgentWaitingForUser { reason, .. } => Some(*reason),
            EventKind::SessionNeedsAttention { reason } => Some(*reason),
            _ => None,
        }
    }
}

fn default_severity(kind: &EventKind) -> Severity {
    match kind {
        EventKind::ProcessFailed { .. } | EventKind::AgentFailed { .. } => Severity::Error,
        EventKind::AgentPermissionRequired { .. } => Severity::Warning,
        EventKind::AgentQuestionAsked { .. }
        | EventKind::AgentWaitingForUser { .. }
        | EventKind::SessionNeedsAttention { .. } => Severity::Notice,
        EventKind::AgentTurnCompleted { .. } | EventKind::AgentTaskCompleted { .. } => {
            Severity::Notice
        }
        EventKind::ProcessStarted { .. }
        | EventKind::ProcessExited { .. }
        | EventKind::ProcessSpawnedChild { .. }
        | EventKind::AgentStarted { .. }
        | EventKind::AgentSpawned { .. }
        | EventKind::AgentSubagentStopped { .. }
        | EventKind::AgentPermissionResolved { .. }
        | EventKind::SessionAttentionResolved => Severity::Info,
        EventKind::AgentTurnStarted { .. } | EventKind::AgentIdle => Severity::Debug,
    }
}

/// A stable discriminant string per event kind, used for dedup and for storage.
fn kind_slug(kind: &EventKind) -> Cow<'static, str> {
    match kind {
        EventKind::ProcessStarted { .. } => "process.started".into(),
        EventKind::ProcessExited { .. } => "process.exited".into(),
        EventKind::ProcessFailed { .. } => "process.failed".into(),
        EventKind::ProcessSpawnedChild { .. } => "process.spawned_child".into(),
        EventKind::AgentStarted { .. } => "agent.started".into(),
        EventKind::AgentTurnStarted { .. } => "agent.turn_started".into(),
        EventKind::AgentTurnCompleted { .. } => "agent.turn_completed".into(),
        EventKind::AgentWaitingForUser { .. } => "agent.waiting_for_user".into(),
        EventKind::AgentQuestionAsked { .. } => "agent.question_asked".into(),
        EventKind::AgentPermissionRequired { .. } => "agent.permission_required".into(),
        EventKind::AgentPermissionResolved { .. } => "agent.permission_resolved".into(),
        EventKind::AgentTaskCompleted { .. } => "agent.task_completed".into(),
        EventKind::AgentFailed { .. } => "agent.failed".into(),
        EventKind::AgentIdle => "agent.idle".into(),
        EventKind::AgentSpawned { .. } => "agent.spawned".into(),
        EventKind::AgentSubagentStopped { .. } => "agent.subagent_stopped".into(),
        EventKind::SessionNeedsAttention { .. } => "session.needs_attention".into(),
        EventKind::SessionAttentionResolved => "session.attention_resolved".into(),
    }
}

fn default_dedup_key(session: &SessionId, kind: &EventKind) -> String {
    format!("{}|{}", session, kind_slug(kind))
}

/// The public name of an event kind, for logs and the event panel.
pub fn event_name(kind: &EventKind) -> String {
    kind_slug(kind).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sess() -> SessionId {
        SessionId::from_stored("sess_test00000001")
    }

    #[test]
    fn a_heuristic_cannot_promote_itself_to_explicit() {
        let event = TurnEvent::new(
            sess(),
            EventKind::AgentIdle,
            EventSource::PtyHeuristic {
                rule: "idle_prompt".into(),
            },
            Confidence::Explicit,
            0,
        );
        assert_eq!(event.confidence, Confidence::InferredHigh);
        assert!(!event.confidence.may_steal_focus());
        assert!(event.confidence.is_provisional());
    }

    #[test]
    fn hook_events_keep_explicit_confidence_and_may_take_focus() {
        let event = TurnEvent::new(
            sess(),
            EventKind::AgentTurnCompleted {
                last_message: None,
                background_tasks: 0,
            },
            EventSource::Hook {
                tool: "claude-code".into(),
                event_name: "Stop".into(),
            },
            Confidence::Explicit,
            1000,
        );
        assert_eq!(event.confidence, Confidence::Explicit);
        assert!(event.confidence.may_steal_focus());
    }

    #[test]
    fn identical_events_in_one_session_share_a_dedup_key() {
        let a = TurnEvent::new(
            sess(),
            EventKind::AgentIdle,
            EventSource::Supervisor,
            Confidence::Explicit,
            0,
        );
        let b = TurnEvent::new(
            sess(),
            EventKind::AgentIdle,
            EventSource::Supervisor,
            Confidence::Explicit,
            50,
        );
        assert_eq!(a.dedup_key, b.dedup_key);
        assert_ne!(a.id, b.id, "each occurrence still gets its own id");
    }

    #[test]
    fn two_subagents_waiting_at_once_do_not_collapse() {
        let make = |node: &str| {
            TurnEvent::new(
                sess(),
                EventKind::AgentWaitingForUser {
                    reason: AwaitingReason::Question,
                    summary: None,
                },
                EventSource::Hook {
                    tool: "claude-code".into(),
                    event_name: "Notification".into(),
                },
                Confidence::Explicit,
                0,
            )
            .with_node(NodeId::from_stored(node))
        };
        assert_ne!(make("proc_a").dedup_key, make("proc_b").dedup_key);
    }

    #[test]
    fn attention_reason_is_derived_from_the_event_not_guessed_by_consumers() {
        let permission = TurnEvent::new(
            sess(),
            EventKind::AgentPermissionRequired {
                summary: "run make verify".into(),
                command: Some("make verify".into()),
                tool_name: Some("Bash".into()),
                risk: Risk::Medium,
            },
            EventSource::Hook {
                tool: "claude-code".into(),
                event_name: "PermissionRequest".into(),
            },
            Confidence::Explicit,
            0,
        );
        assert_eq!(
            permission.attention_reason(),
            Some(AwaitingReason::Permission)
        );

        let turn_done = TurnEvent::new(
            sess(),
            EventKind::AgentTurnCompleted {
                last_message: None,
                background_tasks: 0,
            },
            EventSource::Supervisor,
            Confidence::Explicit,
            0,
        );
        // Finishing a turn is not, by itself, a demand for attention. Whether
        // it becomes one is the session policy's call.
        assert_eq!(turn_done.attention_reason(), None);
    }

    #[test]
    fn severity_is_derived_from_the_kind() {
        let failed = TurnEvent::new(
            sess(),
            EventKind::AgentFailed {
                reason: "boom".into(),
            },
            EventSource::Supervisor,
            Confidence::Explicit,
            0,
        );
        assert_eq!(failed.severity, Severity::Error);
    }

    /// The confidence cap is the safety property the whole attention system rests
    /// on, so it must survive the trip through JSON — which is how events reach
    /// the UI and come back out of SQLite. Enforcing it only in the constructor
    /// left a forged event able to claim the focus channel.
    ///
    /// The payload is built by serialising a real event and then tampering with
    /// the one field, so the test cannot drift out of sync with the wire format.
    #[test]
    fn a_deserialised_event_cannot_claim_more_confidence_than_its_source_allows() {
        let honest = TurnEvent::new(
            sess(),
            EventKind::AgentWaitingForUser {
                reason: AwaitingReason::Permission,
                summary: None,
            },
            EventSource::PtyHeuristic {
                rule: "permission_box".into(),
            },
            Confidence::InferredHigh,
            0,
        );
        let mut forged: serde_json::Value = serde_json::to_value(&honest).unwrap();
        // A heuristic claiming to be a fact.
        forged["confidence"] = serde_json::json!("explicit");

        let event: TurnEvent = serde_json::from_value(forged).expect("a decodable event");
        assert_eq!(
            event.confidence,
            Confidence::InferredHigh,
            "a forged payload must be downgraded, not trusted"
        );
        assert!(!event.confidence.may_steal_focus());
    }

    #[test]
    fn an_honest_event_keeps_its_confidence_through_a_round_trip() {
        let original = TurnEvent::new(
            sess(),
            EventKind::AgentIdle,
            EventSource::Hook {
                tool: "claude-code".into(),
                event_name: "SessionEnd".into(),
            },
            Confidence::Explicit,
            0,
        );
        let back: TurnEvent =
            serde_json::from_str(&serde_json::to_string(&original).unwrap()).unwrap();
        assert_eq!(back.confidence, Confidence::Explicit);
        assert_eq!(back, original);
    }

    #[test]
    fn events_round_trip_through_json_with_tagged_names() {
        let event = TurnEvent::new(
            sess(),
            EventKind::AgentPermissionRequired {
                summary: "run make verify".into(),
                command: Some("make verify".into()),
                tool_name: Some("Bash".into()),
                risk: Risk::Medium,
            },
            EventSource::Hook {
                tool: "claude-code".into(),
                event_name: "PermissionRequest".into(),
            },
            Confidence::Explicit,
            1_723_000_000_000,
        );
        let json = serde_json::to_string(&event).unwrap();
        assert!(
            json.contains("\"agent.permission_required\""),
            "wire name must match the documented vocabulary: {json}"
        );
        let back: TurnEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(event, back);
    }
}
