//! Claude Code integration.
//!
//! Claude Code ships a hook engine with an event for nearly everything Turn cares
//! about, so this adapter reaches [`IntegrationLevel::Structured`]: turn
//! boundaries, pending permissions and — the one that matters most for the
//! hierarchy — real lifecycle and tool callbacks. No terminal-output parsing.
//!
//! The configuration is injected with `--settings`, pointing at a file Turn owns
//! inside the session's scratch directory. The user's own `~/.claude/settings.json`
//! and `.claude/settings.json` are read as usual and never modified: `--settings`
//! adds a layer, it does not replace them.
//!
//! The same isolated layer pins Agent Teams to Claude Code's `in-process` teammate
//! backend. A daemon launched from iTerm inherits `ITERM_SESSION_ID`, and Claude's
//! automatic or user-configured `iterm2` backend would otherwise create teammates in
//! another application, outside Turn's hierarchy and attention model. In-process
//! teammates remain owned by the Claude process in Turn's PTY. Agent Teams do not
//! use the classic `SubagentStart` contract in Claude Code 2.1.222, so Turn also
//! observes the structured `PostToolUse(Agent)` result that declares a teammate.
//!
//! That file holds the node's hook URL, and the token in it is what stops another
//! account on the machine forging "your agent is waiting for you" for this
//! session. So it is written `0600` into a `0700` directory, and the URL never
//! travels in argv — `/proc/<pid>/cmdline` is world-readable on Linux.
//!
//! Two things can stop that being the whole story, and both are reported instead of
//! papered over. `SessionStart` is only deliverable through the `turn-hook` helper,
//! so without it the agent's own session id is never learned and the launch is not
//! `Structured`. And `--settings` is a single slot: a user who passes their own owns
//! it, so Turn adds nothing and says what that costs.

use crate::adapter::{
    control_arguments, insert_control_arguments, AdapterError, AgentAdapter, Capabilities,
    EventContext, IntegrationLevel, LaunchContext, LaunchPermissionPosture, LaunchPlan,
    LaunchProfileDefinition, ResolvedLaunchProfile, AUTONOMOUS_PROFILE_ID, SAFE_PROFILE_ID,
};
use crate::risk;
use crate::text::{self, excerpt};
use serde_json::{json, Value};
use turn_core::event::{AgentRef, Confidence, EventKind, EventSource, Risk, TurnEvent};
use turn_core::model::LaunchConfiguration;
use turn_core::state::AwaitingReason;

/// How hook callbacks reach Turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HookTransport {
    /// Claude Code posts straight to Turn's local server. No process spawn per
    /// event, which matters when a busy agent fires dozens of tool hooks.
    #[default]
    Http,
    /// Claude Code runs the `turn-hook` helper, which posts on its behalf. The
    /// fallback for builds whose hook engine lacks HTTP handlers.
    Helper,
}

/// The hook events Turn subscribes to.
///
/// Deliberately not "all of them": each subscription costs the agent a callback,
/// and Turn only needs the ones that change a state it renders.
const SUBSCRIBED_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PermissionRequest",
    "PermissionDenied",
    "Notification",
    "PostToolUse",
    "SubagentStart",
    "SubagentStop",
    "Stop",
    "StopFailure",
    "SessionEnd",
];

/// The only high-frequency tool hook Turn installs is narrowed at Claude's hook
/// engine. Receiving every tool completion just to discard almost all of them would
/// add avoidable callbacks to active coding sessions.
fn matcher_for(event: &str) -> &'static str {
    match event {
        "PostToolUse" => "Agent",
        _ => "*",
    }
}

/// Events Claude Code refuses to deliver over HTTP.
///
/// Observed on 2.1.221: the hook engine filters HTTP handlers for `SessionStart`
/// and `Setup` before dispatch, logging
/// `Skipping HTTP hook … — HTTP hooks are not supported for SessionStart`.
/// Nothing surfaces to the user, so an HTTP-only subscription loses the event
/// silently — which is how Turn came to never learn the agent's own session id.
/// These fall back to the helper process even when the transport is HTTP.
///
/// Only `SessionStart` is listed because `Setup` is not among the events Turn
/// subscribes to; add it here too if that ever changes.
const EVENTS_WITHOUT_HTTP_DELIVERY: &[&str] = &["SessionStart"];

/// Notification types that report progress rather than ask for something.
///
/// The catch-all below treats an unrecognised notification as "the agent wants
/// you", which is the safe default for a demand but plain wrong for these: they
/// announce that something *finished*. Left in the catch-all they would each
/// raise a false attention demand.
const NON_DEMANDING_NOTIFICATIONS: &[&str] = &[
    "auth_success",
    "agent_completed",
    "computer_use_enter",
    "computer_use_exit",
    "elicitation_complete",
    "elicitation_response",
];

/// Seconds Claude Code waits for Turn before giving up on a callback.
///
/// Short on purpose: if the daemon is gone, the agent must carry on rather than
/// stall on every event. A local POST answers in well under a millisecond.
const HOOK_TIMEOUT_SECONDS: u32 = 3;

/// Claude Code 2.1.222's settings value for teammates hosted by the parent process.
///
/// This belongs in Turn's per-launch settings document, not in the user's global
/// configuration: running Claude directly in iTerm should keep doing whatever the
/// user configured there.
const TEAMMATE_MODE_IN_PROCESS: &str = "in-process";

pub struct ClaudeCodeAdapter {
    transport: HookTransport,
}

impl Default for ClaudeCodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl ClaudeCodeAdapter {
    pub fn new() -> Self {
        Self {
            transport: HookTransport::default(),
        }
    }

    pub fn with_transport(transport: HookTransport) -> Self {
        Self { transport }
    }

    /// Builds the settings document injected via `--settings`.
    pub fn settings_document(&self, ctx: &LaunchContext) -> Value {
        self.subscriptions(ctx).0
    }

    /// The settings document, plus the events it could not find a transport for.
    ///
    /// A subscription is only written when there is something real to point it at.
    /// A `command` handler naming a helper Turn never located is a promise the hook
    /// engine cannot keep: the event is lost either way, and writing it would hide
    /// the loss from the level this launch goes on to report.
    fn subscriptions(&self, ctx: &LaunchContext) -> (Value, Vec<&'static str>) {
        let mut hooks = serde_json::Map::new();
        let mut undeliverable = Vec::new();

        for event in SUBSCRIBED_EVENTS {
            let over_http = self.transport == HookTransport::Http
                && !EVENTS_WITHOUT_HTTP_DELIVERY.contains(event);
            let handler = if over_http {
                Some(json!({
                    "type": "http",
                    "url": ctx.endpoint.url(),
                    "timeout": HOOK_TIMEOUT_SECONDS,
                }))
            } else {
                self.helper_handler(ctx)
            };
            match handler {
                Some(handler) => {
                    hooks.insert(
                        (*event).to_string(),
                        json!([{ "matcher": matcher_for(event), "hooks": [handler] }]),
                    );
                }
                None => undeliverable.push(*event),
            }
        }

        (
            json!({
                "hooks": hooks,
                "teammateMode": TEAMMATE_MODE_IN_PROCESS,
            }),
            undeliverable,
        )
    }

    /// A handler that shells out to `turn-hook`, when there is a `turn-hook` to
    /// shell out to.
    ///
    /// `None` rather than a bare `turn-hook` command: the helper is looked for
    /// beside `turnd` and not on `PATH`, so a missing path means Turn does not know
    /// where it is, and inventing a name for the agent to run would only move the
    /// failure somewhere nobody sees it.
    fn helper_handler(&self, ctx: &LaunchContext) -> Option<Value> {
        let helper = ctx.endpoint.helper_path.as_ref()?;
        // No `--url` argument on purpose: the helper reads `TURN_HOOK_URL`, which
        // it inherits from the agent Turn launched. The URL contains this node's
        // token, and an argument would publish it to every process on the machine
        // that can read `/proc/<pid>/cmdline`.
        json!({
            "type": "command",
            "command": helper.to_string_lossy().to_string(),
            "timeout": HOOK_TIMEOUT_SECONDS,
        })
        .into()
    }

    /// The integration a launch can actually demonstrate, and what to tell the user.
    ///
    /// `SessionStart` is the one event Claude Code refuses over HTTP and the one
    /// that carries the agent's own session id, so losing it costs resuming — which
    /// is not a `Structured` integration however well the rest of the subscription
    /// works.
    fn achieved(&self, undeliverable: &[&'static str]) -> (IntegrationLevel, String) {
        if undeliverable.is_empty() {
            return (
                IntegrationLevel::Structured,
                format!(
                    "Hooks injected via --settings ({}). Your own settings files are untouched.",
                    match self.transport {
                        HookTransport::Http => "HTTP callbacks",
                        HookTransport::Helper => "turn-hook helper",
                    }
                ),
            );
        }
        if undeliverable.len() == SUBSCRIBED_EVENTS.len() {
            return (
                IntegrationLevel::GenericTerminal,
                "The turn-hook helper was not found, so Claude Code has no way to \
                 report back on this transport. Turn will show the terminal but \
                 cannot detect turns or permissions."
                    .to_string(),
            );
        }
        (
            IntegrationLevel::Wrapper,
            format!(
                "Hooks injected via --settings (HTTP callbacks), but the turn-hook helper \
                 was not found and Claude Code will not deliver {} over HTTP: turn \
                 boundaries, permissions and subagents are reported, the agent's own \
                 session id is not, so this session cannot be resumed. Your own settings \
                 files are untouched.",
                undeliverable.join(", ")
            ),
        )
    }
}

/// Whether the user passed a `--settings` of their own.
///
/// Claude Code takes one settings file, so Turn's and the user's cannot both be in
/// effect: whichever the parser prefers, the other is discarded without a word.
/// Turn does not get to be the one that wins. Replacing a user's configuration to
/// make Turn's own detection work would be Turn deciding something on their behalf,
/// and losing Turn's hooks while still claiming `Structured` would be worse.
/// This is deliberately conservative across separate, `=`, repeated, malformed
/// and inline-JSON occurrences in the option-bearing prefix. A `--settings` after
/// the first exact `--` is prompt text and must never disable integration or be
/// reinterpreted as a control.
fn user_supplied_settings(args: &[String]) -> bool {
    control_arguments(args)
        .iter()
        .any(|arg| arg == "--settings" || arg.starts_with("--settings="))
}

fn claude_permission_modes(args: &[String]) -> Vec<Option<&str>> {
    let controls = control_arguments(args);
    controls
        .iter()
        .enumerate()
        .filter_map(|(index, arg)| {
            if arg == "--permission-mode" {
                Some(controls.get(index + 1).map(String::as_str))
            } else {
                arg.strip_prefix("--permission-mode=").map(Some)
            }
        })
        .collect()
}

fn claude_skips_permissions(args: &[String]) -> bool {
    control_arguments(args)
        .iter()
        .any(|arg| arg == "--dangerously-skip-permissions")
}

fn observed_claude_permission_mode(args: &[String]) -> Option<&str> {
    let mode = claude_permission_modes(args).into_iter().last().flatten()?;
    [
        "default",
        "acceptEdits",
        "plan",
        "dontAsk",
        "bypassPermissions",
        "delegate",
    ]
    .contains(&mode)
    .then_some(mode)
}

fn resolve_claude_profile(
    profile_id: &str,
    args: &[String],
) -> Result<ResolvedLaunchProfile, AdapterError> {
    let skip_flag = claude_skips_permissions(args);
    let permission_modes = claude_permission_modes(args);
    let bypass_mode = permission_modes.contains(&Some("bypassPermissions"));

    match profile_id {
        SAFE_PROFILE_ID => {
            if skip_flag || !permission_modes.is_empty() {
                return Err(AdapterError::LaunchProfileConflict {
                    adapter_id: "claude-code".to_string(),
                    profile_id: profile_id.to_string(),
                    detail: "an explicit Claude Code permission policy argument".to_string(),
                });
            }
            Ok(ResolvedLaunchProfile::safe("claude-code", args))
        }
        AUTONOMOUS_PROFILE_ID => {
            if permission_modes
                .iter()
                .any(|mode| *mode != Some("bypassPermissions"))
            {
                return Err(AdapterError::LaunchProfileConflict {
                    adapter_id: "claude-code".to_string(),
                    profile_id: profile_id.to_string(),
                    detail: "the explicit --permission-mode argument".to_string(),
                });
            }
            let effective_flag = if skip_flag {
                "--dangerously-skip-permissions"
            } else if bypass_mode {
                "--permission-mode"
            } else {
                "--dangerously-skip-permissions"
            };
            let resolved = if skip_flag || bypass_mode {
                args.to_vec()
            } else {
                insert_control_arguments(args, ["--dangerously-skip-permissions".to_string()])
            };
            Ok(ResolvedLaunchProfile::autonomous(
                "claude-code",
                LaunchPermissionPosture::BypassPermissions,
                resolved,
                vec![effective_flag.to_string()],
            ))
        }
        _ => Err(AdapterError::UnknownLaunchProfile {
            adapter_id: "claude-code".to_string(),
            profile_id: profile_id.to_string(),
        }),
    }
}

impl AgentAdapter for ClaudeCodeAdapter {
    fn id(&self) -> &'static str {
        "claude-code"
    }

    fn provider(&self) -> &'static str {
        "anthropic"
    }

    fn executables(&self) -> &'static [&'static str] {
        &["claude"]
    }

    fn observed_wrapper_path_suffixes(&self) -> &'static [&'static str] {
        &[
            "node_modules/@anthropic-ai/claude-code/cli-wrapper.cjs",
            "node_modules/@anthropic-ai/claude-code/cli.js",
        ]
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
            // Stop identifies the provider transcript, whose bounded tail gives
            // Turn exact input-context consumption after each completed turn.
            usage_events: true,
            external_session_id: true,
        }
    }

    fn launch_profiles(&self) -> Vec<LaunchProfileDefinition> {
        vec![
            LaunchProfileDefinition::safe(
                "Claude Code keeps its normal permission checks in force.",
            ),
            LaunchProfileDefinition::autonomous(
                LaunchPermissionPosture::BypassPermissions,
                "Claude Code skips permission checks for this launch.",
            ),
        ]
    }

    fn resolve_launch_profile(
        &self,
        profile_id: &str,
        user_args: &[String],
    ) -> Result<ResolvedLaunchProfile, AdapterError> {
        resolve_claude_profile(profile_id, user_args)
    }

    fn launch_configuration(
        &self,
        args: &[String],
        profile: &ResolvedLaunchProfile,
    ) -> LaunchConfiguration {
        let mut configuration =
            crate::launch_facts::base_launch_configuration(args, profile, false);
        if profile.role.is_none() {
            let explicit = observed_claude_permission_mode(args);
            if claude_skips_permissions(args) || explicit == Some("bypassPermissions") {
                configuration.permission_mode = Some("Custom · bypass permissions".into());
            } else if let Some(mode) = explicit {
                configuration.permission_mode = Some(format!("Custom · {mode}"));
            }
        }
        configuration
    }

    fn launch_profile_is_grounded(&self, args: &[String], profile: &ResolvedLaunchProfile) -> bool {
        if profile.role != Some(crate::LaunchProfileRole::Autonomous) {
            return true;
        }
        if profile.adapter_id != self.id()
            || profile.posture != LaunchPermissionPosture::BypassPermissions
        {
            return false;
        }
        let modes = claude_permission_modes(args);
        let modes_are_bypass = modes.iter().all(|mode| *mode == Some("bypassPermissions"));
        modes_are_bypass
            && (claude_skips_permissions(args) || modes.contains(&Some("bypassPermissions")))
    }

    fn prepare(&self, ctx: &LaunchContext) -> Result<LaunchPlan, AdapterError> {
        let resolved_profile = self.resolve_context_launch_profile(ctx)?;
        let user_args = resolved_profile.args;
        std::fs::create_dir_all(&ctx.scratch_dir)?;
        let settings_path = ctx.scratch_dir.join("claude-hooks.json");
        // The directory too: the file name is fixed and the path travels in argv,
        // so a readable directory is as good as a readable file.
        restrict_directory(&ctx.scratch_dir);
        let (mut document, undeliverable) = self.subscriptions(ctx);

        let env = vec![
            ("TURN_SESSION_ID".into(), ctx.session_id.to_string()),
            ("TURN_NODE_ID".into(), ctx.node_id.to_string()),
            // The helper transport reads its destination here rather than from an
            // argument. Argv is world-readable on Linux (`/proc/pid/cmdline`), and
            // the URL carries the session's token; an environment variable is at
            // least restricted to the same user.
            ("TURN_HOOK_URL".into(), ctx.endpoint.url()),
            // The status-line fan-out reads this from the environment. The
            // authenticated URL never appears in the wrapper command or argv.
            ("TURN_STATUSLINE_URL".into(), ctx.endpoint.status_line_url()),
        ];

        // The user's own `--settings` stands, and the file Turn wrote is named so
        // they can merge it if they want Turn's detection as well.
        if user_supplied_settings(&user_args) {
            write_private(&settings_path, &serde_json::to_vec_pretty(&document)?)?;
            return Ok(LaunchPlan {
                command: ctx.command.clone(),
                args: user_args,
                env,
                level: IntegrationLevel::GenericTerminal,
                note: format!(
                    "You passed your own --settings and Claude Code takes only one, so \
                     Turn added none: your configuration is what runs, and nothing \
                     reports back. Turn's hook configuration is at {} if you want to \
                     merge it into yours.",
                    settings_path.display()
                ),
            });
        }

        // Claude Code gives this `--settings` layer priority over local, project
        // and user settings. Preserve their effective status line through a
        // private fan-out before adding Turn's higher-priority value.
        let status_line = crate::claude_status::prepare(ctx);
        if let Some(setting) = status_line.setting {
            document
                .as_object_mut()
                .expect("Turn's settings document is always an object")
                .insert("statusLine".into(), setting);
        }
        write_private(&settings_path, &serde_json::to_vec_pretty(&document)?)?;

        // Provider controls belong before the first option terminator. Everything
        // from `--` onward is the operator's literal prompt and remains untouched.
        // A real CLI-owned settings occurrence in the option prefix was refused
        // above rather than quietly shadowed.
        let args = insert_control_arguments(
            &user_args,
            [
                "--settings".to_string(),
                settings_path.to_string_lossy().to_string(),
            ],
        );

        let (level, mut note) = self.achieved(&undeliverable);
        note.push(' ');
        note.push_str(status_line.note);
        Ok(LaunchPlan {
            command: ctx.command.clone(),
            args,
            env,
            level,
            note,
        })
    }

    /// `claude --resume <session_id>` continues the conversation that id names.
    ///
    /// The id is the `session_id` every hook payload carries, which is why the
    /// adapter records it on `SessionStart`: without it a restore can only start a
    /// new conversation, and the user loses the context they were mid-way through.
    fn resume_args(&self, external_id: &str) -> Option<Vec<String>> {
        let id = external_id.trim();
        // A blank or malformed id would turn into `--resume` with no value, which
        // makes Claude Code open its own session picker — an interactive prompt
        // nobody asked for, in a pane that was supposed to come back as it was.
        if id.is_empty() || id.contains(char::is_whitespace) {
            return None;
        }
        Some(vec!["--resume".to_string(), id.to_string()])
    }

    fn normalise(&self, payload: &Value, ctx: &EventContext) -> Vec<TurnEvent> {
        let Some(event_name) = payload.get("hook_event_name").and_then(Value::as_str) else {
            return Vec::new();
        };

        // `model` is optional in Claude Code's own hook schema and absent from
        // every payload captured on 2.1.221, including `SessionStart` — the one
        // event whose schema declares it. So the model stays unknown unless some
        // future release starts sending it, and nothing here may assume it.
        let agent = AgentRef {
            provider: Some("anthropic".into()),
            tool: Some("claude-code".into()),
            model: payload
                .get("model")
                .and_then(Value::as_str)
                .and_then(text::field),
            external_id: payload
                .get("agent_id")
                .and_then(Value::as_str)
                .and_then(text::identifier),
        };
        // The event name reaches log lines and the event panel, and it is a string
        // the sender chose. An unrecognised one is dropped further down, but the
        // recognised names go through the same filter so nothing can dress up as a
        // second log record on the way.
        let source = EventSource::Hook {
            tool: "claude-code".into(),
            event_name: excerpt(event_name, 64),
        };

        let make = |kind: EventKind| -> TurnEvent {
            TurnEvent::new(
                ctx.session_id.clone(),
                kind,
                source.clone(),
                Confidence::Explicit,
                ctx.timestamp_ms,
            )
            .with_node(ctx.node_id.clone())
            .with_agent(agent.clone())
        };

        // Some callbacks arrive on the parent's hook connection while describing
        // a worker. Keep the known parent as a correlation anchor, but leave the
        // subject empty: the daemon owns the live tree and is the only layer that
        // can honestly decide whether there is one possible child, several, or
        // none. Assigning `ctx.node_id` here would silently turn a worker demand
        // into a main-agent demand.
        let make_descendant = |kind: EventKind| -> TurnEvent {
            TurnEvent::new(
                ctx.session_id.clone(),
                kind,
                source.clone(),
                Confidence::Explicit,
                ctx.timestamp_ms,
            )
            .with_parent(ctx.node_id.clone())
            .with_agent(agent.clone())
        };

        // Validated, not merely copied: this id is handed back to the tool as an
        // argument when the user resumes the session.
        let external_id = payload
            .get("session_id")
            .and_then(Value::as_str)
            .and_then(text::identifier);

        match event_name {
            "SessionStart" => vec![make(EventKind::AgentStarted {
                tool: "claude-code".into(),
                model: agent.model.clone(),
                external_id,
            })],

            // The user answered, so a new turn has begun. This is also what
            // clears a pending permission from the attention queue.
            //
            // The field is `prompt`; `user_prompt` is accepted as well because
            // the published documentation names it that way and the two may yet
            // diverge between releases.
            "UserPromptSubmit" => vec![make_descendant(EventKind::AgentTurnStarted {
                prompt_excerpt: payload
                    .get("prompt")
                    .or_else(|| payload.get("user_prompt"))
                    .and_then(Value::as_str)
                    .map(|p| excerpt(p, 160)),
            })],

            "PermissionRequest" => {
                let tool_name = payload.get("tool_name").and_then(Value::as_str);
                let command = payload
                    .get("tool_input")
                    .and_then(|input| input.get("command"))
                    .and_then(Value::as_str);
                // Rated on the command as it arrived, in full. Sanitising or
                // shortening it first could hide the very fragment that makes it
                // dangerous.
                let mut risk = risk::assess(tool_name, command);
                let display_command = command.map(text::command);
                let command_too_long = matches!(&display_command, Some(text::CommandText::TooLong));
                let stored_command = match display_command {
                    Some(text::CommandText::Complete(command)) => Some(command),
                    Some(text::CommandText::TooLong) => {
                        // A command that cannot be reviewed faithfully is high risk
                        // regardless of what the recogniser found in its full tail.
                        risk = Risk::High;
                        None
                    }
                    Some(text::CommandText::Empty) | None => None,
                };
                let summary = if command_too_long {
                    text::COMMAND_TOO_LONG_SUMMARY.to_string()
                } else {
                    permission_summary(tool_name, command, payload)
                };
                vec![make(EventKind::AgentPermissionRequired {
                    summary,
                    command: stored_command,
                    tool_name: tool_name.and_then(text::field),
                    risk,
                })]
            }

            "PermissionDenied" => {
                vec![make(EventKind::AgentPermissionResolved { allowed: false })]
            }

            // The richest signal Claude Code sends: it says explicitly *why* it
            // wants the user.
            "Notification" => {
                let notification_type = payload
                    .get("notification_type")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let message = payload
                    .get("message")
                    .and_then(Value::as_str)
                    .map(|message| excerpt(message, 240));
                match notification_type {
                    "permission_prompt" => {
                        vec![make(EventKind::AgentPermissionRequired {
                            summary: message
                                .clone()
                                .filter(|message| !message.is_empty())
                                .unwrap_or_else(|| "Claude is waiting for permission".to_string()),
                            command: None,
                            tool_name: payload
                                .get("tool_name")
                                .and_then(Value::as_str)
                                .and_then(text::field),
                            risk: risk::assess(
                                payload.get("tool_name").and_then(Value::as_str),
                                None,
                            ),
                        })]
                    }
                    // Worker callbacks use the parent's hook endpoint and do not
                    // consistently carry `agent_id`. Preserve that uncertainty for
                    // tree-aware correlation instead of blaming the parent.
                    "worker_permission_prompt" => {
                        vec![make_descendant(EventKind::AgentPermissionRequired {
                            summary: message
                                .clone()
                                .filter(|message| !message.is_empty())
                                .unwrap_or_else(|| {
                                    "A Claude worker is waiting for permission".to_string()
                                }),
                            command: None,
                            tool_name: payload
                                .get("tool_name")
                                .and_then(Value::as_str)
                                .and_then(text::field),
                            risk: risk::assess(
                                payload.get("tool_name").and_then(Value::as_str),
                                None,
                            ),
                        })]
                    }
                    "idle_prompt" => {
                        vec![make(EventKind::AgentWaitingForUser {
                            reason: AwaitingReason::Input,
                            summary: message,
                        })]
                    }
                    "agent_needs_input" => {
                        vec![make_descendant(EventKind::AgentWaitingForUser {
                            reason: AwaitingReason::Input,
                            summary: message,
                        })]
                    }
                    // Progress reports. Announcing that something finished is not
                    // a demand, and must not put the session in the queue.
                    _ if NON_DEMANDING_NOTIFICATIONS.contains(&notification_type) => Vec::new(),
                    // An unrecognised type is treated as a demand on purpose: a
                    // release that renames the idle notification should cost the
                    // user a stale badge, not a missed hand-off.
                    _ => vec![make(EventKind::AgentWaitingForUser {
                        reason: AwaitingReason::Input,
                        summary: message,
                    })],
                }
            }

            // Claude Code Agent Teams are launched with the Agent tool, but in
            // 2.1.222 they do not emit the classic SubagentStart callback. The
            // successful tool result is structured and contains the exact name,
            // type and tool-owned id, so it is an explicit declaration rather
            // than an inference from terminal output or the process table.
            "PostToolUse"
                if payload.get("tool_name").and_then(Value::as_str) == Some("Agent")
                    && payload
                        .get("tool_response")
                        .and_then(|response| response.get("status"))
                        .and_then(Value::as_str)
                        == Some("teammate_spawned") =>
            {
                let input = payload.get("tool_input").unwrap_or(&Value::Null);
                let response = payload.get("tool_response").unwrap_or(&Value::Null);
                vec![make(EventKind::AgentSpawned {
                    declared_name: response
                        .get("name")
                        .or_else(|| input.get("name"))
                        .and_then(Value::as_str)
                        .and_then(text::field),
                    agent_type: response
                        .get("agent_type")
                        .or_else(|| input.get("subagent_type"))
                        .and_then(Value::as_str)
                        .and_then(text::field),
                    agent_id: response
                        .get("agent_id")
                        .or_else(|| response.get("teammate_id"))
                        .or_else(|| response.get("agentId"))
                        .and_then(Value::as_str)
                        .and_then(text::identifier),
                    task: input
                        .get("description")
                        .or_else(|| response.get("prompt"))
                        .or_else(|| input.get("prompt"))
                        .and_then(Value::as_str)
                        .map(|task| excerpt(task, 240)),
                })]
            }

            // Confirmed hierarchy, straight from the tool. No inference.
            //
            // Newer runtimes may report the parent's declared worker name separately
            // from its generic type. Preserve both: `Reviewer` must not restore as
            // merely `default` or `Explore`.
            "SubagentStart" => vec![make(EventKind::AgentSpawned {
                declared_name: payload
                    .get("agent_name")
                    .or_else(|| payload.get("name"))
                    .and_then(Value::as_str)
                    .and_then(text::field),
                agent_type: payload
                    .get("agent_type")
                    .and_then(Value::as_str)
                    .and_then(text::field),
                agent_id: payload
                    .get("agent_id")
                    .and_then(Value::as_str)
                    .and_then(text::identifier),
                task: payload
                    .get("task")
                    .or_else(|| payload.get("prompt"))
                    .and_then(Value::as_str)
                    .map(|task| excerpt(task, 240)),
            })],

            "SubagentStop" => vec![make_descendant(EventKind::AgentSubagentStopped {
                agent_id: payload
                    .get("agent_id")
                    .and_then(Value::as_str)
                    .and_then(text::identifier),
            })],

            // Claude finished replying — which says nothing about the processes
            // it started. `background_tasks` is Claude Code telling us exactly
            // that, so Case E needs no inference: the turn is done, and these
            // are still running.
            "Stop" => vec![make(EventKind::AgentTurnCompleted {
                last_message: payload
                    .get("last_assistant_message")
                    .and_then(Value::as_str)
                    .map(|m| excerpt(m, 240)),
                background_tasks: payload
                    .get("background_tasks")
                    .and_then(Value::as_array)
                    .map(Vec::len)
                    .unwrap_or(0),
            })],

            // The reason lives in `error` — a fixed vocabulary of failure codes
            // (`rate_limit`, `overloaded`, `model_not_found`, …), which is why it
            // is shown as-is rather than prettified. `message` is accepted too
            // because the published documentation names it that way, and it is
            // what Turn read before a live capture proved otherwise.
            "StopFailure" => {
                let code = payload
                    .get("error")
                    .or_else(|| payload.get("message"))
                    .and_then(Value::as_str)
                    .and_then(text::field);
                let details = payload
                    .get("error_details")
                    .and_then(Value::as_str)
                    .and_then(text::field);
                let reason = match (code, details) {
                    (Some(code), Some(details)) => format!("{code}: {}", excerpt(&details, 120)),
                    (Some(code), None) => code,
                    (None, Some(details)) => excerpt(&details, 120),
                    (None, None) => "the turn ended with an API error".to_string(),
                };
                vec![make(EventKind::AgentFailed { reason })]
            }

            "SessionEnd" => vec![make(EventKind::AgentIdle)],

            // An unrecognised event is dropped rather than guessed at. New hook
            // events appear with every release and must not become noise.
            _ => Vec::new(),
        }
    }
}

/// A one-line description of what the agent wants to do.
///
/// Every part of it comes from the payload, so every part is filtered. The
/// summary is an excerpt by design — the full command travels in its own field,
/// and the approval UI is responsible for showing that, not this sentence.
fn permission_summary(tool_name: Option<&str>, command: Option<&str>, payload: &Value) -> String {
    if let Some(command) = command {
        return format!("Run `{}`", excerpt(command, 120));
    }
    if let Some(path) = payload
        .get("tool_input")
        .and_then(|input| input.get("file_path"))
        .and_then(Value::as_str)
    {
        let tool = tool_name
            .and_then(text::field)
            .unwrap_or_else(|| "A tool".to_string());
        return format!("{tool} on {}", excerpt(path, 200));
    }
    match tool_name.and_then(text::field) {
        Some(tool) => format!("Use {tool}"),
        None => "Permission needed".to_string(),
    }
}

/// Writes a file only the current user can read.
///
/// The document contains this node's hook URL, and the token in that URL is the
/// only thing standing between another account on the machine and the ability to
/// tell Turn that someone else's agent is waiting for them. `std::fs::write`
/// would leave it at the umask's mercy, which on a stock developer machine means
/// world-readable.
fn write_private(path: &std::path::Path, contents: &[u8]) -> std::io::Result<()> {
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
        // An existing file keeps its old mode, so set it explicitly too.
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        file.sync_all()
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, contents)
    }
}

/// Narrows a directory to the current user. Best effort: a directory Turn cannot
/// chmod is not a reason to refuse to launch the user's agent.
fn restrict_directory(dir: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(error) = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)) {
            tracing::warn!(
                dir = %dir.display(),
                %error,
                "could not restrict the scratch directory to this user"
            );
        }
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use turn_core::event::Risk;
    use turn_core::ids::{NodeId, SessionId};

    const T0: i64 = 1_723_000_000_000;

    fn ctx() -> EventContext {
        EventContext {
            session_id: SessionId::from_stored("sess_test01"),
            node_id: NodeId::from_stored("proc_test01"),
            timestamp_ms: T0,
        }
    }

    fn launch_ctx(dir: &std::path::Path) -> LaunchContext {
        LaunchContext {
            session_id: SessionId::from_stored("sess_test01"),
            node_id: NodeId::from_stored("proc_test01"),
            cwd: "/repo".into(),
            command: "claude".into(),
            user_args: vec!["--permission-mode".into(), "acceptEdits".into()],
            launch_profile: None,
            endpoint: crate::adapter::HookEndpoint {
                base_url: "http://127.0.0.1:51234".into(),
                token: "tok_abc".into(),
                helper_path: Some(PathBuf::from("/usr/local/bin/turn-hook")),
            },
            scratch_dir: dir.to_path_buf(),
        }
    }

    fn normalise(payload: Value) -> Vec<TurnEvent> {
        ClaudeCodeAdapter::new().normalise(&payload, &ctx())
    }

    /// The point of recording the agent's own session id: a restore can offer to
    /// continue the conversation instead of starting a new one.
    #[test]
    fn a_recorded_session_id_becomes_a_resume_launch() {
        let adapter = ClaudeCodeAdapter::new();
        assert_eq!(
            adapter.resume_args("84cde77e-f54f-41e7-bb05-2716cb61b6bf"),
            Some(vec![
                "--resume".to_string(),
                "84cde77e-f54f-41e7-bb05-2716cb61b6bf".to_string()
            ])
        );
        assert!(
            adapter.capabilities().resumable,
            "the capability and the mechanism must agree, or the UI offers what the \
             adapter cannot do"
        );
    }

    /// A blank or malformed id must not become a bare `--resume`, which makes Claude
    /// Code open its interactive session picker — a prompt nobody asked for, in a
    /// pane that was supposed to come back the way it was left.
    #[test]
    fn an_unusable_session_id_yields_no_resume_rather_than_a_bare_flag() {
        let adapter = ClaudeCodeAdapter::new();
        for id in ["", "   ", "two words", "id\twith\ttabs"] {
            assert_eq!(
                adapter.resume_args(id),
                None,
                "{id:?} must not produce a resume launch"
            );
        }
        // Surrounding whitespace alone is recoverable, so it is trimmed rather than
        // refused: the id itself is intact. A trailing newline is the common case,
        // since ids reach Turn through JSON payloads and log lines.
        for id in ["  abc-123  ", "abc-123\n"] {
            assert_eq!(
                adapter.resume_args(id),
                Some(vec!["--resume".to_string(), "abc-123".to_string()]),
                "{id:?} names a usable id"
            );
        }
    }

    #[test]
    fn the_adapter_claims_the_claude_executable_only() {
        let adapter = ClaudeCodeAdapter::new();
        assert!(adapter.handles("claude"));
        assert!(adapter.handles("/usr/local/bin/claude --resume"));
        assert!(!adapter.handles("codex"));
        assert!(!adapter.handles("zsh"));
        assert_eq!(adapter.best_level(), IntegrationLevel::Structured);
        assert!(adapter.capabilities().subagent_events);
    }

    #[test]
    fn preparing_writes_a_settings_file_and_passes_it_without_touching_user_config() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = launch_ctx(dir.path());
        let plan = ClaudeCodeAdapter::new().prepare(&ctx).unwrap();

        assert_eq!(plan.command, "claude");
        // The user's own flags survive.
        assert!(plan.args.ends_with(&ctx.user_args));
        let settings_index = plan.args.iter().position(|a| a == "--settings").unwrap();
        let path = PathBuf::from(&plan.args[settings_index + 1]);
        assert!(path.exists(), "the settings file must actually be written");
        assert!(
            path.starts_with(dir.path()),
            "configuration must live in Turn's scratch directory, not the user's"
        );

        let document: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let hooks = document.get("hooks").unwrap().as_object().unwrap();
        for event in SUBSCRIBED_EVENTS {
            assert!(
                hooks.contains_key(*event),
                "missing subscription for {event}"
            );
        }
        // And the callback carries the per-node token.
        let handler = &hooks["Stop"][0]["hooks"][0];
        assert_eq!(handler["type"], "http");
        assert_eq!(handler["url"], "http://127.0.0.1:51234/hook/tok_abc");
        assert_eq!(
            hooks["PostToolUse"][0]["matcher"], "Agent",
            "the high-frequency tool hook must be filtered before it reaches Turn"
        );
    }

    /// Claude Code 2.1.222 recognises `teammateMode` with the value `in-process`.
    /// Keeping this alongside the hooks is a product boundary, not presentation:
    /// teammates must stay inside the Claude process supervised by Turn instead of
    /// opening iTerm panes that Turn cannot navigate or focus reliably.
    #[test]
    fn agent_teams_stay_in_process_without_losing_subagent_lifecycle_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = launch_ctx(dir.path());
        let plan = ClaudeCodeAdapter::new().prepare(&ctx).unwrap();

        let settings_index = plan.args.iter().position(|a| a == "--settings").unwrap();
        let settings_path = PathBuf::from(&plan.args[settings_index + 1]);
        let document: Value =
            serde_json::from_slice(&std::fs::read(&settings_path).unwrap()).unwrap();

        assert_eq!(
            document["teammateMode"], TEAMMATE_MODE_IN_PROCESS,
            "Turn's isolated launch settings must override an ambient iTerm teammate backend"
        );
        for event in ["SubagentStart", "SubagentStop"] {
            let handler = &document["hooks"][event][0]["hooks"][0];
            assert_eq!(handler["type"], "http", "{event} lost its hook: {document}");
            assert_eq!(
                handler["url"], "http://127.0.0.1:51234/hook/tok_abc",
                "{event} must still report on the parent node's authenticated channel"
            );
        }
        assert!(
            !plan.args.iter().any(|arg| arg == "--teammate-mode"),
            "the containment policy belongs to the isolated settings file: {:?}",
            plan.args
        );
        assert!(
            settings_path.starts_with(dir.path()),
            "the adapter must not modify ~/.claude/settings.json: {}",
            settings_path.display()
        );
    }

    /// Claude Code 2.1.221 silently drops HTTP handlers registered for
    /// `SessionStart`, so subscribing to it over HTTP means never learning the
    /// agent's own session id — and never being able to resume it.
    #[test]
    fn session_start_is_subscribed_through_the_helper_even_on_the_http_transport() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = launch_ctx(dir.path());
        let document = ClaudeCodeAdapter::new().settings_document(&ctx);
        let hooks = document["hooks"].as_object().unwrap();

        let session_start = &hooks["SessionStart"][0]["hooks"][0];
        assert_eq!(session_start["type"], "command");
        assert_eq!(session_start["command"], "/usr/local/bin/turn-hook");

        // Every other subscription stays on HTTP: one spawn per session start is
        // affordable, one per tool call is not.
        for event in SUBSCRIBED_EVENTS {
            if EVENTS_WITHOUT_HTTP_DELIVERY.contains(event) {
                continue;
            }
            assert_eq!(
                hooks[*event][0]["hooks"][0]["type"], "http",
                "{event} should still use the cheap transport"
            );
        }
    }

    /// `SessionStart` is the one event that cannot travel over HTTP, so without the
    /// helper it cannot be delivered at all — and it is the event carrying the
    /// agent's own session id. A launch in that state must not claim native
    /// integration and must not pretend a `turn-hook` Turn never located will run.
    #[test]
    fn without_the_helper_the_session_id_is_admitted_lost_rather_than_claimed_native() {
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = launch_ctx(dir.path());
        ctx.endpoint.helper_path = None;

        let adapter = ClaudeCodeAdapter::new();
        let document = adapter.settings_document(&ctx);
        let hooks = document["hooks"].as_object().unwrap();
        assert!(
            !hooks.contains_key("SessionStart"),
            "a handler naming a helper Turn could not find is not a subscription: {document}"
        );
        assert!(
            !document.to_string().contains("turn-hook"),
            "no bare helper name may be handed to the agent: {document}"
        );
        // Everything that does have a transport is still configured.
        for event in SUBSCRIBED_EVENTS {
            if EVENTS_WITHOUT_HTTP_DELIVERY.contains(event) {
                continue;
            }
            assert_eq!(hooks[*event][0]["hooks"][0]["type"], "http", "{event}");
        }

        let plan = adapter.prepare(&ctx).unwrap();
        assert_eq!(
            plan.level,
            IntegrationLevel::Wrapper,
            "an integration that cannot learn the session id is not Structured: {}",
            plan.note
        );
        assert!(
            plan.note.contains("SessionStart") && plan.note.contains("turn-hook"),
            "the note must name what is missing and why: {}",
            plan.note
        );
        assert!(
            plan.note.contains("session id"),
            "and what it costs the user: {}",
            plan.note
        );

        // On the helper transport nothing at all can be delivered, so nothing is
        // claimed: the same answer Codex gives with no helper.
        let helper_only = ClaudeCodeAdapter::with_transport(HookTransport::Helper);
        assert!(helper_only.settings_document(&ctx)["hooks"]
            .as_object()
            .unwrap()
            .is_empty());
        let plan = helper_only.prepare(&ctx).unwrap();
        assert_eq!(plan.level, IntegrationLevel::GenericTerminal);
        assert!(plan.note.contains("turn-hook"), "got {}", plan.note);
    }

    /// `--settings` is one slot. Turn appending its own after the user's would let
    /// Claude Code's precedence decide, silently, which of the two configurations
    /// is discarded — and Turn would still be claiming the hooks were installed.
    #[test]
    fn a_users_own_settings_flag_is_left_in_place_and_the_cost_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        let scratch = dir.path().join("scratch");
        std::fs::create_dir_all(project.join(".claude")).unwrap();
        std::fs::write(
            project.join(".claude/settings.local.json"),
            br#"{"statusLine":{"type":"command","command":"private command Turn must not copy"}}"#,
        )
        .unwrap();
        let adapter = ClaudeCodeAdapter::new();

        for user_flag in [
            vec!["--settings".into(), "/home/me/mine.json".into()],
            vec!["--settings=/home/me/mine.json".into()],
            vec!["--settings".into(), r#"{"model":"sonnet"}"#.into()],
            vec![r#"--settings={"model":"sonnet"}"#.into()],
            vec![
                r#"--settings={"disableAllHooks":true,"statusLine":{"type":"command","command":"printf cli-owned"}}"#
                    .into(),
            ],
            vec![
                "--settings".into(),
                "/home/me/first.json".into(),
                "--settings=/home/me/second.json".into(),
            ],
            vec!["--settings".into()],
        ] {
            let mut ctx = launch_ctx(&scratch);
            ctx.cwd = project.to_string_lossy().into_owned();
            ctx.user_args = user_flag.clone();

            let plan = adapter.prepare(&ctx).unwrap();
            assert_eq!(
                plan.args, user_flag,
                "the user's arguments must come through untouched"
            );
            assert_eq!(
                plan.args
                    .iter()
                    .filter(|a| a.starts_with("--settings"))
                    .count(),
                user_flag
                    .iter()
                    .filter(|a| a.starts_with("--settings"))
                    .count(),
                "Turn must not add or remove any --settings occurrence: {:?}",
                plan.args
            );
            assert_eq!(
                plan.level,
                IntegrationLevel::GenericTerminal,
                "with no hooks in effect there is nothing native to claim: {}",
                plan.note
            );
            assert!(
                plan.note.contains("--settings") && plan.note.contains("claude-hooks.json"),
                "the note must explain the collision and where Turn's file is: {}",
                plan.note
            );
            assert!(!plan.note.contains("private command"));
            assert!(!scratch.join("claude-statusline-original.sh").exists());
            assert!(!scratch.join("claude-statusline-turn.sh").exists());
            assert!(!std::fs::read_to_string(scratch.join("claude-hooks.json"))
                .unwrap()
                .contains("private command"));
        }
    }

    #[test]
    fn settings_that_only_look_like_flags_inside_the_prompt_do_not_block_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        let scratch = dir.path().join("scratch");
        std::fs::create_dir_all(&project).unwrap();
        let mut ctx = launch_ctx(&scratch);
        ctx.cwd = project.to_string_lossy().into_owned();
        ctx.user_args = vec![
            "--setting-sources=".into(),
            "--model".into(),
            "opus".into(),
            "--".into(),
            "literal prompt".into(),
            "--settings=/literal/prompt.json".into(),
            "--settings".into(),
            "still literal".into(),
            "--".into(),
            "tail".into(),
        ];
        let requested_boundary = ctx.user_args.iter().position(|arg| arg == "--").unwrap();

        let plan = ClaudeCodeAdapter::new().prepare(&ctx).unwrap();
        let effective_boundary = plan.args.iter().position(|arg| arg == "--").unwrap();
        assert!(plan.args.ends_with(&ctx.user_args));
        assert_eq!(
            &plan.args[effective_boundary..],
            &ctx.user_args[requested_boundary..],
            "the complete prompt suffix must remain literal and exact"
        );
        let settings_index = plan
            .args
            .iter()
            .position(|arg| arg == "--settings")
            .unwrap();
        assert!(settings_index < effective_boundary, "{:?}", plan.args);
        assert!(std::path::Path::new(&plan.args[settings_index + 1]).starts_with(&scratch));
        assert_eq!(plan.level, IntegrationLevel::Structured, "{}", plan.note);
        assert!(plan.note.contains("Hooks injected via --settings"));
    }

    #[cfg(unix)]
    #[test]
    fn an_effective_status_command_stays_out_of_launch_surfaces() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        let scratch = dir.path().join("scratch");
        let private_command = "printf status-command-private-sentinel";
        std::fs::create_dir_all(project.join(".claude")).unwrap();
        std::fs::write(
            project.join(".claude/settings.json"),
            serde_json::to_vec(&serde_json::json!({
                "statusLine": {
                    "type": "command",
                    "command": private_command,
                    "padding": 5
                }
            }))
            .unwrap(),
        )
        .unwrap();
        let mut ctx = launch_ctx(&scratch);
        ctx.cwd = project.to_string_lossy().into_owned();
        ctx.user_args = vec!["--setting-sources=project".into()];

        let plan = ClaudeCodeAdapter::new().prepare(&ctx).unwrap();
        assert!(!plan
            .args
            .iter()
            .any(|value| value.contains(private_command)));
        assert!(!plan.env.iter().any(|(key, value)| {
            key.contains(private_command) || value.contains(private_command)
        }));
        assert!(!plan.note.contains(private_command));
        let settings_index = plan
            .args
            .iter()
            .position(|arg| arg == "--settings")
            .unwrap();
        let settings = std::fs::read_to_string(&plan.args[settings_index + 1]).unwrap();
        assert!(!settings.contains(private_command));
        assert!(settings.contains("claude-statusline-turn.sh"));
        assert_eq!(
            std::fs::read_to_string(scratch.join("claude-statusline-original.sh")).unwrap(),
            format!("#!/bin/sh\n{private_command}\n")
        );
    }

    /// The fallback transport runs the helper. It must not hand it the URL as an
    /// argument: that string carries the node's token, and an argument list is
    /// world-readable on Linux.
    #[test]
    fn the_helper_transport_runs_turn_hook_without_putting_the_token_in_argv() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = launch_ctx(dir.path());
        let adapter = ClaudeCodeAdapter::with_transport(HookTransport::Helper);
        let document = adapter.settings_document(&ctx);

        let handler = &document["hooks"]["Stop"][0]["hooks"][0];
        assert_eq!(handler["type"], "command");
        assert_eq!(handler["command"], "/usr/local/bin/turn-hook");
        assert!(handler.get("args").is_none(), "got {handler}");
        assert!(
            !document.to_string().contains("tok_abc"),
            "no part of a command-based handler may carry the token"
        );

        // The helper learns the destination from the environment instead.
        let plan = adapter.prepare(&ctx).unwrap();
        assert_eq!(
            plan.env
                .iter()
                .find(|(key, _)| key == "TURN_HOOK_URL")
                .map(|(_, value)| value.as_str()),
            Some("http://127.0.0.1:51234/hook/tok_abc")
        );
    }

    /// The settings file holds the token. Another account on the machine must not
    /// be able to read it out of Turn's scratch space.
    #[cfg(unix)]
    #[test]
    fn the_settings_file_and_its_directory_are_readable_only_by_this_user() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let scratch = dir.path().join("node");
        let mut ctx = launch_ctx(dir.path());
        ctx.scratch_dir = scratch.clone();

        let plan = ClaudeCodeAdapter::new().prepare(&ctx).unwrap();
        let index = plan.args.iter().position(|a| a == "--settings").unwrap();
        let settings = PathBuf::from(&plan.args[index + 1]);

        let file_mode = std::fs::metadata(&settings).unwrap().permissions().mode() & 0o777;
        assert_eq!(file_mode, 0o600, "settings mode was {file_mode:o}");
        let dir_mode = std::fs::metadata(&scratch).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700, "scratch mode was {dir_mode:o}");

        // Rewriting an existing file must not widen it either.
        std::fs::set_permissions(&settings, std::fs::Permissions::from_mode(0o644)).unwrap();
        ClaudeCodeAdapter::new().prepare(&ctx).unwrap();
        let mode = std::fs::metadata(&settings).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "a rewrite must narrow it again, got {mode:o}");
    }

    #[test]
    fn stop_becomes_a_completed_turn_and_nothing_more() {
        let events = normalise(json!({
            "hook_event_name": "Stop",
            "session_id": "claude-abc",
            "last_assistant_message": "I have fixed   the\nclimbing bug."
        }));

        assert_eq!(events.len(), 1);
        match &events[0].kind {
            EventKind::AgentTurnCompleted {
                last_message,
                background_tasks,
            } => {
                assert_eq!(
                    last_message.as_deref(),
                    Some("I have fixed the climbing bug.")
                );
                assert_eq!(*background_tasks, 0);
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(events[0].confidence, Confidence::Explicit);
        // Crucially: finishing a turn is not a demand for attention by itself.
        assert_eq!(events[0].attention_reason(), None);
    }

    #[test]
    fn a_permission_request_carries_the_command_and_a_risk_rating() {
        let events = normalise(json!({
            "hook_event_name": "PermissionRequest",
            "session_id": "claude-abc",
            "tool_name": "Bash",
            "tool_input": { "command": "make verify", "description": "Run checks" }
        }));

        assert_eq!(events.len(), 1);
        match &events[0].kind {
            EventKind::AgentPermissionRequired {
                summary,
                command,
                tool_name,
                risk,
            } => {
                assert_eq!(summary, "Run `make verify`");
                assert_eq!(command.as_deref(), Some("make verify"));
                assert_eq!(tool_name.as_deref(), Some("Bash"));
                assert_eq!(*risk, Risk::Medium);
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(
            events[0].attention_reason(),
            Some(AwaitingReason::Permission)
        );
    }

    #[test]
    fn an_oversized_permission_command_is_never_presented_as_a_complete_command() {
        let dangerous = format!("{} && rm -rf /", "x".repeat(text::MAX_COMMAND_CHARS));
        let events = normalise(json!({
            "hook_event_name": "PermissionRequest",
            "tool_name": "Bash",
            "tool_input": { "command": dangerous }
        }));

        match &events[0].kind {
            EventKind::AgentPermissionRequired {
                summary,
                command,
                risk,
                ..
            } => {
                assert_eq!(summary, text::COMMAND_TOO_LONG_SUMMARY);
                assert_eq!(command, &None, "a partial command would be a lie");
                assert_eq!(*risk, Risk::High);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn a_dangerous_permission_request_is_rated_high() {
        let events = normalise(json!({
            "hook_event_name": "PermissionRequest",
            "tool_name": "Bash",
            "tool_input": { "command": "rm -rf ./build" }
        }));
        match &events[0].kind {
            EventKind::AgentPermissionRequired { risk, .. } => assert_eq!(*risk, Risk::High),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn a_file_edit_permission_names_the_file() {
        let events = normalise(json!({
            "hook_event_name": "PermissionRequest",
            "tool_name": "Edit",
            "tool_input": { "file_path": "/repo/src/main.rs" }
        }));
        match &events[0].kind {
            EventKind::AgentPermissionRequired { summary, .. } => {
                assert_eq!(summary, "Edit on /repo/src/main.rs");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    /// The three notification types mean different things and must not be
    /// flattened into one.
    #[test]
    fn notification_types_are_told_apart() {
        let permission = normalise(json!({
            "hook_event_name": "Notification",
            "notification_type": "permission_prompt",
            "message": "Claude needs your permission to use Bash"
        }));
        assert!(matches!(
            permission[0].kind,
            EventKind::AgentPermissionRequired { .. }
        ));
        assert_eq!(
            permission[0].attention_reason(),
            Some(AwaitingReason::Permission)
        );
        assert_eq!(permission[0].node_id.as_ref(), Some(&ctx().node_id));
        assert_eq!(permission[0].parent_node_id, None);

        let idle = normalise(json!({
            "hook_event_name": "Notification",
            "notification_type": "idle_prompt",
            "message": "Claude is waiting for your input"
        }));
        assert_eq!(idle[0].attention_reason(), Some(AwaitingReason::Input));
        assert_eq!(idle[0].node_id.as_ref(), Some(&ctx().node_id));

        let needs_input = normalise(json!({
            "hook_event_name": "Notification",
            "notification_type": "agent_needs_input"
        }));
        assert_eq!(
            needs_input[0].attention_reason(),
            Some(AwaitingReason::Input)
        );
        assert_eq!(needs_input[0].node_id, None);
        assert_eq!(needs_input[0].parent_node_id.as_ref(), Some(&ctx().node_id));

        // A worker's permission prompt blocks work just the same.
        let worker = normalise(json!({
            "hook_event_name": "Notification",
            "notification_type": "worker_permission_prompt",
            "message": "A worker needs your permission"
        }));
        assert_eq!(
            worker[0].attention_reason(),
            Some(AwaitingReason::Permission)
        );
        assert_eq!(worker[0].node_id, None);
        assert_eq!(worker[0].parent_node_id.as_ref(), Some(&ctx().node_id));
        assert_eq!(worker[0].confidence, Confidence::Explicit);
        assert!(matches!(
            worker[0].source,
            EventSource::Hook {
                ref tool,
                ref event_name
            } if tool == "claude-code" && event_name == "Notification"
        ));

        // Announcements of things that finished are not demands on the user.
        for progress in NON_DEMANDING_NOTIFICATIONS {
            let events = normalise(json!({
                "hook_event_name": "Notification",
                "notification_type": progress,
                "message": "something finished"
            }));
            assert!(events.is_empty(), "{progress} must not raise attention");
        }

        // An unknown type errs towards asking for the user rather than losing them.
        let future = normalise(json!({
            "hook_event_name": "Notification",
            "notification_type": "some_future_type"
        }));
        assert_eq!(future[0].attention_reason(), Some(AwaitingReason::Input));
    }

    /// Case D: subagents arrive as confirmed facts, not inferences.
    #[test]
    fn subagents_are_reported_explicitly_with_their_type() {
        let started = normalise(json!({
            "hook_event_name": "SubagentStart",
            "session_id": "claude-abc",
            "agent_id": "sub-42",
            "agent_type": "Explore"
        }));
        match &started[0].kind {
            EventKind::AgentSpawned {
                declared_name,
                agent_type,
                agent_id,
                task,
            } => {
                assert_eq!(declared_name, &None);
                assert_eq!(agent_type.as_deref(), Some("Explore"));
                assert_eq!(agent_id.as_deref(), Some("sub-42"));
                assert_eq!(task, &None);
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(started[0].confidence, Confidence::Explicit);

        let stopped = normalise(json!({
            "hook_event_name": "SubagentStop",
            "agent_id": "sub-42"
        }));
        assert!(matches!(
            stopped[0].kind,
            EventKind::AgentSubagentStopped { .. }
        ));
        assert_eq!(stopped[0].node_id, None);
        assert_eq!(stopped[0].parent_node_id.as_ref(), Some(&ctx().node_id));
    }

    #[test]
    fn a_parent_declared_subagent_name_is_not_confused_with_its_type() {
        let started = normalise(json!({
            "hook_event_name": "SubagentStart",
            "agent_id": "sub-reviewer",
            "agent_name": "Reviewer",
            "agent_type": "Explore",
            "task": "Review the climbing diff"
        }));
        match &started[0].kind {
            EventKind::AgentSpawned {
                declared_name,
                agent_type,
                task,
                ..
            } => {
                assert_eq!(declared_name.as_deref(), Some("Reviewer"));
                assert_eq!(agent_type.as_deref(), Some("Explore"));
                assert_eq!(task.as_deref(), Some("Review the climbing diff"));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    /// Agent Teams in Claude Code 2.1.222 report their declaration as the
    /// successful Agent tool result rather than through SubagentStart. This shape
    /// is projected from a live transcript; the hook uses `tool_response` for the
    /// same structured value.
    #[test]
    fn an_agent_team_member_is_declared_from_the_agent_tool_result() {
        let spawned = normalise(json!({
            "hook_event_name": "PostToolUse",
            "tool_name": "Agent",
            "tool_use_id": "toolu_reviewer",
            "tool_input": {
                "name": "Reviewer",
                "description": "Review the climbing changes",
                "prompt": "A much longer private task prompt",
                "subagent_type": "Explore"
            },
            "tool_response": {
                "status": "teammate_spawned",
                "agent_id": "Reviewer@session-a1b2c3d4",
                "teammate_id": "Reviewer@session-a1b2c3d4",
                "name": "Reviewer",
                "agent_type": "Explore",
                "team_name": "session-a1b2c3d4",
                "is_splitpane": false
            }
        }));

        assert_eq!(spawned.len(), 1);
        match &spawned[0].kind {
            EventKind::AgentSpawned {
                declared_name,
                agent_type,
                agent_id,
                task,
            } => {
                assert_eq!(declared_name.as_deref(), Some("Reviewer"));
                assert_eq!(agent_type.as_deref(), Some("Explore"));
                assert_eq!(agent_id.as_deref(), Some("Reviewer@session-a1b2c3d4"));
                assert_eq!(task.as_deref(), Some("Review the climbing changes"));
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(spawned[0].node_id.as_ref(), Some(&ctx().node_id));
        assert_eq!(spawned[0].confidence, Confidence::Explicit);
    }

    #[test]
    fn ordinary_agent_tool_results_do_not_create_phantom_teammates() {
        for payload in [
            json!({
                "hook_event_name": "PostToolUse",
                "tool_name": "Bash",
                "tool_response": { "status": "teammate_spawned" }
            }),
            json!({
                "hook_event_name": "PostToolUse",
                "tool_name": "Agent",
                "tool_response": { "status": "completed", "agentId": "sub-42" }
            }),
            json!({
                "hook_event_name": "PostToolUse",
                "tool_name": "Agent",
                "tool_response": "not structured"
            }),
        ] {
            assert!(normalise(payload).is_empty());
        }
    }

    #[test]
    fn session_start_records_the_tools_own_session_id_for_resuming() {
        let events = normalise(json!({
            "hook_event_name": "SessionStart",
            "session_id": "claude-abc123",
            "model": "claude-opus-5"
        }));
        match &events[0].kind {
            EventKind::AgentStarted {
                tool,
                model,
                external_id,
            } => {
                assert_eq!(tool, "claude-code");
                assert_eq!(model.as_deref(), Some("claude-opus-5"));
                assert_eq!(external_id.as_deref(), Some("claude-abc123"));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn a_prompt_submission_starts_a_turn_and_clears_pending_demands() {
        let events = normalise(json!({
            "hook_event_name": "UserPromptSubmit",
            "user_prompt": "yes, go ahead"
        }));
        assert!(matches!(events[0].kind, EventKind::AgentTurnStarted { .. }));
        assert_eq!(events[0].node_id, None);
        assert_eq!(events[0].parent_node_id.as_ref(), Some(&ctx().node_id));
    }

    #[test]
    fn an_api_failure_becomes_a_failure_event() {
        // The field Claude Code actually sends.
        let events = normalise(json!({
            "hook_event_name": "StopFailure",
            "error": "overloaded"
        }));
        match &events[0].kind {
            EventKind::AgentFailed { reason } => assert_eq!(reason, "overloaded"),
            other => panic!("unexpected {other:?}"),
        }

        // Details, when offered, are worth more than the bare code.
        let detailed = normalise(json!({
            "hook_event_name": "StopFailure",
            "error": "rate_limit",
            "error_details": "retry after 30s"
        }));
        match &detailed[0].kind {
            EventKind::AgentFailed { reason } => {
                assert_eq!(reason, "rate_limit: retry after 30s");
            }
            other => panic!("unexpected {other:?}"),
        }

        // And the documented spelling still works, in case a release adopts it.
        let documented = normalise(json!({
            "hook_event_name": "StopFailure",
            "message": "overloaded_error"
        }));
        match &documented[0].kind {
            EventKind::AgentFailed { reason } => assert_eq!(reason, "overloaded_error"),
            other => panic!("unexpected {other:?}"),
        }

        // A failure with nothing to say is still a failure.
        let bare = normalise(json!({ "hook_event_name": "StopFailure" }));
        match &bare[0].kind {
            EventKind::AgentFailed { reason } => {
                assert_eq!(reason, "the turn ended with an API error");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn an_unknown_hook_event_is_ignored_rather_than_guessed_at() {
        // New releases add events; they must not turn into noise or wrong states.
        assert!(normalise(json!({ "hook_event_name": "SomeFutureEvent" })).is_empty());
        assert!(normalise(json!({ "hook_event_name": "PostToolUse" })).is_empty());
        assert!(normalise(json!({ "no_event_name": true })).is_empty());
    }

    #[test]
    fn malformed_payloads_do_not_panic() {
        for payload in [
            json!({}),
            json!({ "hook_event_name": 42 }),
            json!({ "hook_event_name": "PermissionRequest", "tool_input": "not an object" }),
            json!({ "hook_event_name": "Stop", "last_assistant_message": null }),
            json!(null),
        ] {
            let _ = normalise(payload);
        }
    }

    #[test]
    fn long_messages_are_trimmed_on_a_character_boundary() {
        let long = "á".repeat(500);
        let events = normalise(json!({
            "hook_event_name": "Stop",
            "last_assistant_message": long
        }));
        match &events[0].kind {
            EventKind::AgentTurnCompleted { last_message, .. } => {
                let message = last_message.as_deref().unwrap();
                assert!(message.chars().count() <= 241);
                assert!(message.ends_with('…'));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn every_event_is_attributed_to_its_node_without_carrying_the_hook_payload() {
        let secret = "free-text-secret-with-no-recognisable-shape-8675309";
        let events = normalise(json!({
            "hook_event_name": "Stop",
            "last_assistant_message": "finished",
            // Not a field the adapter reads, and deliberately not shaped like a
            // credential a redactor could recognise. The confidentiality boundary
            // is dropping the callback, not hoping every possible secret matches a
            // scanner rule.
            "diagnostic_note": secret
        }));
        assert_eq!(events[0].node_id.as_ref().unwrap().as_str(), "proc_test01");
        assert_eq!(events[0].session_id.as_str(), "sess_test01");
        assert!(
            events[0].raw.is_none(),
            "a Claude callback is ingress data, not a TurnEvent field"
        );
        assert!(
            !format!("{:?}", events[0]).contains(secret),
            "an ignored callback member must not survive in another event field"
        );
        assert!(matches!(
            events[0].kind,
            EventKind::AgentTurnCompleted {
                last_message: Some(ref message),
                ..
            } if message == "finished"
        ));
        assert_eq!(events[0].agent.tool.as_deref(), Some("claude-code"));
    }
}
