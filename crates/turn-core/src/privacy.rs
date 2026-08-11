//! Local-data privacy contracts.
//!
//! These types deliberately live in the I/O-free domain crate. The store produces
//! database entries, the daemon adds private filesystem artifacts, the protocol
//! transports reports, and the window renders the same policy without inventing a
//! second vocabulary for any of them.

use crate::ids::{NodeId, SessionId, WorkspaceId};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const EVENT_RETENTION_DAYS_KEY: &str = "records.event_retention_days";
pub const EVENT_LIMIT_KEY: &str = "records.event_limit";
pub const EVENT_SESSION_FLOOR_KEY: &str = "records.event_session_floor";
pub const PREVIEW_RETENTION_DAYS_KEY: &str = "records.preview_retention_days";
pub const PREVIEWS_PER_AGENT_KEY: &str = "records.previews_per_agent";
pub const PREVIEW_LIMIT_KEY: &str = "records.preview_limit";
pub const TERMINAL_HISTORY_KEY: &str = "records.terminal_history";
pub const TERMINAL_JOURNAL_MIB_KEY: &str = "records.terminal_journal_mib";
pub const TERMINAL_CHECKPOINT_MIB_KEY: &str = "records.terminal_checkpoint_mib";
pub const DAEMON_LOG_MIB_KEY: &str = "records.daemon_log_mib";

pub const PRIVACY_POLICY_KEYS: [&str; 10] = [
    EVENT_RETENTION_DAYS_KEY,
    EVENT_LIMIT_KEY,
    EVENT_SESSION_FLOOR_KEY,
    PREVIEW_RETENTION_DAYS_KEY,
    PREVIEWS_PER_AGENT_KEY,
    PREVIEW_LIMIT_KEY,
    TERMINAL_HISTORY_KEY,
    TERMINAL_JOURNAL_MIB_KEY,
    TERMINAL_CHECKPOINT_MIB_KEY,
    DAEMON_LOG_MIB_KEY,
];

/// The smallest durable identity a privacy operation may address.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "snake_case")]
pub enum PrivacyScope {
    Installation,
    Workspace {
        workspace_id: WorkspaceId,
    },
    Session {
        session_id: SessionId,
    },
    Agent {
        session_id: SessionId,
        node_id: NodeId,
    },
}

impl PrivacyScope {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Installation => "installation",
            Self::Workspace { .. } => "workspace",
            Self::Session { .. } => "session",
            Self::Agent { .. } => "agent",
        }
    }
}

/// The retention policy currently enforced by the daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PrivacyPolicy {
    pub event_max_age_days: u32,
    pub event_max_records: u32,
    pub event_keep_per_session: u32,
    pub preview_max_age_days: u32,
    pub preview_keep_per_agent: u32,
    pub preview_max_records: u32,
    pub terminal_history_enabled: bool,
    pub terminal_journal_bytes: u64,
    pub terminal_checkpoint_bytes: u64,
    pub diagnostic_log_bytes: u64,
}

impl Default for PrivacyPolicy {
    fn default() -> Self {
        const MIB: u64 = 1024 * 1024;
        Self {
            event_max_age_days: 30,
            event_max_records: 50_000,
            event_keep_per_session: 50,
            preview_max_age_days: 30,
            preview_keep_per_agent: 20,
            preview_max_records: 2_000,
            terminal_history_enabled: true,
            terminal_journal_bytes: 8 * MIB,
            terminal_checkpoint_bytes: 4 * MIB,
            diagnostic_log_bytes: 4 * MIB,
        }
    }
}

impl PrivacyPolicy {
    /// Applies one already-validated catalogue value. Unknown or ill-shaped
    /// values are ignored so an older build never turns a future preference into
    /// an unsafe zero limit.
    pub fn apply(&mut self, key: &str, value: &Value) {
        const MIB: u64 = 1024 * 1024;
        let u32_value = || value.as_u64().and_then(|raw| u32::try_from(raw).ok());
        match key {
            EVENT_RETENTION_DAYS_KEY => {
                if let Some(value @ 1..=3650) = u32_value() {
                    self.event_max_age_days = value;
                }
            }
            EVENT_LIMIT_KEY => {
                if let Some(value @ 100..=1_000_000) = u32_value() {
                    self.event_max_records = value;
                }
            }
            EVENT_SESSION_FLOOR_KEY => {
                if let Some(value @ 0..=1000) = u32_value() {
                    self.event_keep_per_session = value;
                }
            }
            PREVIEW_RETENTION_DAYS_KEY => {
                if let Some(value @ 1..=3650) = u32_value() {
                    self.preview_max_age_days = value;
                }
            }
            PREVIEWS_PER_AGENT_KEY => {
                if let Some(value @ 1..=1000) = u32_value() {
                    self.preview_keep_per_agent = value;
                }
            }
            PREVIEW_LIMIT_KEY => {
                if let Some(value @ 100..=100_000) = u32_value() {
                    self.preview_max_records = value;
                }
            }
            TERMINAL_HISTORY_KEY => {
                if let Some(value) = value.as_bool() {
                    self.terminal_history_enabled = value;
                }
            }
            TERMINAL_JOURNAL_MIB_KEY => {
                if let Some(value @ 1..=64) = value.as_u64() {
                    self.terminal_journal_bytes = value.saturating_mul(MIB);
                }
            }
            TERMINAL_CHECKPOINT_MIB_KEY => {
                if let Some(value @ 1..=32) = value.as_u64() {
                    self.terminal_checkpoint_bytes = value.saturating_mul(MIB);
                }
            }
            DAEMON_LOG_MIB_KEY => {
                if let Some(value @ 1..=64) = value.as_u64() {
                    self.diagnostic_log_bytes = value.saturating_mul(MIB);
                }
            }
            _ => {}
        }
    }
}

/// One reviewable durable datum.
///
/// `content` contains the stored projection after field- and value-level secret
/// redaction. Files whose payload can be terminal output or injected agent config
/// are represented only by metadata; their bytes are never copied into an export.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PrivacyDatum {
    pub origin: String,
    pub data_type: String,
    /// `null` when the backing format has no timestamp. The field is never
    /// omitted, so every export row answers the timestamp question explicitly.
    #[serde(default)]
    pub timestamp_ms: Option<i64>,
    pub bytes: u64,
    pub content: Value,
}

/// Aggregate size/count for one durable data type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PrivacyCategory {
    pub data_type: String,
    pub items: u64,
    pub bytes: u64,
}

/// A current inventory. Telemetry is explicit even though Turn has no telemetry
/// transport, so an export can prove that fact rather than relying on absence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PrivacyReport {
    pub generated_ms: i64,
    pub scope: PrivacyScope,
    pub policy: PrivacyPolicy,
    pub telemetry_enabled: bool,
    pub telemetry_endpoints: u32,
    pub total_items: u64,
    pub total_bytes: u64,
    pub categories: Vec<PrivacyCategory>,
}

/// The on-disk, human-reviewable export document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PrivacyExportDocument {
    pub schema: u32,
    pub generated_ms: i64,
    pub scope: PrivacyScope,
    pub policy: PrivacyPolicy,
    pub telemetry_enabled: bool,
    pub telemetry_endpoints: u32,
    pub data: Vec<PrivacyDatum>,
}

/// Result of an export written with create-new semantics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PrivacyExportResult {
    pub path: String,
    pub items: u64,
    pub bytes: u64,
}

/// What one destructive privacy operation removed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PrivacyDeletionReport {
    pub scope: PrivacyScope,
    pub records_deleted: u64,
    pub files_deleted: u64,
    pub bytes_freed: u64,
    pub database_compacted: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub escaped_processes: Vec<NodeId>,
}

/// Offline installation purge. The stable lock inode and user checkout roots are
/// intentionally not data and intentionally not deleted.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct InstallationPurgeReport {
    pub files_deleted: u64,
    pub directories_deleted: u64,
    pub bytes_freed: u64,
    pub retained_checkout_roots: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn privacy_policy_ignores_values_outside_the_catalogue_bounds() {
        let mut policy = PrivacyPolicy::default();

        policy.apply(EVENT_RETENTION_DAYS_KEY, &serde_json::json!(0));
        policy.apply(EVENT_LIMIT_KEY, &serde_json::json!(99));
        policy.apply(PREVIEWS_PER_AGENT_KEY, &serde_json::json!(1001));
        policy.apply(TERMINAL_JOURNAL_MIB_KEY, &serde_json::json!(65));

        assert_eq!(policy, PrivacyPolicy::default());

        policy.apply(EVENT_RETENTION_DAYS_KEY, &serde_json::json!(7));
        policy.apply(EVENT_LIMIT_KEY, &serde_json::json!(500));
        policy.apply(TERMINAL_HISTORY_KEY, &serde_json::json!(false));
        assert_eq!(policy.event_max_age_days, 7);
        assert_eq!(policy.event_max_records, 500);
        assert!(!policy.terminal_history_enabled);
    }
}
