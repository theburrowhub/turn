//! Gemini CLI integration through its documented command-hook contract.
//!
//! Turn injects a lowest-precedence settings file through
//! `GEMINI_CLI_SYSTEM_DEFAULTS_PATH`. Gemini merges hook arrays across settings
//! layers, so the user's global and project settings (including their own hooks)
//! remain in force. The launch starts with output inference and is promoted to a
//! structured integration only after an authenticated callback is observed.

use crate::adapter::{
    AdapterError, AgentAdapter, Capabilities, EventContext, IntegrationLevel, LaunchContext,
    LaunchPlan,
};
use crate::{risk, text};
use serde_json::{json, Value};
use std::path::Path;
use turn_core::event::{AgentRef, Confidence, EventKind, EventSource, Risk, TurnEvent};

/// Version of Gemini CLI whose documented hook contract the fixture covers.
pub const CONTRACT_VERSION: &str = "0.46.0";

const HOOK_TIMEOUT_MS: u64 = 3_000;
const EVENTS: &[(&str, &str)] = &[
    ("SessionStart", "*"),
    ("BeforeAgent", "*"),
    ("AfterAgent", "*"),
    ("BeforeModel", "*"),
    ("BeforeTool", "ask_user"),
    ("Notification", "*"),
    ("SessionEnd", "*"),
];

#[derive(Debug, Default)]
pub struct GeminiCliAdapter;

impl GeminiCliAdapter {
    pub fn new() -> Self {
        Self
    }

    /// The exact settings fragment Turn asks Gemini CLI to merge.
    pub fn settings_document(&self, helper: &Path) -> Value {
        // Gemini parses successful hook stdout as JSON. turn-hook is deliberately
        // silent, so append the smallest valid response after it has consumed the
        // callback. A missing daemon still yields `{}` and never delays the CLI.
        let command = format!("{}; printf '{{}}'", shell_quote(&helper.to_string_lossy()));
        let mut hooks = serde_json::Map::new();
        for (event, matcher) in EVENTS {
            hooks.insert(
                (*event).to_string(),
                json!([{
                    "matcher": matcher,
                    "hooks": [{
                        "name": "turn-observer",
                        "type": "command",
                        "command": command,
                        "timeout": HOOK_TIMEOUT_MS,
                    }]
                }]),
            );
        }
        json!({ "hooks": hooks })
    }

    fn fallback(ctx: &LaunchContext, reason: impl std::fmt::Display) -> LaunchPlan {
        LaunchPlan {
            command: ctx.command.clone(),
            args: ctx.user_args.clone(),
            env: base_env(ctx),
            level: IntegrationLevel::Heuristic,
            note: format!(
                "Gemini CLI still launched, but its {} hook contract could not be installed ({reason}). Turn is inferring state from terminal output.",
                CONTRACT_VERSION
            ),
        }
    }

    fn install(
        &self,
        ctx: &LaunchContext,
        helper: &Path,
    ) -> Result<std::path::PathBuf, AdapterError> {
        std::fs::create_dir_all(&ctx.scratch_dir)?;
        restrict_directory(&ctx.scratch_dir);
        let settings = ctx.scratch_dir.join("gemini-system-defaults.json");
        let contents = serde_json::to_vec_pretty(&self.settings_document(helper))?;
        write_private(&settings, &contents)?;
        Ok(settings)
    }
}

impl AgentAdapter for GeminiCliAdapter {
    fn id(&self) -> &'static str {
        "gemini-cli"
    }

    fn provider(&self) -> &'static str {
        "google"
    }

    fn executables(&self) -> &'static [&'static str] {
        &["gemini"]
    }

    fn best_level(&self) -> IntegrationLevel {
        IntegrationLevel::Structured
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            turn_events: true,
            permission_events: true,
            subagent_events: false,
            resumable: true,
            usage_events: false,
            external_session_id: true,
        }
    }

    fn prepare(&self, ctx: &LaunchContext) -> Result<LaunchPlan, AdapterError> {
        let Some(helper) = ctx.endpoint.helper_path.as_deref() else {
            return Ok(Self::fallback(ctx, "the turn-hook helper was not found"));
        };
        let settings = match self.install(ctx, helper) {
            Ok(settings) => settings,
            Err(error) => return Ok(Self::fallback(ctx, error)),
        };

        let mut env = base_env(ctx);
        env.push((
            "GEMINI_CLI_SYSTEM_DEFAULTS_PATH".into(),
            settings.to_string_lossy().into_owned(),
        ));
        Ok(LaunchPlan {
            command: ctx.command.clone(),
            args: ctx.user_args.clone(),
            env,
            // Configuration is not evidence that the user left hooks enabled.
            // The daemon promotes this as soon as the first callback arrives.
            level: IntegrationLevel::Heuristic,
            note: format!(
                "Gemini CLI {} hooks are installed as mergeable system defaults. Output inference remains active until the first authenticated callback proves the contract is live; user and project settings are untouched.",
                CONTRACT_VERSION
            ),
        })
    }

    fn resume_args(&self, external_id: &str) -> Option<Vec<String>> {
        let id = text::identifier(external_id)?;
        Some(vec!["--resume".into(), id])
    }

    fn normalise(&self, payload: &Value, ctx: &EventContext) -> Vec<TurnEvent> {
        let Some(name) = payload.get("hook_event_name").and_then(Value::as_str) else {
            return Vec::new();
        };
        let external_id = payload
            .get("session_id")
            .and_then(Value::as_str)
            .and_then(text::identifier);
        let model = payload
            .pointer("/llm_request/model")
            .or_else(|| payload.get("model"))
            .and_then(Value::as_str)
            .and_then(text::field);
        let agent = AgentRef {
            provider: Some("google".into()),
            tool: Some("gemini-cli".into()),
            model: model.clone(),
            external_id: external_id.clone(),
        };
        let source = EventSource::Hook {
            tool: "gemini-cli".into(),
            event_name: text::excerpt(name, 64),
        };
        let make = |kind| {
            TurnEvent::new(
                ctx.session_id.clone(),
                kind,
                source.clone(),
                Confidence::Explicit,
                ctx.timestamp_ms,
            )
            .with_node(ctx.node_id.clone())
            .with_agent(agent.clone())
            .with_raw(text::raw_for_storage(payload))
        };

        match name {
            "SessionStart" => vec![make(EventKind::AgentStarted {
                tool: "gemini-cli".into(),
                model,
                external_id,
            })],
            "BeforeAgent" => vec![make(EventKind::AgentTurnStarted {
                prompt_excerpt: payload
                    .get("prompt")
                    .and_then(Value::as_str)
                    .map(|prompt| text::excerpt(prompt, 160)),
            })],
            "AfterAgent" => vec![make(EventKind::AgentTurnCompleted {
                last_message: payload
                    .get("prompt_response")
                    .and_then(Value::as_str)
                    .map(|message| text::excerpt(message, 240)),
                background_tasks: 0,
            })],
            // BeforeModel is the documented place Gemini exposes its selected
            // model. AgentStarted doubles as a safe metadata refresh; the reducer
            // preserves an already-active turn.
            "BeforeModel" if model.is_some() => vec![make(EventKind::AgentStarted {
                tool: "gemini-cli".into(),
                model,
                external_id,
            })],
            "BeforeTool"
                if payload.get("tool_name").and_then(Value::as_str) == Some("ask_user") =>
            {
                let question = payload
                    .pointer("/tool_input/questions/0/question")
                    .and_then(Value::as_str)
                    .map(|question| text::excerpt(question, 240))
                    .filter(|question| !question.is_empty())
                    .unwrap_or_else(|| "Gemini needs your input".into());
                vec![make(EventKind::AgentQuestionAsked { question })]
            }
            "Notification"
                if payload.get("notification_type").and_then(Value::as_str)
                    == Some("ToolPermission") =>
            {
                let tool_name = payload
                    .pointer("/details/tool_name")
                    .or_else(|| payload.pointer("/details/toolName"))
                    .and_then(Value::as_str);
                let command = payload.pointer("/details/command").and_then(Value::as_str);
                let display_command = command.and_then(|command| match text::command(command) {
                    text::CommandText::Complete(command) => Some(command),
                    text::CommandText::Empty | text::CommandText::TooLong => None,
                });
                let too_long = command.is_some_and(|command| {
                    matches!(text::command(command), text::CommandText::TooLong)
                });
                vec![make(EventKind::AgentPermissionRequired {
                    summary: if too_long {
                        text::COMMAND_TOO_LONG_SUMMARY.into()
                    } else {
                        payload
                            .get("message")
                            .and_then(Value::as_str)
                            .map(|message| text::excerpt(message, 240))
                            .filter(|message| !message.is_empty())
                            .unwrap_or_else(|| "Gemini is waiting for permission".into())
                    },
                    command: display_command,
                    tool_name: tool_name.and_then(text::field),
                    risk: if too_long {
                        Risk::High
                    } else {
                        risk::assess(tool_name, command)
                    },
                })]
            }
            "SessionEnd" => vec![make(EventKind::AgentIdle)],
            _ => Vec::new(),
        }
    }
}

fn base_env(ctx: &LaunchContext) -> Vec<(String, String)> {
    vec![
        ("TURN_HOOK_URL".into(), ctx.endpoint.url()),
        ("TURN_SESSION_ID".into(), ctx.session_id.to_string()),
        ("TURN_NODE_ID".into(), ctx.node_id.to_string()),
    ]
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn write_private(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(contents)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        file.sync_all()
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, contents)
    }
}

fn restrict_directory(dir: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
}
