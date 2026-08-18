//! Claude Code status-line preservation and runtime telemetry.
//!
//! Claude Code's command-line `--settings` layer outranks local, project and user
//! settings. Turn therefore cannot install a status-line observer by simply
//! replacing `statusLine`: doing so would erase the operator's effective command.
//! This module resolves that lower-precedence command, places it in private
//! per-node scratch, and installs a private fan-out wrapper that delegates to it.
//! Managed settings remain higher priority and are never copied or overridden.

use crate::adapter::{EventContext, LaunchContext};
use crate::text;
use serde_json::{Map, Value};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use turn_core::event::{AgentRef, Confidence, EventKind, EventSource, TurnEvent};
use turn_core::model::{
    AgentLaunchFacts, AgentRuntimeMetadata, ContextTokenUsage, ContextUsageSnapshot,
    LaunchConfiguration, Observable, ObservationSource, ObservationSourceKind, QuotaSnapshot,
    QuotaWindow, UsageMeasurement, UsageMeasurementKind, UsageUnit,
};

const MAX_SETTINGS_BYTES: usize = 1024 * 1024;
const MAX_EXACT_F64_INTEGER: u64 = 1_u64 << 53;
const WRAPPER_NAME: &str = "claude-statusline-turn.sh";
const ORIGINAL_NAME: &str = "claude-statusline-original.sh";
const STATUS_SOURCE_LABEL: &str = "claude-code status line";

pub(crate) struct PreparedStatusLine {
    pub setting: Option<Value>,
    pub note: &'static str,
}

struct EffectiveStatusLine {
    object: Map<String, Value>,
    command: String,
}

enum LowerSettings {
    Ready {
        status_line: Option<EffectiveStatusLine>,
        disabled: bool,
    },
    Unavailable,
}

#[derive(Clone)]
struct SettingsPaths {
    lower_precedence: Vec<PathBuf>,
    managed: Vec<PathBuf>,
}

impl SettingsPaths {
    fn for_launch(ctx: &LaunchContext) -> Self {
        let project = PathBuf::from(&ctx.cwd).join(".claude");
        let mut lower_precedence = vec![
            project.join("settings.local.json"),
            project.join("settings.json"),
        ];
        if let Some(config_dir) = claude_config_dir() {
            lower_precedence.push(config_dir.join("settings.json"));
        }

        Self {
            lower_precedence,
            managed: managed_settings_paths(),
        }
    }
}

/// Installs a status-line fan-out only when the user's effective command can be
/// preserved exactly and no accessible managed source owns the feature.
pub(crate) fn prepare(ctx: &LaunchContext) -> PreparedStatusLine {
    prepare_with_paths(ctx, SettingsPaths::for_launch(ctx))
}

fn prepare_with_paths(ctx: &LaunchContext, paths: SettingsPaths) -> PreparedStatusLine {
    let Some(helper) = ctx.endpoint.helper_path.as_deref() else {
        return PreparedStatusLine {
            setting: None,
            note: "Claude status telemetry is unavailable because the turn-hook helper is missing.",
        };
    };

    #[cfg(not(unix))]
    {
        let _ = helper;
        return PreparedStatusLine {
            setting: None,
            note: "Claude status telemetry is unavailable on this platform; the user's status line is untouched.",
        };
    }

    #[cfg(unix)]
    {
        if managed_status_line_blocked(&paths.managed) != Some(false) {
            return PreparedStatusLine {
                setting: None,
                note: "Claude status telemetry is controlled or disabled by higher-priority managed settings; Turn left it untouched.",
            };
        }

        let (status_line, disabled) = match resolve_lower_settings(&paths.lower_precedence) {
            LowerSettings::Ready {
                status_line,
                disabled,
            } => (status_line, disabled),
            LowerSettings::Unavailable => {
                return PreparedStatusLine {
                    setting: None,
                    note: "Claude status telemetry was not installed because the effective status line could not be resolved safely; it is untouched.",
                };
            }
        };
        if disabled {
            return PreparedStatusLine {
                setting: None,
                note: "Claude status telemetry is disabled by the effective Claude Code settings; Turn left that choice untouched.",
            };
        }

        let original_path = status_line
            .as_ref()
            .map(|_| ctx.scratch_dir.join(ORIGINAL_NAME));
        if let (Some(original), Some(path)) = (&status_line, original_path.as_deref()) {
            let script = format!("#!/bin/sh\n{}\n", original.command);
            if write_private_executable(path, script.as_bytes()).is_err() {
                return PreparedStatusLine {
                    setting: None,
                    note: "Claude status telemetry could not create its private fan-out; the user's status line is untouched.",
                };
            }
        }

        let wrapper_path = ctx.scratch_dir.join(WRAPPER_NAME);
        let mut wrapper = format!("#!/bin/sh\nexec {} --statusline", shell_quote_path(helper));
        if let Some(path) = original_path.as_deref() {
            wrapper.push(' ');
            wrapper.push_str(&shell_quote_path(path));
        }
        wrapper.push('\n');
        if write_private_executable(&wrapper_path, wrapper.as_bytes()).is_err() {
            return PreparedStatusLine {
                setting: None,
                note: "Claude status telemetry could not create its private fan-out; the user's status line is untouched.",
            };
        }

        let mut object = status_line.map_or_else(Map::new, |line| line.object);
        object.insert("type".into(), Value::String("command".into()));
        object.insert(
            "command".into(),
            Value::String(shell_quote_path(&wrapper_path)),
        );
        PreparedStatusLine {
            setting: Some(Value::Object(object)),
            note: if original_path.is_some() {
                "Claude status telemetry is active through a private fan-out; the effective user status line is preserved."
            } else {
                "Claude status telemetry is active with Turn's compact fallback status line."
            },
        }
    }
}

fn resolve_lower_settings(paths: &[PathBuf]) -> LowerSettings {
    let mut status_line = None;
    let mut status_line_decided = false;
    let mut disabled = false;
    let mut disabled_decided = false;

    for path in paths {
        let document = match read_settings(path) {
            Ok(Some(document)) => document,
            Ok(None) => continue,
            Err(()) => return LowerSettings::Unavailable,
        };
        let Some(object) = document.as_object() else {
            return LowerSettings::Unavailable;
        };

        if !status_line_decided {
            if let Some(value) = object.get("statusLine") {
                let Some(line) = effective_status_line(value) else {
                    return LowerSettings::Unavailable;
                };
                status_line = Some(line);
                status_line_decided = true;
            }
        }
        if !disabled_decided {
            if let Some(value) = object.get("disableAllHooks") {
                let Some(value) = value.as_bool() else {
                    return LowerSettings::Unavailable;
                };
                disabled = value;
                disabled_decided = true;
            }
        }
    }

    LowerSettings::Ready {
        status_line,
        disabled,
    }
}

fn effective_status_line(value: &Value) -> Option<EffectiveStatusLine> {
    let object = value.as_object()?.clone();
    if object.get("type")?.as_str()? != "command" {
        return None;
    }
    let command = object.get("command")?.as_str()?.to_string();
    if command.trim().is_empty() {
        return None;
    }
    Some(EffectiveStatusLine { object, command })
}

fn read_settings(path: &Path) -> Result<Option<Value>, ()> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(()),
    };
    if !metadata.is_file() || metadata.len() > MAX_SETTINGS_BYTES as u64 {
        return Err(());
    }
    let file = std::fs::File::open(path).map_err(|_| ())?;
    let mut contents = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_SETTINGS_BYTES as u64 + 1)
        .read_to_end(&mut contents)
        .map_err(|_| ())?;
    if contents.len() > MAX_SETTINGS_BYTES {
        return Err(());
    }
    serde_json::from_slice(&contents).map(Some).map_err(|_| ())
}

/// `Some(false)` means the accessible managed tier permits a CLI status line.
/// `Some(true)` means it owns/disables the feature. `None` is deliberately
/// conservative: a managed document exists but cannot be inspected safely.
fn managed_status_line_blocked(paths: &[PathBuf]) -> Option<bool> {
    let mut status_line_defined = false;
    let mut disable_all_hooks = None;
    let mut managed_only = None;

    for path in paths {
        let document = match read_settings(path) {
            Ok(Some(document)) => document,
            Ok(None) => continue,
            Err(()) => return None,
        };
        let object = document.as_object()?;
        if object.contains_key("statusLine") {
            status_line_defined = true;
        }
        if let Some(value) = object.get("disableAllHooks") {
            disable_all_hooks = Some(value.as_bool()?);
        }
        if let Some(value) = object.get("allowManagedHooksOnly") {
            managed_only = Some(value.as_bool()?);
        }
    }

    Some(status_line_defined || disable_all_hooks == Some(true) || managed_only == Some(true))
}

fn claude_config_dir() -> Option<PathBuf> {
    if let Some(path) = nonempty_env("CLAUDE_CONFIG_DIR") {
        return Some(PathBuf::from(path));
    }
    nonempty_env(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .map(|home| home.join(".claude"))
}

fn nonempty_env(name: &str) -> Option<OsString> {
    std::env::var_os(name).filter(|value| !value.is_empty())
}

fn managed_settings_paths() -> Vec<PathBuf> {
    #[cfg(target_os = "macos")]
    let root = PathBuf::from("/Library/Application Support/ClaudeCode");
    #[cfg(any(target_os = "linux", target_os = "android"))]
    let root = PathBuf::from("/etc/claude-code");
    #[cfg(target_os = "windows")]
    let root = PathBuf::from(r"C:\Program Files\ClaudeCode");
    #[cfg(not(any(
        target_os = "macos",
        target_os = "linux",
        target_os = "android",
        target_os = "windows"
    )))]
    return Vec::new();

    #[cfg(any(
        target_os = "macos",
        target_os = "linux",
        target_os = "android",
        target_os = "windows"
    ))]
    {
        let mut paths = vec![root.join("managed-settings.json")];
        let drop_ins = root.join("managed-settings.d");
        if let Ok(entries) = std::fs::read_dir(drop_ins) {
            let mut files: Vec<PathBuf> = entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| {
                    path.extension().and_then(|extension| extension.to_str()) == Some("json")
                        && !path
                            .file_name()
                            .and_then(|name| name.to_str())
                            .is_some_and(|name| name.starts_with('.'))
                })
                .collect();
            files.sort();
            paths.extend(files);
        }
        paths
    }
}

#[cfg(unix)]
fn write_private_executable(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o700)
        .open(path)?;
    file.write_all(contents)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    file.sync_all()
}

#[cfg(unix)]
fn shell_quote_path(path: &Path) -> String {
    let text = path.to_string_lossy();
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// Projects Claude Code's documented status-line schema into typed runtime
/// metadata. Every ignored payload member, including cwd and transcript path,
/// disappears at this boundary.
pub(crate) fn observation_event(payload: &Value, ctx: &EventContext) -> Option<TurnEvent> {
    let source = ObservationSource::new(ObservationSourceKind::Provider, STATUS_SOURCE_LABEL);
    let scope_id = payload
        .get("session_id")
        .and_then(Value::as_str)
        .and_then(text::identifier);
    let model = payload
        .pointer("/model/id")
        .and_then(Value::as_str)
        .and_then(text::field);
    let model_display_name = payload
        .pointer("/model/display_name")
        .and_then(Value::as_str)
        .and_then(text::field);
    let effort_level = payload
        .pointer("/effort/level")
        .and_then(Value::as_str)
        .and_then(valid_effort);
    let thinking_enabled = payload
        .pointer("/thinking/enabled")
        .and_then(Value::as_bool);

    let launch_value = LaunchConfiguration {
        model: model.clone(),
        model_display_name,
        effort_level,
        thinking_enabled,
        ..LaunchConfiguration::default()
    };
    let launch = if launch_value.model.is_some()
        || launch_value.model_display_name.is_some()
        || launch_value.effort_level.is_some()
        || launch_value.thinking_enabled.is_some()
    {
        AgentLaunchFacts {
            current: Observable::observed(launch_value, source.clone(), ctx.timestamp_ms, None),
            ..AgentLaunchFacts::default()
        }
    } else {
        AgentLaunchFacts::default()
    };
    let context = context_snapshot(payload, scope_id.clone())
        .map_or(Observable::Waiting, |context| {
            Observable::observed(context, source.clone(), ctx.timestamp_ms, None)
        });
    let quota = quota_snapshot(payload).map_or(Observable::Waiting, |quota| {
        let expires_at_ms = quota
            .windows
            .iter()
            .filter_map(|window| window.resets_at_ms)
            .min();
        Observable::observed(quota, source.clone(), ctx.timestamp_ms, expires_at_ms)
    });

    if matches!(launch.current, Observable::Waiting)
        && matches!(context, Observable::Waiting)
        && matches!(quota, Observable::Waiting)
    {
        return None;
    }

    let runtime = AgentRuntimeMetadata {
        launch,
        context,
        quota,
    };
    Some(
        TurnEvent::new(
            ctx.session_id.clone(),
            EventKind::AgentRuntimeObserved {
                runtime: Box::new(runtime),
            },
            EventSource::SideChannel {
                tool: "claude-code".into(),
                channel: "provider status line".into(),
            },
            Confidence::Explicit,
            ctx.timestamp_ms,
        )
        .with_node(ctx.node_id.clone())
        .with_agent(AgentRef {
            provider: Some("anthropic".into()),
            tool: Some("claude-code".into()),
            model,
            external_id: scope_id,
        }),
    )
}

fn context_snapshot(payload: &Value, scope_id: Option<String>) -> Option<ContextUsageSnapshot> {
    let window = payload.get("context_window")?.as_object()?;
    let total_input = exact_u64(window.get("total_input_tokens"));
    let total_output = exact_u64(window.get("total_output_tokens"));
    let used_tokens = match (total_input, total_output) {
        (None, None) => None,
        (input, output) => input
            .unwrap_or(0)
            .checked_add(output.unwrap_or(0))
            .filter(|value| *value <= MAX_EXACT_F64_INTEGER),
    };
    let window_size_tokens = exact_u64(window.get("context_window_size"));
    let used_percentage = percentage(window.get("used_percentage"));
    let remaining_percentage = percentage(window.get("remaining_percentage"));
    let measurement = if let Some(used) = used_tokens {
        UsageMeasurement {
            kind: UsageMeasurementKind::Used,
            amount: used as f64,
            unit: UsageUnit::Tokens,
            total: window_size_tokens.map(|size| size as f64),
        }
    } else if let Some(used) = used_percentage {
        UsageMeasurement {
            kind: UsageMeasurementKind::ProviderPercent,
            amount: used,
            unit: UsageUnit::Percent,
            total: Some(100.0),
        }
    } else {
        let remaining = remaining_percentage?;
        UsageMeasurement {
            kind: UsageMeasurementKind::Remaining,
            amount: remaining,
            unit: UsageUnit::Percent,
            total: Some(100.0),
        }
    };

    Some(ContextUsageSnapshot {
        scope_id,
        measurement,
        effective_window: None,
        window_size_tokens,
        used_percentage,
        remaining_percentage,
        current_usage: current_usage(window.get("current_usage")),
    })
}

fn current_usage(value: Option<&Value>) -> Option<ContextTokenUsage> {
    let object = value?.as_object()?;
    let usage = ContextTokenUsage {
        input_tokens: exact_u64(object.get("input_tokens")),
        output_tokens: exact_u64(object.get("output_tokens")),
        cache_creation_input_tokens: exact_u64(object.get("cache_creation_input_tokens")),
        cache_read_input_tokens: exact_u64(object.get("cache_read_input_tokens")),
    };
    (usage.input_tokens.is_some()
        || usage.output_tokens.is_some()
        || usage.cache_creation_input_tokens.is_some()
        || usage.cache_read_input_tokens.is_some())
    .then_some(usage)
}

fn quota_snapshot(payload: &Value) -> Option<QuotaSnapshot> {
    let limits = payload.get("rate_limits")?.as_object()?;
    let mut windows = Vec::new();
    if let Some(window) = quota_window(limits.get("five_hour"), "5h") {
        windows.push(window);
    }
    if let Some(window) = quota_window(limits.get("seven_day"), "7d") {
        windows.push(window);
    }
    (!windows.is_empty()).then_some(QuotaSnapshot {
        scope_id: None,
        scope_label: Some("Claude.ai".into()),
        windows,
    })
}

fn quota_window(value: Option<&Value>, label: &str) -> Option<QuotaWindow> {
    let object = value?.as_object()?;
    let used = percentage(object.get("used_percentage"))?;
    let mut remaining = 100.0 - used;
    if remaining == -0.0 {
        remaining = 0.0;
    }
    Some(QuotaWindow {
        label: label.into(),
        measurement: UsageMeasurement {
            kind: UsageMeasurementKind::Remaining,
            amount: remaining,
            unit: UsageUnit::Percent,
            total: Some(100.0),
        },
        resets_at_ms: object
            .get("resets_at")
            .and_then(Value::as_i64)
            .filter(|seconds| *seconds >= 0)
            .and_then(|seconds| seconds.checked_mul(1_000)),
        hard_limit: None,
    })
}

fn exact_u64(value: Option<&Value>) -> Option<u64> {
    value?
        .as_u64()
        .filter(|number| *number <= MAX_EXACT_F64_INTEGER)
}

fn percentage(value: Option<&Value>) -> Option<f64> {
    value?
        .as_f64()
        .filter(|number| number.is_finite() && (0.0..=100.0).contains(number))
}

fn valid_effort(value: &str) -> Option<String> {
    matches!(value, "low" | "medium" | "high" | "xhigh" | "max").then(|| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::HookEndpoint;
    use serde_json::json;
    use turn_core::ids::{NodeId, SessionId};

    const NOW: i64 = 1_700_000_000_000;

    fn event_context() -> EventContext {
        EventContext {
            session_id: SessionId::from_stored("sess_status_line"),
            node_id: NodeId::from_stored("proc_status_line"),
            timestamp_ms: NOW,
        }
    }

    fn write_json(path: &Path, value: Value) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
    }

    fn launch_context(project: &Path, scratch: PathBuf) -> LaunchContext {
        LaunchContext {
            session_id: SessionId::from_stored("sess_status_line"),
            node_id: NodeId::from_stored("proc_status_line"),
            cwd: project.to_string_lossy().into_owned(),
            command: "claude".into(),
            user_args: Vec::new(),
            launch_profile: None,
            endpoint: HookEndpoint {
                base_url: "http://127.0.0.1:51234".into(),
                token: "private-token".into(),
                helper_path: Some(PathBuf::from("/opt/Turn App/bin/turn-hook")),
            },
            scratch_dir: scratch,
        }
    }

    #[test]
    fn local_then_project_then_user_precedence_preserves_the_winning_object() {
        let dir = tempfile::tempdir().unwrap();
        let user = dir.path().join("user.json");
        let project = dir.path().join("project.json");
        let local = dir.path().join("local.json");
        write_json(
            &user,
            json!({"statusLine":{"type":"command","command":"user","padding":1}}),
        );
        write_json(
            &project,
            json!({"statusLine":{"type":"command","command":"project","refreshInterval":3}}),
        );
        write_json(
            &local,
            json!({"statusLine":{"type":"command","command":"local","padding":4,"refreshInterval":2,"hideVimModeIndicator":true,"timeout":900}}),
        );

        let LowerSettings::Ready {
            status_line: Some(line),
            disabled,
        } = resolve_lower_settings(&[local, project, user])
        else {
            panic!("the local status line should win")
        };
        assert!(!disabled);
        assert_eq!(line.command, "local");
        assert_eq!(line.object["padding"], 4);
        assert_eq!(line.object["refreshInterval"], 2);
        assert_eq!(line.object["hideVimModeIndicator"], true);
        assert_eq!(line.object["timeout"], 900);
    }

    #[cfg(unix)]
    #[test]
    fn prepared_fan_out_hides_the_original_command_and_preserves_every_status_property() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        let scratch = dir.path().join("scratch");
        std::fs::create_dir_all(&scratch).unwrap();
        let original = "printf 'private user status command\\n'";
        write_json(
            &project.join(".claude/settings.local.json"),
            json!({
                "statusLine":{
                    "type":"command",
                    "command":original,
                    "padding":7,
                    "refreshInterval":4,
                    "hideVimModeIndicator":true,
                    "timeout":750
                }
            }),
        );
        let prepared = prepare_with_paths(
            &launch_context(&project, scratch.clone()),
            SettingsPaths {
                lower_precedence: vec![project.join(".claude/settings.local.json")],
                managed: Vec::new(),
            },
        );
        let setting = prepared.setting.unwrap();

        assert_eq!(setting["padding"], 7);
        assert_eq!(setting["refreshInterval"], 4);
        assert_eq!(setting["hideVimModeIndicator"], true);
        assert_eq!(setting["timeout"], 750);
        assert!(!setting.to_string().contains(original));
        assert!(setting["command"].as_str().unwrap().contains(WRAPPER_NAME));

        let wrapper = std::fs::read_to_string(scratch.join(WRAPPER_NAME)).unwrap();
        assert!(!wrapper.contains(original));
        assert!(wrapper.contains("--statusline"));
        assert!(!wrapper.contains("private-token"));
        let original_script = scratch.join(ORIGINAL_NAME);
        assert!(std::fs::read_to_string(&original_script)
            .unwrap()
            .contains(original));
        assert_eq!(
            std::fs::metadata(original_script)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }

    #[test]
    fn an_unreadable_or_invalid_higher_scope_never_causes_a_lower_command_to_be_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("local.json");
        let user = dir.path().join("user.json");
        std::fs::write(&local, b"not json").unwrap();
        write_json(
            &user,
            json!({"statusLine":{"type":"command","command":"user"}}),
        );
        assert!(matches!(
            resolve_lower_settings(&[local, user]),
            LowerSettings::Unavailable
        ));
    }

    #[test]
    fn managed_status_line_and_managed_only_hooks_are_detected_without_copying_commands() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("managed-settings.json");
        let override_file = dir.path().join("20-policy.json");
        write_json(
            &base,
            json!({"statusLine":{"type":"command","command":"managed secret"}}),
        );
        write_json(&override_file, json!({"allowManagedHooksOnly":true}));
        assert_eq!(
            managed_status_line_blocked(&[base, override_file]),
            Some(true)
        );
    }

    #[test]
    fn official_status_payload_is_bounded_sanitised_and_keeps_exact_semantics() {
        let payload = json!({
            "session_id":"claude-session-1",
            "transcript_path":"/private/secret/transcript.jsonl",
            "cwd":"/private/repository",
            "model":{"id":"claude-opus-5\nforged","display_name":"Opus\u{001b}[2J"},
            "context_window":{
                "total_input_tokens":15500,
                "total_output_tokens":1200,
                "context_window_size":200000,
                "used_percentage":8.5,
                "remaining_percentage":91.5,
                "current_usage":{
                    "input_tokens":8500,
                    "output_tokens":1200,
                    "cache_creation_input_tokens":5000,
                    "cache_read_input_tokens":2000
                }
            },
            "effort":{"level":"high"},
            "thinking":{"enabled":true},
            "rate_limits":{
                "five_hour":{"used_percentage":23.5,"resets_at":1738425600},
                "seven_day":{"used_percentage":41.2,"resets_at":1738857600}
            }
        });
        let event = observation_event(&payload, &event_context()).unwrap();
        let EventKind::AgentRuntimeObserved { runtime } = &event.kind else {
            unreachable!()
        };
        let launch = runtime.launch.current.value().unwrap();
        assert_eq!(launch.model.as_deref(), Some("claude-opus-5 forged"));
        assert_eq!(launch.model_display_name.as_deref(), Some("Opus"));
        assert_eq!(launch.effort_level.as_deref(), Some("high"));
        assert_eq!(launch.thinking_enabled, Some(true));
        assert_eq!(runtime.launch.current.observed_at_ms(), Some(NOW));

        let context = runtime.context.value().unwrap();
        assert_eq!(context.measurement.amount, 16_700.0);
        assert_eq!(context.measurement.total, Some(200_000.0));
        assert_eq!(context.window_size_tokens, Some(200_000));
        assert_eq!(context.used_percentage, Some(8.5));
        assert_eq!(context.remaining_percentage, Some(91.5));
        assert_eq!(
            context.current_usage.as_ref().unwrap().input_tokens,
            Some(8_500)
        );
        assert_eq!(runtime.context.observed_at_ms(), Some(NOW));

        let quota = runtime.quota.value().unwrap();
        assert_eq!(quota.windows[0].label, "5h");
        assert_eq!(
            quota.windows[0].measurement.kind,
            UsageMeasurementKind::Remaining
        );
        assert_eq!(quota.windows[0].measurement.amount, 76.5);
        assert_eq!(quota.windows[0].measurement.total, Some(100.0));
        assert_eq!(quota.windows[0].resets_at_ms, Some(1_738_425_600_000));
        assert_eq!(quota.windows[1].label, "7d");
        assert_eq!(runtime.quota.observed_at_ms(), Some(NOW));

        let encoded = serde_json::to_string(&event).unwrap();
        assert!(!encoded.contains("transcript_path"));
        assert!(!encoded.contains("/private/"));
        assert!(!encoded.contains("rate_limits"));
        assert_eq!(event.raw, None);
    }

    #[test]
    fn invalid_percentages_effort_and_unsafe_identifiers_are_not_projected() {
        let payload = json!({
            "session_id":"--dangerously-skip-permissions",
            "model":{"id":"x".repeat(text::MAX_FIELD_CHARS + 20)},
            "context_window":{"used_percentage":101,"remaining_percentage":-1},
            "effort":{"level":"ultracode"},
            "rate_limits":{"five_hour":{"used_percentage":-0.1}}
        });
        let event = observation_event(&payload, &event_context()).unwrap();
        assert_eq!(event.agent.external_id, None);
        let EventKind::AgentRuntimeObserved { runtime } = event.kind else {
            unreachable!()
        };
        assert!(runtime
            .launch
            .current
            .value()
            .unwrap()
            .effort_level
            .is_none());
        assert!(matches!(runtime.context, Observable::Waiting));
        assert!(matches!(runtime.quota, Observable::Waiting));
    }
}
