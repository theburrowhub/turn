//! Bounded observations of provider **account quota**.
//!
//! Account quota is not context usage. A transcript can establish how much of one
//! conversation's context is occupied; it cannot establish how much of the
//! operator's rolling provider allowance remains. This module therefore has a
//! separate vocabulary and only exposes a probe where the installed CLI owns a
//! documented local protocol for the fact.
//!
//! Codex exposes `account/rateLimits/read` through its local app-server JSON-RPC
//! protocol. The probe below starts that installed executable directly (never via
//! a shell), performs the required handshake, reads one snapshot, then terminates
//! it. No bearer token, account file or provider endpoint is read by Turn. The
//! child inherits the same authenticated CLI environment the operator already
//! uses and keeps all credentials inside Codex.
//!
//! Claude Code, Gemini CLI and OpenCode currently have no equivalent stable,
//! local, read-only contract. Scraping a dashboard, terminal pixels or private
//! credential files would turn an attractive number into an unreliable and
//! unsafe one, so [`account_quota_source_for_tool`] deliberately returns `None`
//! for them.

use serde_json::{Map, Value};
use std::{ffi::OsStr, io, process::Stdio, time::Duration};
use thiserror::Error;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{ChildStdout, Command},
};

use crate::text;

const INITIALIZE_REQUEST_ID: u64 = 1;
const QUOTA_REQUEST_ID: u64 = 2;
const MAX_ACCOUNT_QUOTA_MESSAGES: usize = 32;
const MAX_ACCOUNT_QUOTA_BUCKETS: usize = 32;

/// Minimum refresh interval recommended for one provider account.
///
/// Account quota is shared by every node using that login. The daemon should
/// cache one observation and fan it out, not start one app-server per agent.
pub const MIN_ACCOUNT_QUOTA_REFRESH_INTERVAL: Duration = Duration::from_secs(60);

/// The most time a caller can allow one account-quota refresh to occupy.
///
/// The public probe clamps longer requested deadlines to this limit. A status
/// refresh must never leave an app-server process waiting indefinitely.
pub const MAX_ACCOUNT_QUOTA_PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Maximum size of one app-server JSON message accepted by the probe.
///
/// A rate-limit snapshot is normally a few kilobytes. The deliberately generous
/// cap permits future buckets while bounding memory controlled by the child.
pub const MAX_ACCOUNT_QUOTA_MESSAGE_BYTES: usize = 64 * 1024;

/// A provider-owned source capable of reporting account quota.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountQuotaSource {
    /// Codex's local `account/rateLimits/read` app-server method.
    CodexAppServer,
}

/// Returns the stable local account-quota source for a tool, if one exists.
///
/// Tool ids are adapter ids, not arbitrary executable paths. Returning `None` is
/// intentional evidence that Turn must render quota as unavailable, rather than
/// silently substituting per-conversation context or locally accumulated spend.
pub fn account_quota_source_for_tool(tool: &str) -> Option<AccountQuotaSource> {
    tool.eq_ignore_ascii_case("codex")
        .then_some(AccountQuotaSource::CodexAppServer)
}

/// One provider account-quota snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountQuotaObservation {
    pub source: AccountQuotaSource,
    /// Metered buckets. Newer Codex versions can report more than one; older
    /// versions expose one backward-compatible bucket.
    pub buckets: Vec<AccountQuotaBucket>,
    /// Number of provider-issued rate-limit reset credits, when reported.
    pub reset_credits_available: Option<u64>,
}

/// One independently metered account bucket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountQuotaBucket {
    pub id: Option<String>,
    pub name: Option<String>,
    pub plan: Option<String>,
    pub primary: Option<AccountQuotaWindow>,
    pub secondary: Option<AccountQuotaWindow>,
    pub credits: Option<AccountCredits>,
    pub spend_control: Option<AccountSpendControl>,
    pub reached: Option<String>,
    pub spend_control_reached: Option<bool>,
}

/// A provider rolling limit window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccountQuotaWindow {
    pub used_percent: u8,
    pub resets_at_unix: Option<u64>,
    pub window_minutes: Option<u64>,
}

impl AccountQuotaWindow {
    /// Remaining allowance derived from the provider's integer `usedPercent`.
    pub fn remaining_percent(self) -> u8 {
        100 - self.used_percent
    }
}

/// Credit state returned alongside rolling limits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountCredits {
    pub has_credits: bool,
    pub unlimited: bool,
    pub balance: Option<String>,
}

/// Account-level spend control, distinct from rolling request windows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountSpendControl {
    pub limit: String,
    pub used: String,
    pub remaining_percent: u8,
    pub resets_at_unix: u64,
}

/// A malformed or rejected quota response.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AccountQuotaParseError {
    #[error("account quota response is not valid JSON")]
    InvalidJson,
    #[error("account quota response has an invalid {0} field")]
    InvalidField(&'static str),
    #[error("account quota response contains too many metered buckets")]
    TooManyBuckets,
    #[error("account quota request was rejected by the provider")]
    RemoteRejected,
}

/// Failure while asking the installed provider CLI for account quota.
#[derive(Debug, Error)]
pub enum QuotaProbeError {
    #[error("could not start the account quota provider")]
    Spawn(#[source] io::Error),
    #[error("account quota provider I/O failed")]
    Io(#[source] io::Error),
    #[error("account quota provider exceeded its response-size limit")]
    MessageTooLarge,
    #[error("account quota provider sent too many unrelated messages")]
    TooManyMessages,
    #[error("account quota provider returned an invalid protocol response")]
    Protocol(#[source] AccountQuotaParseError),
    #[error("account quota provider did not answer before the deadline")]
    TimedOut,
}

/// Parses a Codex JSON-RPC account-quota response.
///
/// `Ok(None)` means the valid JSON message is a notification or response to a
/// different request. The probe uses this to ignore asynchronous notifications
/// without ever treating one as the requested snapshot.
pub fn parse_codex_account_quota_response(
    message: &[u8],
    request_id: u64,
) -> Result<Option<AccountQuotaObservation>, AccountQuotaParseError> {
    let value: Value =
        serde_json::from_slice(message).map_err(|_| AccountQuotaParseError::InvalidJson)?;
    let root = value
        .as_object()
        .ok_or(AccountQuotaParseError::InvalidField("envelope"))?;

    let Some(id) = root.get("id") else {
        return Ok(None);
    };
    if id.as_u64() != Some(request_id) {
        return Ok(None);
    }
    if root.get("error").is_some_and(|error| !error.is_null()) {
        return Err(AccountQuotaParseError::RemoteRejected);
    }
    let result = root
        .get("result")
        .and_then(Value::as_object)
        .ok_or(AccountQuotaParseError::InvalidField("result"))?;
    parse_codex_result(result).map(Some)
}

/// Reads one account-quota snapshot from the installed Codex CLI.
///
/// The executable is invoked directly with `app-server --stdio`; it is never
/// interpolated into a shell command. `timeout` is clamped to
/// [`MAX_ACCOUNT_QUOTA_PROBE_TIMEOUT`]. The child is killed when the exchange
/// finishes or the future is cancelled.
pub async fn probe_codex_account_quota(
    executable: impl AsRef<OsStr>,
    timeout: Duration,
) -> Result<AccountQuotaObservation, QuotaProbeError> {
    let mut command = Command::new(executable);
    command.arg("app-server").arg("--stdio");
    probe_codex_command(command, timeout.min(MAX_ACCOUNT_QUOTA_PROBE_TIMEOUT)).await
}

async fn probe_codex_command(
    mut command: Command,
    timeout: Duration,
) -> Result<AccountQuotaObservation, QuotaProbeError> {
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // Provider diagnostics can contain account metadata and are not needed
        // to interpret the JSON-RPC result. Discarding them also prevents a full
        // stderr pipe from deadlocking the probe.
        .stderr(Stdio::null())
        .kill_on_drop(true);

    let exchange = async move {
        let mut child = command.spawn().map_err(QuotaProbeError::Spawn)?;
        let mut stdin = child.stdin.take().ok_or_else(closed_pipe)?;
        let stdout = child.stdout.take().ok_or_else(closed_pipe)?;
        let mut stdout = BufReader::new(stdout);

        write_message(
            &mut stdin,
            &serde_json::json!({
                "id": INITIALIZE_REQUEST_ID,
                "method": "initialize",
                "params": {
                    "clientInfo": {
                        "name": "turn-quota-probe",
                        "title": "Turn",
                        "version": env!("CARGO_PKG_VERSION")
                    },
                    "capabilities": { "experimentalApi": false }
                }
            }),
        )
        .await?;
        read_response(&mut stdout, INITIALIZE_REQUEST_ID).await?;

        write_message(&mut stdin, &serde_json::json!({ "method": "initialized" })).await?;
        write_message(
            &mut stdin,
            &serde_json::json!({
                "id": QUOTA_REQUEST_ID,
                "method": "account/rateLimits/read",
                "params": null
            }),
        )
        .await?;

        let mut observation = None;
        for _ in 0..MAX_ACCOUNT_QUOTA_MESSAGES {
            let message = read_message(&mut stdout).await?;
            match parse_codex_account_quota_response(&message, QUOTA_REQUEST_ID) {
                Ok(Some(current)) => {
                    observation = Some(current);
                    break;
                }
                Ok(None) => {}
                Err(error) => return Err(QuotaProbeError::Protocol(error)),
            }
        }
        let observation = observation.ok_or(QuotaProbeError::TooManyMessages)?;

        // This is a one-shot observer, not a second long-lived agent runtime.
        drop(stdin);
        let _ = child.start_kill();
        let _ = child.wait().await;
        Ok(observation)
    };

    tokio::time::timeout(timeout, exchange)
        .await
        .map_err(|_| QuotaProbeError::TimedOut)?
}

fn closed_pipe() -> QuotaProbeError {
    QuotaProbeError::Io(io::Error::new(
        io::ErrorKind::BrokenPipe,
        "account quota provider closed its protocol pipe",
    ))
}

async fn write_message(
    stdin: &mut tokio::process::ChildStdin,
    value: &Value,
) -> Result<(), QuotaProbeError> {
    let mut bytes = serde_json::to_vec(value)
        .map_err(|error| QuotaProbeError::Io(io::Error::new(io::ErrorKind::InvalidData, error)))?;
    bytes.push(b'\n');
    stdin.write_all(&bytes).await.map_err(QuotaProbeError::Io)?;
    stdin.flush().await.map_err(QuotaProbeError::Io)
}

async fn read_response(
    stdout: &mut BufReader<ChildStdout>,
    request_id: u64,
) -> Result<(), QuotaProbeError> {
    for _ in 0..MAX_ACCOUNT_QUOTA_MESSAGES {
        let message = read_message(stdout).await?;
        let value: Value = serde_json::from_slice(&message)
            .map_err(|_| QuotaProbeError::Protocol(AccountQuotaParseError::InvalidJson))?;
        let root = value.as_object().ok_or({
            QuotaProbeError::Protocol(AccountQuotaParseError::InvalidField("envelope"))
        })?;
        if root.get("id").and_then(Value::as_u64) != Some(request_id) {
            continue;
        }
        if root.get("error").is_some_and(|error| !error.is_null()) {
            return Err(QuotaProbeError::Protocol(
                AccountQuotaParseError::RemoteRejected,
            ));
        }
        if !root.get("result").is_some_and(Value::is_object) {
            return Err(QuotaProbeError::Protocol(
                AccountQuotaParseError::InvalidField("result"),
            ));
        }
        return Ok(());
    }
    Err(QuotaProbeError::TooManyMessages)
}

async fn read_message(stdout: &mut BufReader<ChildStdout>) -> Result<Vec<u8>, QuotaProbeError> {
    let mut message = Vec::new();
    let mut bounded = stdout.take((MAX_ACCOUNT_QUOTA_MESSAGE_BYTES + 1) as u64);
    let bytes = bounded
        .read_until(b'\n', &mut message)
        .await
        .map_err(QuotaProbeError::Io)?;
    if bytes == 0 {
        return Err(closed_pipe());
    }
    if message.len() > MAX_ACCOUNT_QUOTA_MESSAGE_BYTES {
        return Err(QuotaProbeError::MessageTooLarge);
    }
    Ok(message)
}

fn parse_codex_result(
    root: &Map<String, Value>,
) -> Result<AccountQuotaObservation, AccountQuotaParseError> {
    let historical = object(root, "rateLimits")?;
    let reset_credits_available = match root.get("rateLimitResetCredits") {
        None | Some(Value::Null) => None,
        Some(value) => Some(non_negative_u64(
            object_value(value, "rateLimitResetCredits")?.get("availableCount"),
            "rateLimitResetCredits.availableCount",
        )?),
    };

    let mut buckets = Vec::new();
    match root.get("rateLimitsByLimitId") {
        None | Some(Value::Null) => {}
        Some(Value::Object(by_id)) => {
            if by_id.len() > MAX_ACCOUNT_QUOTA_BUCKETS {
                return Err(AccountQuotaParseError::TooManyBuckets);
            }
            let mut entries = by_id.iter().collect::<Vec<_>>();
            entries.sort_by_key(|(id, _)| *id);
            for (id, value) in entries {
                buckets.push(parse_bucket(
                    object_value(value, "rateLimitsByLimitId bucket")?,
                    Some(id),
                )?);
            }
        }
        Some(_) => {
            return Err(AccountQuotaParseError::InvalidField("rateLimitsByLimitId"));
        }
    }
    if buckets.is_empty() {
        buckets.push(parse_bucket(historical, None)?);
    }

    Ok(AccountQuotaObservation {
        source: AccountQuotaSource::CodexAppServer,
        buckets,
        reset_credits_available,
    })
}

fn parse_bucket(
    snapshot: &Map<String, Value>,
    fallback_id: Option<&str>,
) -> Result<AccountQuotaBucket, AccountQuotaParseError> {
    let id = optional_text(snapshot.get("limitId"), "rateLimits.limitId")?
        .or_else(|| fallback_id.and_then(safe_text));
    let name = optional_text(snapshot.get("limitName"), "rateLimits.limitName")?;
    let plan = optional_text(snapshot.get("planType"), "rateLimits.planType")?;
    let primary = optional_window(snapshot.get("primary"), "rateLimits.primary")?;
    let secondary = optional_window(snapshot.get("secondary"), "rateLimits.secondary")?;
    let credits = optional_credits(snapshot.get("credits"))?;
    let spend_control = optional_spend_control(snapshot.get("individualLimit"))?;
    let reached = optional_text(
        snapshot.get("rateLimitReachedType"),
        "rateLimits.rateLimitReachedType",
    )?;
    let spend_control_reached = optional_bool(
        snapshot.get("spendControlReached"),
        "rateLimits.spendControlReached",
    )?;

    Ok(AccountQuotaBucket {
        id,
        name,
        plan,
        primary,
        secondary,
        credits,
        spend_control,
        reached,
        spend_control_reached,
    })
}

fn optional_window(
    value: Option<&Value>,
    field: &'static str,
) -> Result<Option<AccountQuotaWindow>, AccountQuotaParseError> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let value = object_value(value, field)?;
    let used_percent = percentage(value.get("usedPercent"), "rateLimits.usedPercent")?;
    let resets_at_unix = optional_non_negative_u64(value.get("resetsAt"), "rateLimits.resetsAt")?;
    let window_minutes = optional_non_negative_u64(
        value.get("windowDurationMins"),
        "rateLimits.windowDurationMins",
    )?;
    Ok(Some(AccountQuotaWindow {
        used_percent,
        resets_at_unix,
        window_minutes,
    }))
}

fn optional_credits(
    value: Option<&Value>,
) -> Result<Option<AccountCredits>, AccountQuotaParseError> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let value = object_value(value, "rateLimits.credits")?;
    Ok(Some(AccountCredits {
        has_credits: required_bool(value.get("hasCredits"), "rateLimits.credits.hasCredits")?,
        unlimited: required_bool(value.get("unlimited"), "rateLimits.credits.unlimited")?,
        balance: optional_text(value.get("balance"), "rateLimits.credits.balance")?,
    }))
}

fn optional_spend_control(
    value: Option<&Value>,
) -> Result<Option<AccountSpendControl>, AccountQuotaParseError> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let value = object_value(value, "rateLimits.individualLimit")?;
    Ok(Some(AccountSpendControl {
        limit: required_text(value.get("limit"), "rateLimits.individualLimit.limit")?,
        used: required_text(value.get("used"), "rateLimits.individualLimit.used")?,
        remaining_percent: percentage(
            value.get("remainingPercent"),
            "rateLimits.individualLimit.remainingPercent",
        )?,
        resets_at_unix: non_negative_u64(
            value.get("resetsAt"),
            "rateLimits.individualLimit.resetsAt",
        )?,
    }))
}

fn object<'a>(
    root: &'a Map<String, Value>,
    key: &'static str,
) -> Result<&'a Map<String, Value>, AccountQuotaParseError> {
    object_value(
        root.get(key)
            .ok_or(AccountQuotaParseError::InvalidField(key))?,
        key,
    )
}

fn object_value<'a>(
    value: &'a Value,
    field: &'static str,
) -> Result<&'a Map<String, Value>, AccountQuotaParseError> {
    value
        .as_object()
        .ok_or(AccountQuotaParseError::InvalidField(field))
}

fn required_bool(
    value: Option<&Value>,
    field: &'static str,
) -> Result<bool, AccountQuotaParseError> {
    value
        .and_then(Value::as_bool)
        .ok_or(AccountQuotaParseError::InvalidField(field))
}

fn optional_bool(
    value: Option<&Value>,
    field: &'static str,
) -> Result<Option<bool>, AccountQuotaParseError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_bool()
            .map(Some)
            .ok_or(AccountQuotaParseError::InvalidField(field)),
    }
}

fn percentage(value: Option<&Value>, field: &'static str) -> Result<u8, AccountQuotaParseError> {
    let value = non_negative_u64(value, field)?;
    u8::try_from(value)
        .ok()
        .filter(|value| *value <= 100)
        .ok_or(AccountQuotaParseError::InvalidField(field))
}

fn non_negative_u64(
    value: Option<&Value>,
    field: &'static str,
) -> Result<u64, AccountQuotaParseError> {
    value
        .and_then(Value::as_u64)
        .ok_or(AccountQuotaParseError::InvalidField(field))
}

fn optional_non_negative_u64(
    value: Option<&Value>,
    field: &'static str,
) -> Result<Option<u64>, AccountQuotaParseError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or(AccountQuotaParseError::InvalidField(field)),
    }
}

fn required_text(
    value: Option<&Value>,
    field: &'static str,
) -> Result<String, AccountQuotaParseError> {
    value
        .and_then(Value::as_str)
        .and_then(safe_text)
        .ok_or(AccountQuotaParseError::InvalidField(field))
}

fn optional_text(
    value: Option<&Value>,
    field: &'static str,
) -> Result<Option<String>, AccountQuotaParseError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_str()
            .and_then(safe_text)
            .map(Some)
            .ok_or(AccountQuotaParseError::InvalidField(field)),
    }
}

fn safe_text(raw: &str) -> Option<String> {
    if raw.chars().count() > text::MAX_FIELD_CHARS {
        return None;
    }
    text::field(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> &'static [u8] {
        include_bytes!("../tests/fixtures/quota/codex-rate-limits-response.jsonl")
    }

    #[test]
    fn only_codex_claims_a_stable_local_account_quota_source() {
        assert_eq!(
            account_quota_source_for_tool("codex"),
            Some(AccountQuotaSource::CodexAppServer)
        );
        assert_eq!(
            account_quota_source_for_tool("CODEX"),
            Some(AccountQuotaSource::CodexAppServer)
        );
        for tool in ["claude-code", "gemini-cli", "opencode", "generic"] {
            assert_eq!(account_quota_source_for_tool(tool), None, "tool={tool}");
        }
    }

    #[test]
    fn parser_ignores_notifications_and_other_request_ids() {
        assert_eq!(
            parse_codex_account_quota_response(
                br#"{"method":"account/rateLimits/updated","params":{}}"#,
                QUOTA_REQUEST_ID,
            ),
            Ok(None)
        );
        assert_eq!(
            parse_codex_account_quota_response(br#"{"id":99,"result":{}}"#, QUOTA_REQUEST_ID),
            Ok(None)
        );
    }

    #[test]
    fn percentages_are_facts_not_values_turn_clamps_into_plausibility() {
        let invalid = br#"{"id":2,"result":{"rateLimits":{"primary":{"usedPercent":101}}}}"#;
        assert_eq!(
            parse_codex_account_quota_response(invalid, QUOTA_REQUEST_ID),
            Err(AccountQuotaParseError::InvalidField(
                "rateLimits.usedPercent"
            ))
        );
    }

    #[test]
    fn a_bounded_message_cannot_expand_into_an_unbounded_bucket_list() {
        let buckets = (0..=MAX_ACCOUNT_QUOTA_BUCKETS)
            .map(|index| format!(r#""bucket-{index}":{{}}"#))
            .collect::<Vec<_>>()
            .join(",");
        let response = format!(
            r#"{{"id":2,"result":{{"rateLimits":{{}},"rateLimitsByLimitId":{{{buckets}}}}}}}"#
        );
        assert_eq!(
            parse_codex_account_quota_response(response.as_bytes(), QUOTA_REQUEST_ID),
            Err(AccountQuotaParseError::TooManyBuckets)
        );
    }

    #[test]
    fn response_text_is_bounded_and_made_safe_for_display() {
        let hostile = br#"{"id":2,"result":{"rateLimits":{"limitName":"safe\u001b[2Jforged","primary":{"usedPercent":1}}}}"#;
        let observation = parse_codex_account_quota_response(hostile, QUOTA_REQUEST_ID)
            .expect("valid envelope")
            .expect("matching response");
        assert_eq!(observation.buckets[0].name.as_deref(), Some("safeforged"));

        let oversized = format!(
            r#"{{"id":2,"result":{{"rateLimits":{{"limitName":"{}"}}}}}}"#,
            "x".repeat(text::MAX_FIELD_CHARS + 1)
        );
        assert_eq!(
            parse_codex_account_quota_response(oversized.as_bytes(), QUOTA_REQUEST_ID),
            Err(AccountQuotaParseError::InvalidField("rateLimits.limitName"))
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn bounded_probe_completes_the_handshake_and_reads_the_snapshot() {
        let response = std::str::from_utf8(fixture()).expect("utf8 fixture");
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg(
                "IFS= read -r initialize; printf '%s\\n' '{\"id\":1,\"result\":{}}'; \
                 IFS= read -r initialized; IFS= read -r quota; printf '%s\\n' \"$TURN_TEST_QUOTA\"",
            )
            .env("TURN_TEST_QUOTA", response.trim());

        let observation = probe_codex_command(command, Duration::from_secs(2))
            .await
            .expect("bounded fake app-server exchange");
        assert_eq!(observation.buckets.len(), 2);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stalled_provider_is_cancelled_at_the_deadline() {
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg("IFS= read -r initialize; IFS= read -r forever");
        let error = probe_codex_command(command, Duration::from_millis(20))
            .await
            .expect_err("stalled provider must time out");
        assert!(matches!(error, QuotaProbeError::TimedOut));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn oversized_protocol_message_is_refused_before_json_parsing() {
        let mut command = Command::new("sh");
        command.arg("-c").arg(format!(
            "IFS= read -r initialize; printf '%*s\\n' {} ''",
            MAX_ACCOUNT_QUOTA_MESSAGE_BYTES + 1
        ));
        let error = probe_codex_command(command, Duration::from_secs(2))
            .await
            .expect_err("oversized response must be bounded");
        assert!(matches!(error, QuotaProbeError::MessageTooLarge));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn notification_flood_cannot_keep_the_probe_scanning_forever() {
        let mut command = Command::new("sh");
        command.arg("-c").arg(format!(
            "IFS= read -r initialize; i=0; while [ $i -lt {} ]; do \
             printf '%s\\n' '{{\"method\":\"noise\"}}'; i=$((i + 1)); done",
            MAX_ACCOUNT_QUOTA_MESSAGES
        ));
        let error = probe_codex_command(command, Duration::from_secs(2))
            .await
            .expect_err("unrelated message flood must be bounded");
        assert!(matches!(error, QuotaProbeError::TooManyMessages));
    }

    #[test]
    fn fixture_parses_into_sorted_multi_bucket_account_quota() {
        let observation = parse_codex_account_quota_response(fixture(), QUOTA_REQUEST_ID)
            .expect("valid fixture")
            .expect("matching response");
        assert_eq!(observation.source, AccountQuotaSource::CodexAppServer);
        assert_eq!(observation.reset_credits_available, Some(2));
        assert_eq!(observation.buckets.len(), 2);
        assert_eq!(observation.buckets[0].id.as_deref(), Some("codex"));
        assert_eq!(observation.buckets[0].name.as_deref(), Some("Codex"));
        assert_eq!(observation.buckets[0].plan.as_deref(), Some("plus"));
        assert_eq!(
            observation.buckets[0].primary,
            Some(AccountQuotaWindow {
                used_percent: 37,
                resets_at_unix: Some(1_800_000_000),
                window_minutes: Some(300),
            })
        );
        assert_eq!(
            observation.buckets[0]
                .primary
                .expect("primary")
                .remaining_percent(),
            63
        );
        assert_eq!(observation.buckets[1].id.as_deref(), Some("review"));
    }
}
