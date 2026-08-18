//! Shared provider/account quota observation.
//!
//! A rolling account allowance is not owned by an Agent node. Every Codex node
//! using the daemon's current local login sees the same value, so `Core` runs at
//! most one bounded provider probe, caches its result, and fans a metadata event
//! out to matching nodes. Providers without a stable local read-only source are
//! marked unsupported instead of borrowing conversation-context numbers.

use std::path::PathBuf;
use std::time::Duration;

use turn_agents::{
    account_quota_source_for_tool, probe_codex_account_quota, AccountQuotaBucket,
    AccountQuotaObservation, AccountQuotaWindow, QuotaProbeError, MAX_ACCOUNT_QUOTA_PROBE_TIMEOUT,
    MIN_ACCOUNT_QUOTA_REFRESH_INTERVAL,
};
use turn_core::event::{AgentRef, Confidence, EventKind, EventSource, TurnEvent};
use turn_core::ids::{NodeId, SessionId};
use turn_core::model::{
    AgentRuntimeMetadata, Observable, ObservationSource, ObservationSourceKind, QuotaSnapshot,
    QuotaWindow, Session, UsageMeasurement, UsageMeasurementKind, UsageUnit,
};

use super::{Command, Core};

/// A successful observation stays fresh across one failed scheduling tick while
/// the next bounded probe is in flight. An explicit failure turns the last-known
/// value stale immediately.
const ACCOUNT_QUOTA_FRESHNESS: Duration = Duration::from_secs(2 * 60);
const CODEX_TOOL: &str = "codex";
const CODEX_SCOPE_ID: &str = "codex:current-local-login";
const CODEX_SOURCE_LABEL: &str = "codex account quota";
const CACHE_SOURCE_LABEL: &str = "codex account quota cache";

/// Safe failure classes crossing the detached-task/Core boundary.
///
/// Provider stderr and source `io::Error`s can contain paths or account data, so
/// neither is retained. The operator gets an actionable bounded category only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaProbeFailure {
    Unavailable,
    TimedOut,
    InvalidResponse,
    Failed,
}

impl QuotaProbeFailure {
    fn message(self) -> &'static str {
        match self {
            Self::Unavailable => "account quota provider is unavailable",
            Self::TimedOut => "account quota provider timed out",
            Self::InvalidResponse => "account quota provider returned no usable limits",
            Self::Failed => "account quota provider could not be read",
        }
    }
}

impl From<QuotaProbeError> for QuotaProbeFailure {
    fn from(error: QuotaProbeError) -> Self {
        match error {
            QuotaProbeError::Spawn(_) => Self::Unavailable,
            QuotaProbeError::TimedOut => Self::TimedOut,
            QuotaProbeError::Protocol(_)
            | QuotaProbeError::MessageTooLarge
            | QuotaProbeError::TooManyMessages => Self::InvalidResponse,
            QuotaProbeError::Io(_) => Self::Failed,
        }
    }
}

pub type QuotaProbeResult = Result<AccountQuotaObservation, QuotaProbeFailure>;

/// Scheduling and cache state for the current local Codex login.
///
/// There is deliberately one coordinator, rather than one entry on every Agent:
/// the provider protocol reports an account-level scope and the probe inherits
/// the daemon's single authenticated CLI environment.
#[derive(Debug, Default)]
pub(crate) struct AccountQuotaCoordinator {
    cached: Option<Observable<QuotaSnapshot>>,
    last_attempt_ms: Option<i64>,
    in_flight: bool,
}

impl AccountQuotaCoordinator {
    fn begin_refresh(&mut self, now_ms: i64) -> bool {
        if self.in_flight {
            return false;
        }
        if self
            .last_attempt_ms
            .is_some_and(|last| now_ms.saturating_sub(last) < refresh_interval_ms())
        {
            return false;
        }
        self.in_flight = true;
        self.last_attempt_ms = Some(now_ms);
        true
    }

    fn finish_refresh(
        &mut self,
        result: QuotaProbeResult,
        now_ms: i64,
    ) -> Option<Observable<QuotaSnapshot>> {
        if !self.in_flight {
            // A late completion from a cancelled/replaced task must not overwrite
            // the current cache.
            return None;
        }
        self.in_flight = false;

        let next = match result.and_then(quota_snapshot) {
            Ok(snapshot) => Observable::observed(
                snapshot,
                provider_source(),
                now_ms,
                now_ms.checked_add(freshness_ms()),
            ),
            Err(failure) => self.cached.take().and_then(stale_value).unwrap_or_else(|| {
                Observable::failed(provider_source(), now_ms, failure.message())
            }),
        };
        self.cached = Some(next.clone());
        Some(next)
    }

    fn seed_persisted(&mut self, candidate: Observable<QuotaSnapshot>) {
        if self.cached.is_some() {
            return;
        }
        self.cached = stale_value_with_source(candidate, cache_source());
    }

    fn cached_at(&mut self, now_ms: i64) -> Option<Observable<QuotaSnapshot>> {
        let expired = matches!(
            self.cached.as_ref(),
            Some(Observable::Observed {
                expires_at_ms: Some(expires_at_ms),
                ..
            }) if now_ms >= *expires_at_ms
        );
        if expired {
            self.cached = self.cached.take().and_then(stale_value);
        }
        self.cached.clone()
    }
}

#[derive(Clone)]
struct QuotaTarget {
    session_id: SessionId,
    node_id: NodeId,
    agent: AgentRef,
    current: Observable<QuotaSnapshot>,
    running: bool,
}

impl Core {
    /// Marks unsupported providers, fans out the shared cache, and schedules at
    /// most one refresh for all live Codex nodes.
    pub(crate) fn observe_account_quotas(&mut self, now_ms: i64) {
        self.mark_unsupported_account_quotas(now_ms);

        let codex = self.quota_targets(|tool| account_quota_source_for_tool(tool).is_some());
        if codex.is_empty() {
            return;
        }

        if self.account_quota.cached.is_none() {
            if let Some(persisted) = newest_usable_quota(&codex) {
                self.account_quota.seed_persisted(persisted);
            }
        }
        if let Some(cached) = self.account_quota.cached_at(now_ms) {
            self.fan_out_codex_quota(&codex, cached, now_ms);
        }

        if !codex.iter().any(|target| target.running) || !self.account_quota.begin_refresh(now_ms) {
            return;
        }

        let executable = self
            .registry
            .by_id(CODEX_TOOL)
            .and_then(|adapter| adapter.detect(CODEX_TOOL));
        let Some(executable) = executable else {
            self.account_quota_probe_finished(Err(QuotaProbeFailure::Unavailable), now_ms);
            return;
        };
        self.spawn_account_quota_probe(executable);
    }

    fn spawn_account_quota_probe(&mut self, executable: PathBuf) {
        debug_assert!(self.account_quota_probe.is_none());
        let commands = self.commands.clone();
        self.account_quota_probe = Some(tokio::spawn(async move {
            let result = probe_codex_account_quota(executable, MAX_ACCOUNT_QUOTA_PROBE_TIMEOUT)
                .await
                .map_err(QuotaProbeFailure::from);
            let _ = commands
                .send(Command::AccountQuotaProbeFinished { result })
                .await;
        }));
    }

    pub(crate) fn account_quota_probe_finished(&mut self, result: QuotaProbeResult, now_ms: i64) {
        // The task has enqueued this command and no longer owns provider state.
        // Dropping a completed JoinHandle is enough; shutdown aborts a live one.
        self.account_quota_probe.take();
        let Some(quota) = self.account_quota.finish_refresh(result, now_ms) else {
            return;
        };
        let targets = self.quota_targets(|tool| account_quota_source_for_tool(tool).is_some());
        self.fan_out_codex_quota(&targets, quota, now_ms);
    }

    fn mark_unsupported_account_quotas(&mut self, now_ms: i64) {
        let unsupported = self.quota_targets(|tool| account_quota_source_for_tool(tool).is_none());
        for target in unsupported {
            if !matches!(target.current, Observable::Waiting) {
                continue;
            }
            let quota = Observable::unsupported(
                ObservationSource::new(
                    ObservationSourceKind::Adapter,
                    "provider account quota capability",
                ),
                now_ms,
            );
            let event = quota_event(&target, quota, EventSource::Supervisor, now_ms);
            self.ingest(event, now_ms);
        }
    }

    fn fan_out_codex_quota(
        &mut self,
        targets: &[QuotaTarget],
        quota: Observable<QuotaSnapshot>,
        now_ms: i64,
    ) {
        let events = targets
            .iter()
            .filter(|target| target.current != quota)
            .map(|target| {
                quota_event(
                    target,
                    quota.clone(),
                    EventSource::SideChannel {
                        tool: CODEX_TOOL.into(),
                        channel: "account quota".into(),
                    },
                    now_ms,
                )
            })
            .collect::<Vec<_>>();
        for event in events {
            self.ingest(event, now_ms);
        }
    }

    fn quota_targets(&self, matches_tool: impl Fn(&str) -> bool) -> Vec<QuotaTarget> {
        self.sessions
            .values()
            .flat_map(|session| {
                session.tree.iter().filter_map(|node| {
                    let agent = node.agent.as_ref()?;
                    let effective_agent = effective_agent_ref(session, node)?;
                    let tool = effective_agent.tool.as_deref()?;
                    (node.kind.is_agentic() && matches_tool(tool)).then(|| QuotaTarget {
                        session_id: session.id.clone(),
                        node_id: node.id.clone(),
                        agent: effective_agent,
                        current: agent.runtime.quota.clone(),
                        running: node.is_running(),
                    })
                })
            })
            .collect()
    }
}

/// Subagent declarations do not repeat their parent's provider on every event.
/// Account quota is inherited from the nearest agentic runtime in that case; the
/// child's own model and external identity remain untouched.
fn effective_agent_ref(
    session: &Session,
    node: &turn_core::model::ProcessNode,
) -> Option<AgentRef> {
    let mut effective = node.agent.as_ref()?.agent.clone();
    let mut parent = node.parent.as_ref();
    // Links are cycle-checked on creation, but keep malformed restored data
    // bounded rather than following a corrupt parent chain forever.
    for _ in 0..64 {
        if effective.tool.is_some() && effective.provider.is_some() {
            break;
        }
        let Some(ancestor) = parent.and_then(|parent| session.tree.get(parent)) else {
            break;
        };
        if let Some(agent) = ancestor.agent.as_ref() {
            if effective.tool.is_none() {
                effective.tool = agent.agent.tool.clone();
            }
            if effective.provider.is_none() {
                effective.provider = agent.agent.provider.clone();
            }
        }
        parent = ancestor.parent.as_ref();
    }
    effective.tool.is_some().then_some(effective)
}

fn quota_event(
    target: &QuotaTarget,
    quota: Observable<QuotaSnapshot>,
    source: EventSource,
    now_ms: i64,
) -> TurnEvent {
    TurnEvent::new(
        target.session_id.clone(),
        EventKind::AgentRuntimeObserved {
            runtime: Box::new(AgentRuntimeMetadata {
                quota,
                ..AgentRuntimeMetadata::default()
            }),
        },
        source,
        Confidence::Explicit,
        now_ms,
    )
    .with_node(target.node_id.clone())
    .with_agent(target.agent.clone())
}

fn newest_usable_quota(targets: &[QuotaTarget]) -> Option<Observable<QuotaSnapshot>> {
    targets
        .iter()
        .filter_map(|target| match &target.current {
            Observable::Observed { .. } | Observable::Stale { .. } => Some(target.current.clone()),
            Observable::Waiting | Observable::Unsupported { .. } | Observable::Failed { .. } => {
                None
            }
        })
        .max_by_key(Observable::observed_at_ms)
}

fn stale_value(observation: Observable<QuotaSnapshot>) -> Option<Observable<QuotaSnapshot>> {
    let source = observation.source()?.clone();
    stale_value_with_source(observation, source)
}

fn stale_value_with_source(
    observation: Observable<QuotaSnapshot>,
    source: ObservationSource,
) -> Option<Observable<QuotaSnapshot>> {
    match observation {
        Observable::Observed {
            value,
            observed_at_ms,
            expires_at_ms,
            ..
        }
        | Observable::Stale {
            value,
            observed_at_ms,
            expires_at_ms,
            ..
        } => Some(Observable::stale(
            value,
            source,
            observed_at_ms,
            expires_at_ms,
        )),
        Observable::Waiting | Observable::Unsupported { .. } | Observable::Failed { .. } => None,
    }
}

fn quota_snapshot(
    observation: AccountQuotaObservation,
) -> Result<QuotaSnapshot, QuotaProbeFailure> {
    let mut windows = Vec::new();
    for (index, bucket) in observation.buckets.iter().enumerate() {
        let label = bucket_label(bucket, index);
        if let Some(window) = bucket.primary {
            windows.push(rolling_window(&label, "primary", window, bucket));
        }
        if let Some(window) = bucket.secondary {
            windows.push(rolling_window(&label, "secondary", window, bucket));
        }
        if let Some(spend) = &bucket.spend_control {
            windows.push(QuotaWindow {
                label: format!("{label} · spend"),
                measurement: remaining_percent(spend.remaining_percent),
                resets_at_ms: unix_seconds_to_ms(Some(spend.resets_at_unix)),
                exhausted: bucket.spend_control_reached,
                hard_limit: None,
            });
        }
    }
    if let Some(credits) = observation.reset_credits_available {
        windows.push(QuotaWindow {
            label: "Codex · reset credits".into(),
            measurement: UsageMeasurement {
                kind: UsageMeasurementKind::Remaining,
                amount: credits as f64,
                unit: UsageUnit::Requests,
                total: None,
            },
            resets_at_ms: None,
            exhausted: Some(credits == 0),
            hard_limit: None,
        });
    }
    if windows.is_empty() {
        return Err(QuotaProbeFailure::InvalidResponse);
    }

    let mut plans = observation
        .buckets
        .iter()
        .filter_map(|bucket| bucket.plan.clone())
        .collect::<Vec<_>>();
    plans.sort();
    plans.dedup();

    Ok(QuotaSnapshot {
        scope_id: Some(CODEX_SCOPE_ID.into()),
        scope_label: (!plans.is_empty()).then(|| plans.join(", ")),
        windows,
    })
}

fn rolling_window(
    bucket_label: &str,
    fallback: &str,
    window: AccountQuotaWindow,
    bucket: &AccountQuotaBucket,
) -> QuotaWindow {
    let window_label = window
        .window_minutes
        .map(duration_label)
        .unwrap_or_else(|| fallback.to_string());
    QuotaWindow {
        label: format!("{bucket_label} · {window_label}"),
        measurement: remaining_percent(window.remaining_percent()),
        resets_at_ms: unix_seconds_to_ms(window.resets_at_unix),
        exhausted: bucket.reached.as_ref().map(|_| true),
        hard_limit: None,
    }
}

fn remaining_percent(amount: u8) -> UsageMeasurement {
    UsageMeasurement {
        kind: UsageMeasurementKind::Remaining,
        amount: f64::from(amount),
        unit: UsageUnit::Percent,
        total: Some(100.0),
    }
}

fn bucket_label(bucket: &AccountQuotaBucket, index: usize) -> String {
    bucket
        .name
        .clone()
        .or_else(|| bucket.id.clone())
        .unwrap_or_else(|| format!("Quota {}", index + 1))
}

fn duration_label(minutes: u64) -> String {
    if minutes != 0 && minutes % (7 * 24 * 60) == 0 {
        format!("{}w", minutes / (7 * 24 * 60))
    } else if minutes != 0 && minutes % (24 * 60) == 0 {
        format!("{}d", minutes / (24 * 60))
    } else if minutes != 0 && minutes % 60 == 0 {
        format!("{}h", minutes / 60)
    } else {
        format!("{minutes}m")
    }
}

fn unix_seconds_to_ms(seconds: Option<u64>) -> Option<i64> {
    seconds
        .and_then(|seconds| i64::try_from(seconds).ok())
        .and_then(|seconds| seconds.checked_mul(1_000))
}

fn provider_source() -> ObservationSource {
    ObservationSource::new(ObservationSourceKind::Provider, CODEX_SOURCE_LABEL)
}

fn cache_source() -> ObservationSource {
    ObservationSource::new(ObservationSourceKind::Cache, CACHE_SOURCE_LABEL)
}

fn refresh_interval_ms() -> i64 {
    i64::try_from(MIN_ACCOUNT_QUOTA_REFRESH_INTERVAL.as_millis()).unwrap_or(i64::MAX)
}

fn freshness_ms() -> i64 {
    i64::try_from(ACCOUNT_QUOTA_FRESHNESS.as_millis()).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use std::io;

    use super::*;
    use crate::core::testing::Harness;
    use turn_core::ids::PaneId;
    use turn_core::model::{NodeKind, ProcessNode, Relation};
    use turn_core::state::Lifecycle;

    const T0: i64 = 1_700_000_000_000;

    fn observation() -> AccountQuotaObservation {
        AccountQuotaObservation {
            source: turn_agents::AccountQuotaSource::CodexAppServer,
            buckets: vec![AccountQuotaBucket {
                id: Some("codex".into()),
                name: Some("Codex".into()),
                plan: Some("plus".into()),
                primary: Some(AccountQuotaWindow {
                    used_percent: 37,
                    resets_at_unix: Some(1_800_000_000),
                    window_minutes: Some(300),
                }),
                secondary: Some(AccountQuotaWindow {
                    used_percent: 12,
                    resets_at_unix: Some(1_800_600_000),
                    window_minutes: Some(10_080),
                }),
                credits: None,
                spend_control: Some(turn_agents::AccountSpendControl {
                    limit: "100.00".into(),
                    used: "31.25".into(),
                    remaining_percent: 69,
                    resets_at_unix: 1_801_000_000,
                }),
                reached: None,
                spend_control_reached: Some(false),
            }],
            reset_credits_available: Some(2),
        }
    }

    fn agent(session_id: SessionId, tool: &str, now_ms: i64) -> ProcessNode {
        let mut node = ProcessNode::process(session_id, NodeKind::Agent, tool, "/tmp", now_ms);
        node.lifecycle = Lifecycle::Alive;
        node.agent = Some(Default::default());
        node.agent.as_mut().unwrap().agent = AgentRef {
            provider: Some(if tool == CODEX_TOOL {
                "openai".into()
            } else {
                "provider".into()
            }),
            tool: Some(tool.into()),
            model: None,
            external_id: None,
        };
        node
    }

    #[test]
    fn coordinator_allows_one_probe_and_waits_sixty_seconds_between_attempts() {
        let mut coordinator = AccountQuotaCoordinator::default();
        assert!(coordinator.begin_refresh(T0));
        assert!(!coordinator.begin_refresh(T0 + 1));
        assert!(coordinator
            .finish_refresh(Ok(observation()), T0 + 10)
            .is_some());
        assert!(!coordinator.begin_refresh(T0 + refresh_interval_ms() - 1));
        assert!(coordinator.begin_refresh(T0 + refresh_interval_ms()));
    }

    #[test]
    fn mapping_exposes_exact_remaining_percent_and_reset_times() {
        let snapshot = quota_snapshot(observation()).expect("usable provider limits");
        assert_eq!(snapshot.scope_id.as_deref(), Some(CODEX_SCOPE_ID));
        assert_eq!(snapshot.scope_label.as_deref(), Some("plus"));
        assert_eq!(snapshot.windows[0].label, "Codex · 5h");
        assert_eq!(
            snapshot.windows[0].measurement.kind,
            UsageMeasurementKind::Remaining
        );
        assert_eq!(snapshot.windows[0].measurement.amount, 63.0);
        assert_eq!(snapshot.windows[0].measurement.total, Some(100.0));
        assert_eq!(snapshot.windows[0].resets_at_ms, Some(1_800_000_000_000));
        assert_eq!(snapshot.windows[0].exhausted, None);
        assert_eq!(snapshot.windows[0].hard_limit, None);
        assert_eq!(snapshot.windows[1].label, "Codex · 1w");
        assert_eq!(snapshot.windows[1].measurement.amount, 88.0);
        assert_eq!(snapshot.windows[1].resets_at_ms, Some(1_800_600_000_000));
        assert_eq!(snapshot.windows[2].measurement.amount, 69.0);
        assert_eq!(snapshot.windows[2].resets_at_ms, Some(1_801_000_000_000));
        assert_eq!(snapshot.windows[3].measurement.unit, UsageUnit::Requests);
        assert_eq!(snapshot.windows[3].measurement.amount, 2.0);
    }

    #[test]
    fn provider_limit_reached_means_exhausted_with_unknown_hardness() {
        let mut observation = observation();
        observation.buckets[0].reached = Some("primary".into());
        let snapshot = quota_snapshot(observation).expect("usable provider limits");

        assert_eq!(snapshot.windows[0].exhausted, Some(true));
        assert_eq!(snapshot.windows[0].hard_limit, None);
        assert_eq!(snapshot.windows[1].hard_limit, None);
        assert!(snapshot
            .windows
            .iter()
            .all(|window| window.hard_limit.is_none()));
    }

    #[test]
    fn provider_errors_are_collapsed_before_the_core_boundary() {
        let secret = "sk-secret-at-/Users/operator/account.json";
        let failure = QuotaProbeFailure::from(QuotaProbeError::Spawn(io::Error::other(secret)));
        assert_eq!(failure, QuotaProbeFailure::Unavailable);
        assert!(!failure.message().contains(secret));
    }

    #[tokio::test]
    async fn one_shared_cache_fans_out_to_every_codex_agent_and_new_nodes() {
        let mut harness = Harness::new().await;
        let first_session = SessionId::from_stored("sess_quota_a");
        let second_session = SessionId::from_stored("sess_quota_b");
        harness.add_session(first_session.clone(), PaneId::from_stored("pane_a"), T0);
        harness.add_session(second_session.clone(), PaneId::from_stored("pane_b"), T0);

        let first = agent(first_session.clone(), CODEX_TOOL, T0);
        let first_id = first.id.clone();
        harness
            .core
            .sessions
            .get_mut(&first_session)
            .unwrap()
            .tree
            .insert(first);
        // Provider identity is declared once by the parent runtime. Subagents
        // still share that account quota without duplicating provider metadata
        // in every lifecycle payload.
        let mut child = ProcessNode::agent(first_session.clone(), CODEX_TOOL, "/tmp", T0);
        child.kind = NodeKind::Subagent;
        child.lifecycle = Lifecycle::Alive;
        child.link_to(first_id.clone(), Relation::Confirmed);
        let child_id = child.id.clone();
        harness
            .core
            .sessions
            .get_mut(&first_session)
            .unwrap()
            .tree
            .insert(child);
        let second = agent(second_session.clone(), CODEX_TOOL, T0);
        let second_id = second.id.clone();
        harness
            .core
            .sessions
            .get_mut(&second_session)
            .unwrap()
            .tree
            .insert(second);

        assert!(harness.core.account_quota.begin_refresh(T0));
        assert!(!harness.core.account_quota.begin_refresh(T0 + 1));
        harness
            .core
            .account_quota_probe_finished(Ok(observation()), T0 + 10);

        for (session_id, node_id) in [
            (&first_session, &first_id),
            (&first_session, &child_id),
            (&second_session, &second_id),
        ] {
            let quota = &harness.core.sessions[session_id]
                .tree
                .get(node_id)
                .unwrap()
                .agent
                .as_ref()
                .unwrap()
                .runtime
                .quota;
            assert_eq!(quota.value().unwrap().windows[0].measurement.amount, 63.0);
        }

        let third = agent(first_session.clone(), CODEX_TOOL, T0 + 20);
        let third_id = third.id.clone();
        harness
            .core
            .sessions
            .get_mut(&first_session)
            .unwrap()
            .tree
            .insert(third);
        let cached = harness.core.account_quota.cached_at(T0 + 20).unwrap();
        let targets = harness.core.quota_targets(|tool| tool == CODEX_TOOL);
        harness.core.fan_out_codex_quota(&targets, cached, T0 + 20);
        assert!(matches!(
            harness.core.sessions[&first_session]
                .tree
                .get(&third_id)
                .unwrap()
                .agent
                .as_ref()
                .unwrap()
                .runtime
                .quota,
            Observable::Observed { .. }
        ));
        assert!(!harness
            .core
            .account_quota
            .begin_refresh(T0 + refresh_interval_ms() - 1));
    }

    #[tokio::test]
    async fn failed_refresh_keeps_last_known_value_as_stale_and_other_providers_are_unsupported() {
        let mut harness = Harness::new().await;
        let session_id = SessionId::from_stored("sess_quota_states");
        harness.add_session(session_id.clone(), PaneId::from_stored("pane"), T0);
        let codex = agent(session_id.clone(), CODEX_TOOL, T0);
        let codex_id = codex.id.clone();
        let claude = agent(session_id.clone(), "claude-code", T0);
        let claude_id = claude.id.clone();
        let session = harness.core.sessions.get_mut(&session_id).unwrap();
        session.tree.insert(codex);
        session.tree.insert(claude);

        assert!(harness.core.account_quota.begin_refresh(T0));
        harness
            .core
            .account_quota_probe_finished(Ok(observation()), T0 + 1);
        assert!(harness
            .core
            .account_quota
            .begin_refresh(T0 + refresh_interval_ms()));
        harness.core.account_quota_probe_finished(
            Err(QuotaProbeFailure::Failed),
            T0 + refresh_interval_ms() + 1,
        );
        harness.core.mark_unsupported_account_quotas(T0 + 2);

        let codex_quota = &harness.core.sessions[&session_id]
            .tree
            .get(&codex_id)
            .unwrap()
            .agent
            .as_ref()
            .unwrap()
            .runtime
            .quota;
        assert!(matches!(codex_quota, Observable::Stale { .. }));
        assert_eq!(
            codex_quota.value().unwrap().windows[0].measurement.amount,
            63.0
        );
        assert!(matches!(
            harness.core.sessions[&session_id]
                .tree
                .get(&claude_id)
                .unwrap()
                .agent
                .as_ref()
                .unwrap()
                .runtime
                .quota,
            Observable::Unsupported { .. }
        ));
    }
}
