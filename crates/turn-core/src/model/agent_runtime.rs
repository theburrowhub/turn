//! Observable runtime facts for one agent attempt.
//!
//! Provider CLIs expose different subsets of their launch configuration and
//! capacity data. These types keep absence honest: an adapter reports whether a
//! fact is still being collected, unsupported, stale, or failed instead of
//! manufacturing a value. Conversation context and provider/account quota are
//! deliberately separate observations because they have different scopes and
//! semantics.

use serde::{Deserialize, Serialize};

/// Where an observable runtime fact came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ObservationSource {
    /// The structured channel that supplied the fact.
    #[serde(default)]
    pub kind: ObservationSourceKind,
    /// A safe adapter/provider label, never a credential or raw command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl ObservationSource {
    pub fn new(kind: ObservationSourceKind, label: impl Into<String>) -> Self {
        Self {
            kind,
            label: Some(label.into()),
        }
    }
}

impl Default for ObservationSource {
    fn default() -> Self {
        Self {
            kind: ObservationSourceKind::Unknown,
            label: None,
        }
    }
}

/// The channel class behind an observation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationSourceKind {
    /// No structured source was identified.
    #[default]
    Unknown,
    /// The operator/template request before adapter normalisation.
    LaunchRequest,
    /// The adapter's normalised launch receipt.
    Adapter,
    /// A provider-owned API, event, status line, or runtime endpoint.
    Provider,
    /// A locally observed process/runtime fact.
    Process,
    /// A persisted last-known observation restored after restart.
    Cache,
}

/// A value whose availability and freshness remain explicit.
///
/// `Observed` and `Stale` always retain both provenance and observation time.
/// Unsupported and failed probes also say when and where that conclusion was
/// reached. `Waiting` is the serde/default state for peers and stored rows that
/// predate runtime telemetry.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Observable<T> {
    #[default]
    Waiting,
    Observed {
        value: T,
        source: ObservationSource,
        observed_at_ms: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expires_at_ms: Option<i64>,
    },
    Unsupported {
        source: ObservationSource,
        observed_at_ms: i64,
    },
    Stale {
        value: T,
        source: ObservationSource,
        observed_at_ms: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expires_at_ms: Option<i64>,
    },
    Failed {
        source: ObservationSource,
        observed_at_ms: i64,
        /// Safe, bounded diagnostic text. It must cross Turn's redaction
        /// boundary before persistence or display.
        message: String,
    },
}

impl<T> Observable<T> {
    pub fn observed(
        value: T,
        source: ObservationSource,
        observed_at_ms: i64,
        expires_at_ms: Option<i64>,
    ) -> Self {
        Self::Observed {
            value,
            source,
            observed_at_ms,
            expires_at_ms,
        }
    }

    pub fn stale(
        value: T,
        source: ObservationSource,
        observed_at_ms: i64,
        expires_at_ms: Option<i64>,
    ) -> Self {
        Self::Stale {
            value,
            source,
            observed_at_ms,
            expires_at_ms,
        }
    }

    pub fn unsupported(source: ObservationSource, observed_at_ms: i64) -> Self {
        Self::Unsupported {
            source,
            observed_at_ms,
        }
    }

    pub fn failed(
        source: ObservationSource,
        observed_at_ms: i64,
        message: impl Into<String>,
    ) -> Self {
        Self::Failed {
            source,
            observed_at_ms,
            message: message.into(),
        }
    }

    /// The usable last-known value. Stale values remain available so a surface
    /// can label them stale rather than replacing them with an invented zero.
    pub fn value(&self) -> Option<&T> {
        match self {
            Self::Observed { value, .. } | Self::Stale { value, .. } => Some(value),
            Self::Waiting | Self::Unsupported { .. } | Self::Failed { .. } => None,
        }
    }

    pub fn source(&self) -> Option<&ObservationSource> {
        match self {
            Self::Waiting => None,
            Self::Observed { source, .. }
            | Self::Unsupported { source, .. }
            | Self::Stale { source, .. }
            | Self::Failed { source, .. } => Some(source),
        }
    }

    pub fn observed_at_ms(&self) -> Option<i64> {
        match self {
            Self::Waiting => None,
            Self::Observed { observed_at_ms, .. }
            | Self::Unsupported { observed_at_ms, .. }
            | Self::Stale { observed_at_ms, .. }
            | Self::Failed { observed_at_ms, .. } => Some(*observed_at_ms),
        }
    }

    pub fn is_stale(&self) -> bool {
        matches!(self, Self::Stale { .. })
    }

    /// Whether this value must be presented as stale at `now_ms`.
    ///
    /// A live projection can remain in a client past its provider deadline, so
    /// callers must not rely on a coordinator having rewritten `Observed` to
    /// `Stale` first. The deadline is inclusive: at the instant it expires the
    /// observation is no longer current.
    pub fn is_stale_at(&self, now_ms: i64) -> bool {
        matches!(self, Self::Stale { .. })
            || matches!(
                self,
                Self::Observed {
                    expires_at_ms: Some(expires_at_ms),
                    ..
                } if now_ms >= *expires_at_ms
            )
    }

    /// Materialises an elapsed provider deadline without losing its last-known
    /// value, provenance, sample time, or original expiry receipt.
    pub fn stale_if_expired(self, now_ms: i64) -> Self {
        match self {
            Self::Observed {
                value,
                source,
                observed_at_ms,
                expires_at_ms: Some(expires_at_ms),
            } if now_ms >= expires_at_ms => Self::Stale {
                value,
                source,
                observed_at_ms,
                expires_at_ms: Some(expires_at_ms),
            },
            observation => observation,
        }
    }

    /// Maps the contained value while preserving its observation receipt.
    /// Useful at privacy boundaries that must redact every text leaf without
    /// weakening availability/freshness semantics.
    pub fn map<U>(self, map: impl FnOnce(T) -> U) -> Observable<U> {
        match self {
            Self::Waiting => Observable::Waiting,
            Self::Observed {
                value,
                source,
                observed_at_ms,
                expires_at_ms,
            } => Observable::Observed {
                value: map(value),
                source,
                observed_at_ms,
                expires_at_ms,
            },
            Self::Unsupported {
                source,
                observed_at_ms,
            } => Observable::Unsupported {
                source,
                observed_at_ms,
            },
            Self::Stale {
                value,
                source,
                observed_at_ms,
                expires_at_ms,
            } => Observable::Stale {
                value: map(value),
                source,
                observed_at_ms,
                expires_at_ms,
            },
            Self::Failed {
                source,
                observed_at_ms,
                message,
            } => Observable::Failed {
                source,
                observed_at_ms,
                message,
            },
        }
    }

    /// Selects the newer conclusion without confusing `Waiting` with a newer
    /// negative observation. Ties prefer `other`, which is useful when a live
    /// projection supersedes a restored value at the same timestamp.
    pub fn prefer_newer(self, other: Self) -> Self {
        match (self.observed_at_ms(), other.observed_at_ms()) {
            (None, None) => other,
            (Some(_), None) => self,
            (None, Some(_)) => other,
            (Some(left), Some(right)) if left > right => self,
            (Some(_), Some(_)) => other,
        }
    }

    fn value_mut(&mut self) -> Option<&mut T> {
        match self {
            Self::Observed { value, .. } | Self::Stale { value, .. } => Some(value),
            Self::Waiting | Self::Unsupported { .. } | Self::Failed { .. } => None,
        }
    }
}

/// Requested/effective/current launch facts safe to expose to the operator.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchConfiguration {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Provider-owned human label for the current model, when distinct from its id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_mode: Option<String>,
    /// Live provider reasoning effort. Kept separate from permission policy and
    /// safe flags because it may change while an agent is already running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort_level: Option<String>,
    /// Whether the provider reports extended thinking enabled for this attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_enabled: Option<bool>,
    /// Flag names or already-sanitised non-secret values. Raw argv does not
    /// belong here.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub safe_flags: Vec<String>,
}

impl LaunchConfiguration {
    /// Retains independently observed fields that a newer partial provider
    /// sample did not carry. Provider status lines and transcript tails expose
    /// different subsets of the current configuration; treating either subset
    /// as a replacement would erase valid facts merely because its task
    /// completed later.
    fn fill_missing_from(&mut self, fallback: &Self) {
        if self.model.is_none() {
            self.model.clone_from(&fallback.model);
        }
        if self.model_display_name.is_none() {
            self.model_display_name
                .clone_from(&fallback.model_display_name);
        }
        if self.permission_mode.is_none() {
            self.permission_mode.clone_from(&fallback.permission_mode);
        }
        if self.approval_mode.is_none() {
            self.approval_mode.clone_from(&fallback.approval_mode);
        }
        if self.sandbox_mode.is_none() {
            self.sandbox_mode.clone_from(&fallback.sandbox_mode);
        }
        if self.effort_level.is_none() {
            self.effort_level.clone_from(&fallback.effort_level);
        }
        if self.thinking_enabled.is_none() {
            self.thinking_enabled = fallback.thinking_enabled;
        }
        if self.safe_flags.is_empty() {
            self.safe_flags.clone_from(&fallback.safe_flags);
        }
    }
}

/// The launch request, adapter receipt, and currently observed configuration.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentLaunchFacts {
    #[serde(default)]
    pub requested: Observable<LaunchConfiguration>,
    #[serde(default)]
    pub effective: Observable<LaunchConfiguration>,
    #[serde(default)]
    pub current: Observable<LaunchConfiguration>,
}

impl AgentLaunchFacts {
    pub fn prefer_newer(self, other: Self) -> Self {
        Self {
            requested: prefer_newer_configuration(self.requested, other.requested),
            effective: prefer_newer_configuration(self.effective, other.effective),
            current: prefer_newer_configuration(self.current, other.current),
        }
    }

    fn stale_if_expired(self, now_ms: i64) -> Self {
        Self {
            requested: self.requested.stale_if_expired(now_ms),
            effective: self.effective.stale_if_expired(now_ms),
            current: self.current.stale_if_expired(now_ms),
        }
    }
}

fn prefer_newer_configuration(
    left: Observable<LaunchConfiguration>,
    right: Observable<LaunchConfiguration>,
) -> Observable<LaunchConfiguration> {
    let right_is_preferred = match (left.observed_at_ms(), right.observed_at_ms()) {
        (Some(left), Some(right)) => right >= left,
        (None, Some(_)) | (None, None) => true,
        (Some(_), None) => false,
    };
    let (mut preferred, fallback) = if right_is_preferred {
        (right, left)
    } else {
        (left, right)
    };
    if let (Some(preferred_value), Some(fallback_value)) = (preferred.value_mut(), fallback.value())
    {
        preferred_value.fill_missing_from(fallback_value);
    }
    preferred
}

/// Whether a measurement is consumption, remaining capacity, or a provider's
/// own percentage. Turn does not silently convert one into another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageMeasurementKind {
    Used,
    Remaining,
    ProviderPercent,
}

/// Unit of an exact usage quantity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageUnit {
    Tokens,
    Percent,
    Requests,
    Credits,
    Other(String),
}

/// One provider-reported quantity with its original semantics intact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageMeasurement {
    pub kind: UsageMeasurementKind,
    pub amount: f64,
    pub unit: UsageUnit,
    /// Exact compatible denominator reported in the same scope, when known.
    /// Its absence forbids Turn from inventing a complement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<f64>,
}

/// Conversation-context consumption for one provider conversation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextUsageSnapshot {
    /// Stable provider conversation/context scope when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_id: Option<String>,
    pub measurement: UsageMeasurement,
    /// Provider-reported effective window after system/tool reservations, when
    /// distinct from the measurement's total.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_window: Option<UsageMeasurement>,
    /// Provider-reported context-window size without converting it through a
    /// floating-point measurement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_size_tokens: Option<u64>,
    /// Provider-owned percentages, retained independently from exact token
    /// counts so Turn never invents one from the other.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used_percentage: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remaining_percentage: Option<f64>,
    /// Token counters from the provider's most recent API call. This is not the
    /// same scope as the live context total and must not be added to it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_usage: Option<ContextTokenUsage>,
}

/// Provider token counters for the most recent API call.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextTokenUsage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<u64>,
}

/// One account/provider quota window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuotaWindow {
    pub label: String,
    pub measurement: UsageMeasurement,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at_ms: Option<i64>,
    /// Whether the provider explicitly reports this allowance exhausted.
    /// This is independent of enforcement hardness: "limit reached" proves
    /// exhaustion, but does not prove whether the boundary is hard or soft.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exhausted: Option<bool>,
    /// Whether the provider schema explicitly classifies the boundary as hard
    /// (`true`) or soft (`false`). `None` means hardness is unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hard_limit: Option<bool>,
}

/// Provider/account quota, intentionally not conversation-context usage.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuotaSnapshot {
    /// Stable shared scope id. Several agent nodes may reference the same scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_id: Option<String>,
    /// Safe account/profile/organisation label for the operator.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_label: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub windows: Vec<QuotaWindow>,
}

/// All observable runtime/capacity facts attached to an agent node.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentRuntimeMetadata {
    #[serde(default)]
    pub launch: AgentLaunchFacts,
    #[serde(default)]
    pub context: Observable<ContextUsageSnapshot>,
    #[serde(default)]
    pub quota: Observable<QuotaSnapshot>,
}

impl AgentRuntimeMetadata {
    /// Reconciles two projections of the same attempt by observation time.
    pub fn prefer_newer(self, other: Self) -> Self {
        Self {
            launch: self.launch.prefer_newer(other.launch),
            context: self.context.prefer_newer(other.context),
            quota: self.quota.prefer_newer(other.quota),
        }
    }

    /// Materialises every elapsed observation deadline for restoration and
    /// point-in-time projections. Live views must additionally consult
    /// [`Observable::is_stale_at`] because a received projection can itself age.
    pub fn stale_if_expired(self, now_ms: i64) -> Self {
        Self {
            launch: self.launch.stale_if_expired(now_ms),
            context: self.context.stale_if_expired(now_ms),
            quota: self.quota.stale_if_expired(now_ms),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: i64 = 1_700_000_000_000;

    fn provider() -> ObservationSource {
        ObservationSource::new(ObservationSourceKind::Provider, "provider status")
    }

    #[test]
    fn old_agent_runtime_payload_defaults_every_fact_to_waiting() {
        let runtime: AgentRuntimeMetadata = serde_json::from_str("{}").unwrap();
        assert_eq!(runtime, AgentRuntimeMetadata::default());
        assert!(matches!(runtime.launch.requested, Observable::Waiting));
        assert!(matches!(runtime.launch.effective, Observable::Waiting));
        assert!(matches!(runtime.launch.current, Observable::Waiting));
        assert!(matches!(runtime.context, Observable::Waiting));
        assert!(matches!(runtime.quota, Observable::Waiting));
    }

    #[test]
    fn observed_stale_unsupported_and_failed_keep_source_and_time() {
        let observed = Observable::observed("opus".to_string(), provider(), T0, Some(T0 + 60_000));
        let stale = Observable::stale("sonnet".to_string(), provider(), T0 - 1, Some(T0));
        let unsupported = Observable::<String>::unsupported(provider(), T0 + 1);
        let failed = Observable::<String>::failed(provider(), T0 + 2, "probe timed out");

        for fact in [&observed, &stale, &unsupported, &failed] {
            let json = serde_json::to_string(fact).unwrap();
            let round_trip: Observable<String> = serde_json::from_str(&json).unwrap();
            assert_eq!(&round_trip, fact);
            assert_eq!(
                round_trip.source().unwrap().kind,
                ObservationSourceKind::Provider
            );
            assert!(round_trip.observed_at_ms().is_some());
        }
        assert!(stale.is_stale());
        assert_eq!(stale.value().map(String::as_str), Some("sonnet"));
        assert!(unsupported.value().is_none());
        assert!(failed.value().is_none());
    }

    #[test]
    fn context_and_quota_preserve_reported_semantics_without_complements() {
        let context = ContextUsageSnapshot {
            scope_id: Some("conversation-1".into()),
            measurement: UsageMeasurement {
                kind: UsageMeasurementKind::Used,
                amount: 42_000.0,
                unit: UsageUnit::Tokens,
                total: None,
            },
            effective_window: None,
            window_size_tokens: None,
            used_percentage: None,
            remaining_percentage: None,
            current_usage: None,
        };
        let quota = QuotaSnapshot {
            scope_id: Some("account-1".into()),
            scope_label: Some("work account".into()),
            windows: vec![QuotaWindow {
                label: "five hour".into(),
                measurement: UsageMeasurement {
                    kind: UsageMeasurementKind::ProviderPercent,
                    amount: 61.0,
                    unit: UsageUnit::Percent,
                    total: None,
                },
                resets_at_ms: Some(T0 + 3_600_000),
                exhausted: None,
                hard_limit: Some(true),
            }],
        };

        let context_json = serde_json::to_value(&context).unwrap();
        let quota_json = serde_json::to_value(&quota).unwrap();
        assert_eq!(context_json["measurement"]["kind"], "used");
        assert!(context_json["measurement"].get("remaining").is_none());
        assert_eq!(
            quota_json["windows"][0]["measurement"]["kind"],
            "provider_percent"
        );
        assert!(quota_json["windows"][0]["measurement"]
            .get("remaining")
            .is_none());
    }

    #[test]
    fn newer_negative_observations_are_not_lost_to_cached_values() {
        let cached = Observable::stale("cached", provider(), T0, Some(T0 + 1));
        let unsupported = Observable::unsupported(provider(), T0 + 2);
        assert!(matches!(
            cached.prefer_newer(unsupported),
            Observable::Unsupported { .. }
        ));
    }

    #[test]
    fn elapsed_deadlines_are_stale_at_the_boundary_and_keep_their_receipt() {
        let before = Observable::observed("value", provider(), T0, Some(T0 + 10));
        assert!(!before.is_stale_at(T0 + 9));

        let expired = before.stale_if_expired(T0 + 10);
        assert!(expired.is_stale_at(T0 + 10));
        assert_eq!(expired.value(), Some(&"value"));
        assert!(matches!(
            expired,
            Observable::Stale {
                observed_at_ms: T0,
                expires_at_ms: Some(expires_at_ms),
                ..
            } if expires_at_ms == T0 + 10
        ));

        let unbounded = Observable::observed("current", provider(), T0, None);
        assert!(!unbounded.is_stale_at(i64::MAX));
        assert!(matches!(
            unbounded.stale_if_expired(i64::MAX),
            Observable::Observed { .. }
        ));
    }

    #[test]
    fn newer_partial_current_configuration_keeps_independent_richer_facts() {
        let status_line = Observable::observed(
            LaunchConfiguration {
                model: Some("opus".into()),
                model_display_name: Some("Opus".into()),
                effort_level: Some("high".into()),
                thinking_enabled: Some(true),
                ..LaunchConfiguration::default()
            },
            provider(),
            T0,
            None,
        );
        let transcript = Observable::observed(
            LaunchConfiguration {
                model: Some("opus-latest-id".into()),
                ..LaunchConfiguration::default()
            },
            ObservationSource::new(ObservationSourceKind::Provider, "provider transcript"),
            T0 + 1,
            None,
        );

        let merged = prefer_newer_configuration(status_line, transcript);
        let current = merged.value().unwrap();
        assert_eq!(current.model.as_deref(), Some("opus-latest-id"));
        assert_eq!(current.model_display_name.as_deref(), Some("Opus"));
        assert_eq!(current.effort_level.as_deref(), Some("high"));
        assert_eq!(current.thinking_enabled, Some(true));
        assert_eq!(merged.observed_at_ms(), Some(T0 + 1));
    }

    #[test]
    fn late_older_partial_configuration_cannot_replace_newer_fields() {
        let newer = Observable::observed(
            LaunchConfiguration {
                model: Some("new-model".into()),
                effort_level: Some("xhigh".into()),
                ..LaunchConfiguration::default()
            },
            provider(),
            T0 + 2,
            None,
        );
        let older_completed_late = Observable::observed(
            LaunchConfiguration {
                model: Some("old-model".into()),
                model_display_name: Some("Useful display name".into()),
                ..LaunchConfiguration::default()
            },
            provider(),
            T0 + 1,
            None,
        );

        let merged = prefer_newer_configuration(newer, older_completed_late);
        let current = merged.value().unwrap();
        assert_eq!(current.model.as_deref(), Some("new-model"));
        assert_eq!(current.effort_level.as_deref(), Some("xhigh"));
        assert_eq!(
            current.model_display_name.as_deref(),
            Some("Useful display name")
        );
        assert_eq!(merged.observed_at_ms(), Some(T0 + 2));
    }
}
