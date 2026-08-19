//! OpenCode integration through its documented plugin event contract.
//!
//! OpenCode merges `OPENCODE_CONFIG_DIR` with global and project configuration,
//! and automatically loads JavaScript files from that directory's `plugins/`
//! folder. Turn writes one fire-and-forget observer there. A missing daemon or a
//! changed plugin contract can cost an event, never an OpenCode interaction.

use crate::adapter::{
    control_arguments, insert_control_arguments, AdapterError, AgentAdapter, Capabilities,
    EventContext, IntegrationLevel, LaunchContext, LaunchPermissionPosture, LaunchPlan,
    LaunchProfileDefinition, ResolvedLaunchProfile, AUTONOMOUS_PROFILE_ID, SAFE_PROFILE_ID,
};
use crate::{risk, text};
use serde_json::Value;
use std::path::Path;
use turn_core::event::{AgentRef, Confidence, EventKind, EventSource, Risk, TurnEvent};
use turn_core::model::LaunchConfiguration;

/// Version of OpenCode whose official schemas the fixture was recorded from.
pub const CONTRACT_VERSION: &str = "1.18.16";

const OPENCODE_AUTO: &str = "--auto";

const PLUGIN: &str = r#"export const TurnObserver = async () => {
  const send = (event) => {
    const url = process.env.TURN_HOOK_URL
    if (!url) return
    void fetch(url, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(event),
      signal: AbortSignal.timeout(250),
    }).catch(() => {})
  }
  return { event: async ({ event }) => send(event) }
}
"#;

#[derive(Debug, Default)]
pub struct OpenCodeAdapter;

impl OpenCodeAdapter {
    pub fn new() -> Self {
        Self
    }

    pub fn plugin_source(&self) -> &'static str {
        PLUGIN
    }

    fn fallback(
        ctx: &LaunchContext,
        args: Vec<String>,
        reason: impl std::fmt::Display,
    ) -> LaunchPlan {
        LaunchPlan {
            command: ctx.command.clone(),
            args,
            env: base_env(ctx),
            level: IntegrationLevel::Heuristic,
            note: format!(
                "OpenCode still launched, but its {} plugin contract could not be installed ({reason}). Turn is inferring state from terminal output.",
                CONTRACT_VERSION
            ),
        }
    }

    fn install(&self, ctx: &LaunchContext) -> std::io::Result<std::path::PathBuf> {
        let config_dir = ctx.scratch_dir.join("opencode-config");
        let plugins = config_dir.join("plugins");
        std::fs::create_dir_all(&plugins)?;
        restrict_directory(&ctx.scratch_dir);
        restrict_directory(&config_dir);
        restrict_directory(&plugins);
        write_private(&plugins.join("turn-observer.js"), PLUGIN.as_bytes())?;
        Ok(config_dir)
    }
}

fn resolve_opencode_profile(
    profile_id: &str,
    args: &[String],
) -> Result<ResolvedLaunchProfile, AdapterError> {
    let auto = control_arguments(args)
        .iter()
        .any(|arg| arg == OPENCODE_AUTO);
    match profile_id {
        SAFE_PROFILE_ID => {
            if auto {
                return Err(AdapterError::LaunchProfileConflict {
                    adapter_id: "opencode".to_string(),
                    profile_id: profile_id.to_string(),
                    detail: format!("the explicit {OPENCODE_AUTO} argument"),
                });
            }
            Ok(ResolvedLaunchProfile::safe("opencode", args))
        }
        AUTONOMOUS_PROFILE_ID => {
            let resolved = if auto {
                args.to_vec()
            } else {
                insert_control_arguments(args, [OPENCODE_AUTO.to_string()])
            };
            Ok(ResolvedLaunchProfile::autonomous(
                "opencode",
                LaunchPermissionPosture::AutoApproveUnlessDenied,
                resolved,
                vec![OPENCODE_AUTO.to_string()],
            ))
        }
        _ => Err(AdapterError::UnknownLaunchProfile {
            adapter_id: "opencode".to_string(),
            profile_id: profile_id.to_string(),
        }),
    }
}

impl AgentAdapter for OpenCodeAdapter {
    fn id(&self) -> &'static str {
        "opencode"
    }

    fn provider(&self) -> &'static str {
        "opencode"
    }

    fn executables(&self) -> &'static [&'static str] {
        &["opencode"]
    }

    fn observed_wrapper_path_suffixes(&self) -> &'static [&'static str] {
        &["node_modules/opencode-ai/bin/opencode"]
    }

    fn best_level(&self) -> IntegrationLevel {
        IntegrationLevel::Structured
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            turn_events: true,
            permission_events: true,
            subagent_events: true,
            resumable: true,
            usage_events: false,
            external_session_id: true,
        }
    }

    fn launch_profiles(&self) -> Vec<LaunchProfileDefinition> {
        vec![
            LaunchProfileDefinition::safe(
                "OpenCode keeps its configured permission policy in force.",
            ),
            LaunchProfileDefinition::autonomous(
                LaunchPermissionPosture::AutoApproveUnlessDenied,
                "OpenCode auto-approves permission requests, while explicit deny rules remain in force.",
            ),
        ]
    }

    fn resolve_launch_profile(
        &self,
        profile_id: &str,
        user_args: &[String],
    ) -> Result<ResolvedLaunchProfile, AdapterError> {
        resolve_opencode_profile(profile_id, user_args)
    }

    fn launch_configuration(
        &self,
        args: &[String],
        profile: &ResolvedLaunchProfile,
    ) -> LaunchConfiguration {
        let mut configuration = crate::launch_facts::base_launch_configuration(args, profile, true);
        if control_arguments(args)
            .iter()
            .any(|arg| arg == OPENCODE_AUTO)
        {
            configuration.approval_mode = Some("auto unless denied".into());
            if profile.role.is_none() {
                configuration.permission_mode = Some("Custom · auto-approve unless denied".into());
            }
        }
        configuration
    }

    fn launch_profile_is_grounded(&self, args: &[String], profile: &ResolvedLaunchProfile) -> bool {
        profile.role != Some(crate::LaunchProfileRole::Autonomous)
            || (profile.adapter_id == self.id()
                && profile.posture == LaunchPermissionPosture::AutoApproveUnlessDenied
                && control_arguments(args)
                    .iter()
                    .any(|arg| arg == OPENCODE_AUTO))
    }

    fn prepare(&self, ctx: &LaunchContext) -> Result<LaunchPlan, AdapterError> {
        let resolved_profile = self.resolve_context_launch_profile(ctx)?;
        let profile_args = resolved_profile.args;
        if control_arguments(&profile_args)
            .iter()
            .any(|arg| arg == "--pure")
        {
            return Ok(Self::fallback(
                ctx,
                profile_args,
                "--pure explicitly disables plugins",
            ));
        }
        let config_dir = match self.install(ctx) {
            Ok(path) => path,
            Err(error) => return Ok(Self::fallback(ctx, profile_args, error)),
        };
        let mut env = base_env(ctx);
        env.push((
            "OPENCODE_CONFIG_DIR".into(),
            config_dir.to_string_lossy().into_owned(),
        ));
        Ok(LaunchPlan {
            command: ctx.command.clone(),
            args: profile_args,
            env,
            level: IntegrationLevel::Heuristic,
            note: format!(
                "OpenCode {} observer installed in a merged scratch config directory. Output inference remains active until an authenticated plugin event proves the structured contract is live; user and project settings are untouched.",
                CONTRACT_VERSION
            ),
        })
    }

    fn resume_args(&self, external_id: &str) -> Option<Vec<String>> {
        let id = text::identifier(external_id)?;
        Some(vec!["--session".into(), id])
    }

    fn normalise(&self, payload: &Value, ctx: &EventContext) -> Vec<TurnEvent> {
        let Some(name) = payload.get("type").and_then(Value::as_str) else {
            return Vec::new();
        };
        let properties = payload.get("properties").unwrap_or(payload);
        let info = properties.get("info").unwrap_or(&Value::Null);
        let external_id = string(properties, &["sessionID", "sessionId"])
            .or_else(|| string(info, &["id"]))
            .and_then(text::identifier);
        let parent_id = string(info, &["parentID", "parentId"]).and_then(text::identifier);
        let provider = info
            .pointer("/model/providerID")
            .and_then(Value::as_str)
            .and_then(text::field)
            .unwrap_or_else(|| "opencode".into());
        let model = info
            .pointer("/model/id")
            .and_then(Value::as_str)
            .and_then(text::field);
        let agent = AgentRef {
            provider: Some(provider),
            tool: Some("opencode".into()),
            model: model.clone(),
            external_id: external_id.clone(),
        };
        let source = EventSource::Hook {
            tool: "opencode".into(),
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
            "session.created" if parent_id.is_some() => vec![make(EventKind::AgentSpawned {
                declared_name: string(info, &["title"]).and_then(text::field),
                agent_type: Some("opencode-session".into()),
                agent_id: external_id,
                task: string(info, &["title"]).map(|title| text::excerpt(title, 240)),
            })],
            "session.created" | "session.updated" => vec![make(EventKind::AgentStarted {
                tool: "opencode".into(),
                model,
                external_id,
            })],
            "session.status" => match properties.pointer("/status/type").and_then(Value::as_str) {
                Some("busy") => vec![make(EventKind::AgentTurnStarted {
                    prompt_excerpt: None,
                })],
                Some("idle") => vec![make(EventKind::AgentTurnCompleted {
                    last_message: None,
                    background_tasks: 0,
                })],
                _ => Vec::new(),
            },
            "permission.asked" => {
                let tool_name = string(properties, &["permission"]);
                let command = properties
                    .pointer("/metadata/command")
                    .and_then(Value::as_str);
                let too_long = command.is_some_and(|command| {
                    matches!(text::command(command), text::CommandText::TooLong)
                });
                let stored = command.and_then(|command| match text::command(command) {
                    text::CommandText::Complete(command) => Some(command),
                    text::CommandText::Empty | text::CommandText::TooLong => None,
                });
                let summary = if too_long {
                    text::COMMAND_TOO_LONG_SUMMARY.into()
                } else if let Some(command) = command {
                    format!("Run `{}`", text::excerpt(command, 120))
                } else if let Some(tool) = tool_name.and_then(text::field) {
                    format!("OpenCode requests {tool}")
                } else {
                    "OpenCode is waiting for permission".into()
                };
                vec![make(EventKind::AgentPermissionRequired {
                    summary,
                    command: stored,
                    tool_name: tool_name.and_then(text::field),
                    risk: if too_long {
                        Risk::High
                    } else {
                        risk::assess(tool_name, command)
                    },
                })]
            }
            "permission.replied" => vec![make(EventKind::AgentPermissionResolved {
                allowed: string(properties, &["reply"]) != Some("reject"),
            })],
            "question.asked" => {
                let question = properties
                    .pointer("/questions/0/question")
                    .and_then(Value::as_str)
                    .map(|question| text::excerpt(question, 240))
                    .filter(|question| !question.is_empty())
                    .unwrap_or_else(|| "OpenCode needs your input".into());
                vec![make(EventKind::AgentQuestionAsked { question })]
            }
            "question.replied" | "question.rejected" => {
                vec![make(EventKind::AgentTurnStarted {
                    prompt_excerpt: None,
                })]
            }
            "session.error" => vec![make(EventKind::AgentFailed {
                reason: error_message(properties)
                    .unwrap_or_else(|| "OpenCode reported a session failure".into()),
            })],
            "session.deleted" if parent_id.is_some() => {
                vec![make(EventKind::AgentSubagentStopped {
                    agent_id: external_id,
                })]
            }
            "session.deleted" => vec![make(EventKind::AgentIdle)],
            _ => Vec::new(),
        }
    }
}

fn string<'a>(value: &'a Value, names: &[&str]) -> Option<&'a str> {
    names
        .iter()
        .find_map(|name| value.get(*name).and_then(Value::as_str))
}

fn error_message(properties: &Value) -> Option<String> {
    string(properties, &["message", "error"])
        .or_else(|| properties.pointer("/error/message").and_then(Value::as_str))
        .or_else(|| {
            properties
                .pointer("/error/data/message")
                .and_then(Value::as_str)
        })
        .map(|message| text::excerpt(message, 240))
        .filter(|message| !message.is_empty())
}

fn base_env(ctx: &LaunchContext) -> Vec<(String, String)> {
    vec![
        ("TURN_HOOK_URL".into(), ctx.endpoint.url()),
        ("TURN_SESSION_ID".into(), ctx.session_id.to_string()),
        ("TURN_NODE_ID".into(), ctx.node_id.to_string()),
    ]
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
