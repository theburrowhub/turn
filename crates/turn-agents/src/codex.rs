//! Codex CLI integration.
//!
//! Codex has two reporting mechanisms and Turn configures both, because they fail
//! in different ways:
//!
//! * **Hooks** (`-c hooks={...}`) cover the fine-grained events — session start
//!   and end, prompt submission, permission requests, subagents. They are rich,
//!   and they are gated: Codex will not run a hook the user has not trusted.
//! * **`notify`** (`-c notify=[...]`) delivers one payload, `type =
//!   "agent-turn-complete"`, and is **not** gated on anything.
//!
//! Everything below was captured live against codex-cli 0.146.0; the recording is
//! in `tests/fixtures/codex-cli-0.146.0.json` and the assertions that pin it are
//! in `tests/contract_codex.rs`.
//!
//! ## The config shape, and why getting it wrong is invisible
//!
//! Codex validates the *type* of `hooks` — `hooks="/path/file.json"` is rejected
//! with *invalid type: string, expected struct HooksToml* — but it does not
//! validate the keys inside it. A wrong key parses, produces no error, and then
//! never fires. Three spellings were tried and only one works:
//!
//! ```text
//! hooks={SessionStart=[{matcher="*",hooks=[{type="command",command="'/abs/turn-hook'"}]}]}
//!        ^^^^^^^^^^^^                ^^^^^                        ^^^^^^^^^^^^^^^^
//!        PascalCase                  `hooks`, not `handlers`       shell-quoted
//! ```
//!
//! `session_start=` and `sessionStart=` both parse and both fire nothing.
//! `handlers=` likewise. The oracle that settles it is `codex app-server` plus a
//! JSON-RPC `hooks/list` call, which lists exactly the hooks Codex parsed — a
//! configuration that produces an empty list is dead, however well it parsed.
//!
//! The `command` value is run **through a shell**: `$HOME` expands, `*` globs, and
//! a bare path containing a space fails outright. So the path is POSIX-quoted, not
//! merely TOML-escaped. A handler `args` array parses and is silently ignored —
//! argv reaches the handler empty — which is why the callback URL travels in
//! `TURN_HOOK_URL`, an environment variable Codex cannot drop: the whole
//! environment of the `codex` process is inherited by hook handlers, confirmed by
//! reading it back out of one.
//!
//! Payloads arrive on **stdin** as one JSON object with snake_case keys and a
//! PascalCase `hook_event_name`. `notify` is the opposite: its payload is appended
//! as one further argv entry and its keys are hyphenated.
//!
//! ## Hook trust, and why a launch does not claim Structured
//!
//! A newly configured hook is `untrusted`, and Codex's two front ends handle that
//! differently:
//!
//! * `codex exec` runs nothing, says nothing, and exits normally. Zero callbacks,
//!   no warning — indistinguishable from a broken integration.
//! * The interactive TUI blocks at startup on *"Hooks need review — N hooks are
//!   new or changed"*, offering *Review hooks* / *Trust all and continue* /
//!   *Continue without trusting (hooks won't run)*.
//!
//! Trusting writes `[hooks.state."…"] trusted_hash = "sha256:…"` into the user's
//! `config.toml`, one entry per handler, and changing the handler command
//! invalidates it. So whether hooks work depends on a decision Turn cannot see at
//! launch time and must not make: `--dangerously-bypass-hook-trust` exists and
//! this adapter never passes it, because granting hook trust is the user's
//! security call and hooks run outside Codex's sandbox.
//!
//! Which is why [`CodexTransport::HooksAndNotify`] — the default, which configures
//! everything — reports [`IntegrationLevel::Wrapper`] and not `Structured`.
//! `notify` is live immediately, so turn boundaries work; the rest turns on when
//! the user trusts. The moment a hook payload actually arrives it has proved
//! itself, and [`CodexAdapter::hooks_confirmed_live`] is how the caller recognises
//! that and moves to [`CodexTransport::ConfirmedHooksAndNotify`]. Turn reports the
//! level it can demonstrate, never the one it hoped for.
//!
//! ## Why the turn boundary comes from `notify` and not the `Stop` hook
//!
//! Codex does have a `Stop` hook, and it fires before `notify` with the same turn
//! id. Subscribing to both would report every turn twice, so exactly one has to
//! win — and it has to be `notify`, the one no trust gate can silence. `Stop` is
//! still translated if it ever arrives, because a user who wires it up themselves
//! means it.

use crate::adapter::{
    AdapterError, AgentAdapter, Capabilities, EventContext, IntegrationLevel, LaunchContext,
    LaunchPermissionPosture, LaunchPlan, LaunchProfileDefinition, ResolvedLaunchProfile,
    AUTONOMOUS_PROFILE_ID, SAFE_PROFILE_ID,
};
use crate::risk;
use crate::text::{self, excerpt};
use serde_json::Value;
use turn_core::event::{AgentRef, Confidence, EventKind, EventSource, Risk, TurnEvent};
use turn_core::model::LaunchConfiguration;

/// Hook events Turn subscribes to, in the only spelling Codex acts on.
///
/// PascalCase, exactly as `HookEventsToml` names its fields. snake_case and
/// camelCase both parse and both fire nothing, so this list is the contract and
/// `tests/contract_codex.rs` is what stops it drifting.
///
/// Three of Codex's eleven events are deliberately absent. `PreToolUse` and
/// `PostToolUse` fire on every single tool call and Turn maps them to nothing, so
/// subscribing would cost the agent a callback per tool for no change in what the
/// user sees; `PreCompact`/`PostCompact` likewise. `Stop` is absent for a
/// different reason — see the module docs: `notify` already reports the turn
/// boundary and it cannot be silenced by a missing trust grant, so subscribing to
/// both would report every turn twice.
const SUBSCRIBED_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PermissionRequest",
    "SubagentStart",
    "SubagentStop",
    "SessionEnd",
];

/// Which mechanisms a launch configures, and how much it may honestly claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CodexTransport {
    /// Hooks and `notify`, with hook trust unknown — the state every first launch
    /// is in. Everything is configured, `notify` works immediately, and the hooks
    /// start reporting the moment the user trusts them. Reports
    /// [`IntegrationLevel::Wrapper`], because Codex runs untrusted hooks silently
    /// and never says it skipped them.
    #[default]
    HooksAndNotify,
    /// The same configuration, after a hook payload has actually arrived. Reports
    /// [`IntegrationLevel::Structured`] on the strength of that evidence rather
    /// than on hope. See [`CodexAdapter::hooks_confirmed_live`].
    ConfirmedHooksAndNotify,
    /// `notify` only, for when the user declined hook trust.
    NotifyOnly,
}

pub struct CodexAdapter {
    transport: CodexTransport,
}

impl Default for CodexAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl CodexAdapter {
    pub fn new() -> Self {
        Self {
            transport: CodexTransport::default(),
        }
    }

    pub fn with_transport(transport: CodexTransport) -> Self {
        Self { transport }
    }

    pub fn transport(&self) -> CodexTransport {
        self.transport
    }

    /// The inline TOML value for `-c hooks=...`.
    ///
    /// Built as a string rather than through a TOML serialiser because Codex
    /// wants a single inline value on the command line, and the shape is fixed
    /// and tiny.
    ///
    /// The helper path crosses two escaping boundaries and both matter. Codex runs
    /// the `command` value **through a shell** — confirmed live, `$HOME` expanded
    /// and `*.txt` globbed inside it, and a bare path containing a space failed the
    /// hook outright — so the path is POSIX single-quoted first. The result then
    /// goes inside a TOML basic string, which is why it is TOML-escaped second.
    /// Skipping either layer turns an ordinary path like `/Users/a b/turn-hook`
    /// into a hook that never runs.
    pub fn hooks_config(&self, helper: &str) -> String {
        let command = toml_escape(&shell_quote(helper));
        let entries: Vec<String> = SUBSCRIBED_EVENTS
            .iter()
            .map(|event| {
                format!(
                    "{event}=[{{matcher=\"*\",hooks=[{{type=\"command\",command=\"{command}\"}}]}}]"
                )
            })
            .collect();
        format!("hooks={{{}}}", entries.join(","))
    }

    /// Whether a payload proves Codex's hooks are running for this session.
    ///
    /// The only trustworthy signal there is. Codex skips untrusted hooks in
    /// silence, so nothing about the launch itself can answer the question — but a
    /// hook payload in hand answers it beyond doubt, and one arrives within
    /// milliseconds of `SessionStart`. `notify` payloads do not count: they are
    /// delivered whether or not hooks were trusted, so treating one as proof would
    /// reinstate exactly the false claim this exists to prevent.
    ///
    /// The discriminator is the captured one: every hook payload carries
    /// `hook_event_name`, and the `notify` payload carries `type`.
    pub fn hooks_confirmed_live(payload: &Value) -> bool {
        payload
            .get("hook_event_name")
            .and_then(Value::as_str)
            .is_some_and(|name| !name.trim().is_empty())
    }

    /// The inline TOML value for `-c notify=...`.
    ///
    /// Codex appends the event JSON as one further argument, which is why
    /// `turn-hook` treats its first positional argument as a payload.
    ///
    /// The array holds the program and nothing else. It used to carry
    /// `"--url", "<url>"`, which put this node's token in Codex's own argv — and
    /// argv is readable by every process on the machine on Linux, so any agent
    /// Turn had launched could harvest the tokens of every other Codex session
    /// with one `ps`. The helper reads `TURN_HOOK_URL` from the environment it
    /// inherits instead.
    pub fn notify_config(&self, helper: &str) -> String {
        format!("notify=[\"{}\"]", toml_escape(helper))
    }
}

fn is_codex_policy_option(arg: &str) -> bool {
    matches!(arg, "--ask-for-approval" | "-a" | "--sandbox" | "-s")
        || arg.starts_with("--ask-for-approval=")
        || arg.starts_with("-a=")
        || arg.starts_with("--sandbox=")
        || arg.starts_with("-s=")
}

const CODEX_BYPASS_POLICY: &str = "--dangerously-bypass-approvals-and-sandbox";

fn resolve_codex_profile(
    profile_id: &str,
    args: &[String],
) -> Result<ResolvedLaunchProfile, AdapterError> {
    let bypass = args.iter().any(|arg| arg == CODEX_BYPASS_POLICY);

    match profile_id {
        SAFE_PROFILE_ID => {
            if bypass || args.iter().any(|arg| is_codex_policy_option(arg)) {
                return Err(AdapterError::LaunchProfileConflict {
                    adapter_id: "codex".to_string(),
                    profile_id: profile_id.to_string(),
                    detail: "an explicit approval or sandbox policy argument".to_string(),
                });
            }
            Ok(ResolvedLaunchProfile::safe("codex", args))
        }
        AUTONOMOUS_PROFILE_ID => {
            if let Some(conflict) = args.iter().find(|arg| is_codex_policy_option(arg)) {
                return Err(AdapterError::LaunchProfileConflict {
                    adapter_id: "codex".to_string(),
                    profile_id: profile_id.to_string(),
                    detail: format!("the explicit `{conflict}` policy argument"),
                });
            }
            let mut resolved = args.to_vec();
            if !bypass {
                resolved.push(CODEX_BYPASS_POLICY.to_string());
            }
            Ok(ResolvedLaunchProfile::autonomous(
                "codex",
                LaunchPermissionPosture::BypassApprovalsAndSandbox,
                resolved,
                vec![CODEX_BYPASS_POLICY.to_string()],
            ))
        }
        _ => Err(AdapterError::UnknownLaunchProfile {
            adapter_id: "codex".to_string(),
            profile_id: profile_id.to_string(),
        }),
    }
}

impl AgentAdapter for CodexAdapter {
    fn id(&self) -> &'static str {
        "codex"
    }

    fn provider(&self) -> &'static str {
        "openai"
    }

    fn executables(&self) -> &'static [&'static str] {
        &["codex"]
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
            // Stop identifies the provider transcript. Its latest token_count
            // record gives exact turn context usage; account quota remains a
            // separate app-server observation.
            usage_events: true,
            external_session_id: true,
        }
    }

    fn launch_profiles(&self) -> Vec<LaunchProfileDefinition> {
        vec![
            LaunchProfileDefinition::safe(
                "Codex keeps its configured approval policy and sandbox in force.",
            ),
            LaunchProfileDefinition::autonomous(
                LaunchPermissionPosture::BypassApprovalsAndSandbox,
                "Codex bypasses approvals and runs without its sandbox for this launch.",
            ),
        ]
    }

    fn resolve_launch_profile(
        &self,
        profile_id: &str,
        user_args: &[String],
    ) -> Result<ResolvedLaunchProfile, AdapterError> {
        resolve_codex_profile(profile_id, user_args)
    }

    fn launch_configuration(
        &self,
        args: &[String],
        profile: &ResolvedLaunchProfile,
    ) -> LaunchConfiguration {
        let mut configuration = crate::launch_facts::base_launch_configuration(args, profile, true);
        if args.iter().any(|arg| arg == CODEX_BYPASS_POLICY) {
            configuration.approval_mode = Some("bypassed".into());
            configuration.sandbox_mode = Some("disabled".into());
            if profile.role.is_none() {
                configuration.permission_mode =
                    Some("Custom · bypass approvals and sandbox".into());
            }
            return configuration;
        }
        configuration.approval_mode = crate::launch_facts::known_option_value(
            args,
            &["--ask-for-approval", "-a"],
            &["untrusted", "on-failure", "on-request", "never"],
        )
        .map(str::to_string);
        configuration.sandbox_mode = crate::launch_facts::known_option_value(
            args,
            &["--sandbox", "-s"],
            &["read-only", "workspace-write", "danger-full-access"],
        )
        .map(str::to_string);
        configuration
    }

    fn prepare(&self, ctx: &LaunchContext) -> Result<LaunchPlan, AdapterError> {
        let resolved_profile = self.resolve_context_launch_profile(ctx)?;
        let profile_args = resolved_profile.args;
        let url = ctx.endpoint.url();

        // Both mechanisms are command-based: Codex has no HTTP handler type, so
        // without the helper binary there is nothing to point it at. Turn still
        // launches the user's command — refusing to start a session because our
        // own helper is missing would be the wrong trade — but it says plainly
        // that no detection is configured.
        let Some(helper) = ctx.endpoint.helper_path.as_ref() else {
            return Ok(LaunchPlan {
                command: ctx.command.clone(),
                args: profile_args,
                env: base_env(ctx, &url),
                level: IntegrationLevel::GenericTerminal,
                note: "The turn-hook helper was not found, so Codex has no way to \
                       report back. Turn will show the terminal but cannot detect \
                       turns or permissions."
                    .to_string(),
            });
        };
        let helper = helper.to_string_lossy().to_string();

        let mut args = profile_args;
        // Hooks are configured in both hook transports. The difference between them
        // is only what Turn is entitled to claim, never what Codex is told.
        if self.transport != CodexTransport::NotifyOnly {
            args.push("-c".to_string());
            args.push(self.hooks_config(&helper));
        }
        args.push("-c".to_string());
        args.push(self.notify_config(&helper));

        // `--dangerously-bypass-hook-trust` would make the hooks run on the first
        // launch. It is deliberately never added: hooks run outside Codex's sandbox,
        // so trusting them is the user's decision and Turn does not get to make it
        // on their behalf to make its own feature look better.
        let (level, note) = match self.transport {
            CodexTransport::HooksAndNotify => (
                IntegrationLevel::Wrapper,
                "Hooks injected inline with -c hooks={…}, plus -c notify for turn \
                 boundaries. Your ~/.codex/config.toml is untouched. Codex will not \
                 run new hooks until you trust them — it asks once, at startup — so \
                 turn completions work now and permission and subagent detection \
                 switch on after you approve."
                    .to_string(),
            ),
            CodexTransport::ConfirmedHooksAndNotify => (
                IntegrationLevel::Structured,
                "Hooks injected inline with -c hooks={…}, plus -c notify for turn \
                 boundaries, and Codex's hooks have been seen firing: permissions \
                 and subagents are reported. Your ~/.codex/config.toml is untouched."
                    .to_string(),
            ),
            CodexTransport::NotifyOnly => (
                IntegrationLevel::Wrapper,
                "Codex hooks were unavailable, so only -c notify is configured: Turn \
                 will see turn completions but not permission requests or subagents."
                    .to_string(),
            ),
        };

        Ok(LaunchPlan {
            command: ctx.command.clone(),
            args,
            env: base_env(ctx, &url),
            level,
            note,
        })
    }

    /// `codex resume <thread-id>` continues an earlier thread.
    ///
    /// A subcommand rather than a flag, unlike Claude Code — and the id is the
    /// `thread-id` from `notify`, which this adapter established is byte-identical
    /// to the `session_id` its hooks report, so Turn has one identifier whichever
    /// mechanism delivered it.
    fn resume_args(&self, external_id: &str) -> Option<Vec<String>> {
        let id = external_id.trim();
        if id.is_empty() || id.contains(char::is_whitespace) {
            return None;
        }
        Some(vec!["resume".to_string(), id.to_string()])
    }

    fn normalise(&self, payload: &Value, ctx: &EventContext) -> Vec<TurnEvent> {
        let Some(raw_name) = event_name(payload) else {
            return Vec::new();
        };
        // Codex spells the same event three ways depending on where it appears —
        // `SessionStart` in a hook payload, `sessionStart` on the app-server wire,
        // `agent-turn-complete` from notify — so everything is folded to snake_case
        // before the match. Naively lowercasing would turn `SessionStart` into
        // `sessionstart`, which matches no arm and drops a real event on the floor.
        let name = snake_case(raw_name);

        // The notify payload is the only one that uses hyphenated keys, so its
        // own event name identifies it — and it is a side channel, not a hook.
        let via_notify = raw_name.contains('-');
        let source = if via_notify {
            EventSource::SideChannel {
                tool: "codex".into(),
                channel: "notify".into(),
            }
        } else {
            EventSource::Hook {
                tool: "codex".into(),
                // Filtered: the name is the sender's string and it reaches log
                // lines and the event panel.
                event_name: excerpt(&name, 64),
            }
        };

        let agent = AgentRef {
            provider: Some("openai".into()),
            tool: Some("codex".into()),
            model: pick(payload, &["model"]).and_then(text::field),
            external_id: pick(payload, &["agent_id", "agent-id", "subagent_id"])
                .and_then(text::identifier),
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
            .with_raw(text::raw_for_storage(payload))
        };

        // Subagent lifecycle callbacks are delivered through the parent's
        // integration endpoint. Keep that runtime as an authenticated boundary
        // and let the daemon resolve the child against its live tree.
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
            .with_raw(text::raw_for_storage(payload))
        };

        // Codex calls its conversation a thread and reports its id twice over: as
        // `session_id` in every hook payload and as `thread-id` from notify. The two
        // held the same value within one live session, so either spelling gives the
        // one id `codex resume` needs — and therefore a string that can end up in an
        // argv position. Validated, never merely copied.
        let external_id = pick(
            payload,
            &["session_id", "thread-id", "thread_id", "conversation_id"],
        )
        .and_then(text::identifier);

        match name.as_str() {
            "session_start" => vec![make(EventKind::AgentStarted {
                tool: "codex".into(),
                model: agent.model.clone(),
                external_id,
            })],

            "user_prompt_submit" => vec![make(EventKind::AgentTurnStarted {
                prompt_excerpt: pick(payload, &["prompt", "user_prompt", "input"])
                    .map(|p| excerpt(p, 160))
                    .or_else(|| first_message(payload).map(|p| excerpt(&p, 160))),
            })],

            "permission_request" => {
                let tool_name = pick(payload, &["tool_name", "tool-name", "tool"]);
                let command = permission_command(payload);
                let display_command = command.as_deref().map(text::command);
                let command_too_long = matches!(&display_command, Some(text::CommandText::TooLong));
                let stored_command = match display_command {
                    Some(text::CommandText::Complete(command)) => Some(command),
                    Some(text::CommandText::TooLong) | Some(text::CommandText::Empty) | None => {
                        None
                    }
                };
                let summary = if command_too_long {
                    text::COMMAND_TOO_LONG_SUMMARY.to_string()
                } else {
                    permission_summary(tool_name, command.as_deref(), payload)
                };
                vec![make(EventKind::AgentPermissionRequired {
                    summary,
                    // Rated on the command as it arrived; stored filtered.
                    risk: if command_too_long {
                        Risk::High
                    } else {
                        risk::assess(tool_name, command.as_deref())
                    },
                    command: stored_command,
                    tool_name: tool_name.and_then(text::field),
                })]
            }

            // Deliberately nothing. Tool calls are not turn boundaries and Turn
            // does not render them, so mapping them would only add noise — and
            // Turn must never treat an intercepted tool call as approval.
            "pre_tool_use" | "post_tool_use" | "pre_compact" | "post_compact" => Vec::new(),

            "subagent_start" => vec![make(EventKind::AgentSpawned {
                declared_name: pick(payload, &["agent_name", "agent-name", "name"])
                    .and_then(text::field),
                agent_type: pick(payload, &["agent_type", "agent-type"]).and_then(text::field),
                agent_id: pick(payload, &["agent_id", "agent-id", "subagent_id"])
                    .and_then(text::identifier),
                task: pick(payload, &["task", "prompt"]).map(|task| excerpt(task, 240)),
            })],

            "subagent_stop" => vec![make_descendant(EventKind::AgentSubagentStopped {
                agent_id: pick(payload, &["agent_id", "agent-id", "subagent_id"])
                    .and_then(text::identifier),
            })],

            "session_end" => vec![make(EventKind::AgentIdle)],

            // The `notify` payload, and the `Stop` hook, which say the same thing.
            // Turn only ever subscribes to the first, so in a Turn-configured
            // session exactly one of these arrives per turn; `Stop` is handled
            // anyway because a user who wired it up themselves meant to, and
            // dropping a real turn completion is worse than a duplicate.
            //
            // `background_tasks` is zero because neither payload reports leftover
            // work — Codex has no equivalent of Claude Code's list, and claiming a
            // count would invent a fact.
            "agent_turn_complete" | "stop" => vec![make(EventKind::AgentTurnCompleted {
                last_message: pick(
                    payload,
                    &["last-assistant-message", "last_assistant_message"],
                )
                .map(|m| excerpt(m, 240)),
                background_tasks: 0,
            })],

            // Unrecognised events are dropped. Codex is under active development
            // and a new event name must not become a wrong state.
            _ => Vec::new(),
        }
    }
}

/// Environment every Codex launch gets.
///
/// `TURN_HOOK_URL` is how the helper learns where to post when it is invoked as a
/// bare command, which is the only handler form confirmed to work.
fn base_env(ctx: &LaunchContext, url: &str) -> Vec<(String, String)> {
    vec![
        ("TURN_HOOK_URL".into(), url.to_string()),
        ("TURN_SESSION_ID".into(), ctx.session_id.to_string()),
        ("TURN_NODE_ID".into(), ctx.node_id.to_string()),
    ]
}

/// Finds the event name, tolerating the spellings both mechanisms use.
///
/// The captured pair is `hook_event_name` for hooks and `type` for `notify`, and
/// those two are first and last. The spellings in between are tolerated rather
/// than relied on: they cost nothing, and Codex is under enough churn that a
/// rename should degrade into "still recognised" rather than "silently blind".
fn event_name(payload: &Value) -> Option<&str> {
    pick(
        payload,
        &[
            "hook_event_name",
            "hook-event-name",
            "hook_event",
            "event_name",
            "event",
            "type",
        ],
    )
}

/// First present string value among a list of candidate keys.
fn pick<'a>(payload: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| payload.get(*key).and_then(Value::as_str))
}

/// The command a permission request is about, wherever Codex put it.
///
/// Captured shape: `tool_input.command` holding a plain string, with `tool_name`
/// reported as `"Bash"`. The argv-array branches below are kept because the
/// approval that produced the recording came from the shell tool, and Codex has
/// other tools whose input Turn has not seen — an unrecognised shape must degrade
/// to "no command shown", never to a panic or a wrong command.
fn permission_command(payload: &Value) -> Option<String> {
    if let Some(command) = pick(payload, &["command"]) {
        return Some(command.to_string());
    }
    for container in ["tool_input", "tool-input", "arguments", "input"] {
        if let Some(inner) = payload.get(container) {
            if let Some(command) = pick(inner, &["command", "cmd"]) {
                return Some(command.to_string());
            }
            // Codex describes an exec approval as an argv array.
            if let Some(argv) = inner.get("command").and_then(Value::as_array) {
                // One non-string member makes the representation unknown. Dropping
                // it would display and persist a command the tool never requested.
                let joined = argv
                    .iter()
                    .map(Value::as_str)
                    .collect::<Option<Vec<_>>>()?
                    .join(" ");
                if !joined.is_empty() {
                    return Some(joined);
                }
            }
        }
    }
    // A bare argv array at the top level.
    if let Some(argv) = payload.get("command").and_then(Value::as_array) {
        let joined = argv
            .iter()
            .map(Value::as_str)
            .collect::<Option<Vec<_>>>()?
            .join(" ");
        if !joined.is_empty() {
            return Some(joined);
        }
    }
    None
}

/// A one-line description of what Codex is asking to do.
///
/// Filtered like every other payload-derived sentence: this one is shown in a
/// notification and next to an approval button.
fn permission_summary(tool_name: Option<&str>, command: Option<&str>, payload: &Value) -> String {
    if let Some(command) = command {
        return format!("Run `{}`", excerpt(command, 120));
    }
    if let Some(reason) = pick(payload, &["reason", "description", "message"]) {
        let cleaned = excerpt(reason, 120);
        if !cleaned.is_empty() {
            return cleaned;
        }
    }
    match tool_name.and_then(text::field) {
        Some(tool) => format!("Use {tool}"),
        None => "Permission needed".to_string(),
    }
}

/// The first entry of the notify payload's `input-messages` array.
fn first_message(payload: &Value) -> Option<String> {
    for key in ["input-messages", "input_messages", "messages"] {
        if let Some(text) = payload
            .get(key)
            .and_then(Value::as_array)
            .and_then(|items| items.first())
            .and_then(Value::as_str)
        {
            return Some(text.to_string());
        }
    }
    None
}

/// Escapes a string for a TOML basic string.
///
/// Only backslash, quote and the control characters TOML forbids can appear in a
/// path or a loopback URL, so the mapping is exhaustive for our inputs.
fn toml_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// Folds any of Codex's event-name spellings to one snake_case form.
///
/// `SessionStart`, `sessionStart`, `session_start` and `agent-turn-complete` all
/// have to arrive at the same match arm, and the naive `to_lowercase` this
/// replaced silently produced `sessionstart` — a name nothing matched, so every
/// hook payload was dropped after being delivered correctly.
///
/// Runs of capitals stay together (`HTTPRequest` → `http_request`) so an
/// acronym does not become one underscore per letter.
fn snake_case(name: &str) -> String {
    let chars: Vec<char> = name.trim().chars().collect();
    let mut out = String::with_capacity(chars.len() + 4);
    for (index, &c) in chars.iter().enumerate() {
        if c == '-' || c == ' ' || c == '.' || c == '_' {
            if !out.ends_with('_') && !out.is_empty() {
                out.push('_');
            }
            continue;
        }
        if c.is_uppercase() && !out.is_empty() && !out.ends_with('_') {
            let previous_was_lower =
                chars[index - 1].is_lowercase() || chars[index - 1].is_numeric();
            let next_is_lower = chars.get(index + 1).is_some_and(|n| n.is_lowercase());
            if previous_was_lower || next_is_lower {
                out.push('_');
            }
        }
        out.extend(c.to_lowercase());
    }
    out
}

/// Wraps a path so a shell passes it through as one literal word.
///
/// Codex hands the `command` value to a shell, so the helper path is subject to
/// word splitting, `$` expansion and globbing. Single quotes suspend all three,
/// and the only character they cannot contain is a single quote itself — closed,
/// escaped, reopened in the usual `'\''` idiom. Quoting unconditionally rather
/// than only when it looks necessary: "looks necessary" is where these bugs live,
/// and an always-quoted path costs two bytes.
fn shell_quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for c in value.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::HookEndpoint;
    use serde_json::json;
    use std::path::PathBuf;
    use turn_core::event::Risk;
    use turn_core::ids::{NodeId, SessionId};
    use turn_core::state::AwaitingReason;

    const T0: i64 = 1_723_000_000_000;

    fn ctx() -> EventContext {
        EventContext {
            session_id: SessionId::from_stored("sess_codex01"),
            node_id: NodeId::from_stored("proc_codex01"),
            timestamp_ms: T0,
        }
    }

    fn launch_ctx(helper: Option<&str>) -> LaunchContext {
        LaunchContext {
            session_id: SessionId::from_stored("sess_codex01"),
            node_id: NodeId::from_stored("proc_codex01"),
            cwd: "/repo".into(),
            command: "codex".into(),
            user_args: vec!["--model".into(), "gpt-5".into()],
            launch_profile: None,
            endpoint: HookEndpoint {
                base_url: "http://127.0.0.1:51234".into(),
                token: "tok_codex".into(),
                helper_path: helper.map(PathBuf::from),
            },
            scratch_dir: PathBuf::from("/tmp/turn-scratch"),
        }
    }

    fn normalise(payload: Value) -> Vec<TurnEvent> {
        CodexAdapter::new().normalise(&payload, &ctx())
    }

    /// Codex resumes with a subcommand, not a flag, and the id is the same one its
    /// hooks and its `notify` payload agree on.
    #[test]
    fn a_recorded_thread_id_becomes_a_resume_launch() {
        let adapter = CodexAdapter::new();
        assert_eq!(
            adapter.resume_args("019fcdc4-5f91-7980-b743-11575462cd61"),
            Some(vec![
                "resume".to_string(),
                "019fcdc4-5f91-7980-b743-11575462cd61".to_string()
            ])
        );
    }

    #[test]
    fn an_unusable_thread_id_yields_no_resume() {
        let adapter = CodexAdapter::new();
        for id in ["", "  ", "two words"] {
            assert_eq!(adapter.resume_args(id), None, "{id:?}");
        }
    }

    #[test]
    fn the_adapter_claims_the_codex_executable_only() {
        let adapter = CodexAdapter::new();
        assert!(adapter.handles("codex"));
        assert!(adapter.handles("/opt/homebrew/bin/codex --model gpt-5"));
        assert!(!adapter.handles("claude"));
        assert!(!adapter.handles("codexx"));
        assert_eq!(adapter.best_level(), IntegrationLevel::Structured);
        assert!(
            adapter.capabilities().usage_events,
            "the Stop transcript exposes context usage"
        );
    }

    /// The shape here is the one that was seen firing. Every part of it was
    /// established by trying the alternatives and watching them do nothing, so if
    /// Codex ever renames a piece this is the test to read first.
    #[test]
    fn the_hooks_config_uses_the_exact_spelling_codex_acts_on() {
        let adapter = CodexAdapter::new();
        let config = adapter.hooks_config("/usr/local/bin/turn-hook");

        assert!(config.starts_with("hooks={"));
        assert!(config.ends_with('}'));
        for event in SUBSCRIBED_EVENTS {
            assert!(config.contains(&format!("{event}=[")), "missing {event}");
        }
        assert!(
            config.contains("hooks=[{type=\"command\",command=\"'/usr/local/bin/turn-hook'\"}]"),
            "the handler list key is `hooks` and the command is shell-quoted: {config}"
        );
        assert!(config.contains("matcher=\"*\""));
        // A path is never passed as a string value, which Codex rejects outright.
        assert!(!config.starts_with("hooks=\""));
        // The three spellings that parse and then never fire.
        assert!(
            !config.contains("handlers=["),
            "`handlers` is not Codex's key: {config}"
        );
        assert!(
            !config.contains("session_start=") && !config.contains("sessionStart="),
            "event keys are PascalCase; the other cases fire nothing: {config}"
        );
        // Tool-call events are not subscribed to, on purpose.
        assert!(!config.contains("PreToolUse"));
        assert!(!config.contains("PostToolUse"));
        // Neither is the turn boundary, which notify owns. `SubagentStop` is
        // subscribed to and ends in the same four letters, so the check is anchored.
        assert!(!config.contains("{Stop=") && !config.contains(",Stop="));
    }

    /// The fold that decides whether a delivered payload is understood or dropped.
    #[test]
    fn every_spelling_codex_uses_for_an_event_folds_to_one_name() {
        assert_eq!(snake_case("SessionStart"), "session_start");
        assert_eq!(snake_case("sessionStart"), "session_start");
        assert_eq!(snake_case("session_start"), "session_start");
        assert_eq!(snake_case("UserPromptSubmit"), "user_prompt_submit");
        assert_eq!(snake_case("PermissionRequest"), "permission_request");
        assert_eq!(snake_case("SubagentStop"), "subagent_stop");
        assert_eq!(snake_case("Stop"), "stop");
        assert_eq!(snake_case("agent-turn-complete"), "agent_turn_complete");
        // An acronym stays one word rather than becoming one underscore per letter.
        assert_eq!(snake_case("HTTPRequest"), "http_request");
        // Degenerate input produces something harmless, never a panic.
        assert_eq!(snake_case(""), "");
        assert_eq!(snake_case("  -_- "), "");
    }

    /// The helper path reaches a shell, so an ordinary path with a space in it is
    /// the failure case that matters: unquoted, the hook is reported as `Failed`.
    #[test]
    fn a_helper_path_with_shell_metacharacters_is_passed_through_as_one_word() {
        let config = CodexAdapter::new().hooks_config("/Users/a b/Turn $x/turn-hook");
        assert!(
            config.contains("command=\"'/Users/a b/Turn $x/turn-hook'\""),
            "got {config}"
        );

        assert_eq!(shell_quote("/plain/path"), "'/plain/path'");
        // The one character single quotes cannot hold: closed, escaped, reopened.
        assert_eq!(shell_quote("a'b"), "'a'\\''b'");
        // And the TOML layer on top escapes that backslash rather than losing it.
        let awkward = CodexAdapter::new().hooks_config("/tmp/a'b/turn-hook");
        assert!(
            awkward.contains("command=\"'/tmp/a'\\\\''b/turn-hook'\""),
            "got {awkward}"
        );
    }

    /// The array form is what Codex accepts. What it must *not* contain is the
    /// callback URL: that string carries this node's token, and Codex's argv is
    /// readable by every process on the machine on Linux.
    #[test]
    fn the_notify_config_names_the_program_and_never_the_token_bearing_url() {
        let config = CodexAdapter::new().notify_config("/usr/local/bin/turn-hook");
        assert_eq!(config, "notify=[\"/usr/local/bin/turn-hook\"]");

        let plan = CodexAdapter::new()
            .prepare(&launch_ctx(Some("/usr/local/bin/turn-hook")))
            .unwrap();
        assert!(
            !plan.args.iter().any(|arg| arg.contains("tok_codex")),
            "the token must not reach the process table: {:?}",
            plan.args
        );
        assert_eq!(
            plan.env
                .iter()
                .find(|(key, _)| key == "TURN_HOOK_URL")
                .map(|(_, value)| value.as_str()),
            Some("http://127.0.0.1:51234/hook/tok_codex"),
            "and the helper still learns where to post"
        );
    }

    /// A first launch configures everything and claims Wrapper, because Codex will
    /// not run hooks it has not been told to trust and does not say when it skips
    /// them. Claiming Structured here would be claiming a feature that may be dead.
    #[test]
    fn a_first_launch_configures_both_mechanisms_and_claims_only_wrapper() {
        let plan = CodexAdapter::new()
            .prepare(&launch_ctx(Some("/usr/local/bin/turn-hook")))
            .unwrap();

        assert_eq!(plan.command, "codex");
        assert!(plan
            .args
            .starts_with(&["--model".to_string(), "gpt-5".to_string()]));
        assert_eq!(
            plan.args.iter().filter(|a| *a == "-c").count(),
            2,
            "hooks and notify are two separate -c values"
        );
        assert!(plan.args.iter().any(|a| a.starts_with("hooks={")));
        assert!(plan.args.iter().any(|a| a.starts_with("notify=[")));
        assert_eq!(plan.level, IntegrationLevel::Wrapper);
        assert!(
            plan.note.contains("trust"),
            "the note must explain why detection is partial: {}",
            plan.note
        );

        // Turn never grants Codex's hook trust for the user: hooks run outside the
        // sandbox, so that approval is theirs to give.
        assert!(
            !plan
                .args
                .iter()
                .any(|arg| arg.contains("dangerously-bypass-hook-trust")),
            "Turn must not bypass a security gate to flatter its own feature: {:?}",
            plan.args
        );

        // The helper learns the URL from the environment, because a handler
        // `args` array parses and is silently ignored.
        let url = plan
            .env
            .iter()
            .find(|(k, _)| k == "TURN_HOOK_URL")
            .map(|(_, v)| v.as_str());
        assert_eq!(url, Some("http://127.0.0.1:51234/hook/tok_codex"));
    }

    /// Structured is reachable, but only once a hook has been seen firing.
    #[test]
    fn structured_is_claimed_only_after_a_hook_has_actually_fired() {
        let plan = CodexAdapter::with_transport(CodexTransport::ConfirmedHooksAndNotify)
            .prepare(&launch_ctx(Some("/usr/local/bin/turn-hook")))
            .unwrap();
        assert_eq!(plan.level, IntegrationLevel::Structured);
        assert!(plan.args.iter().any(|a| a.starts_with("hooks={")));
        assert!(plan.args.iter().any(|a| a.starts_with("notify=[")));

        // What counts as proof: a hook payload, and nothing else. A notify payload
        // arrives whether or not hooks were trusted.
        assert!(CodexAdapter::hooks_confirmed_live(
            &json!({ "hook_event_name": "SessionStart" })
        ));
        assert!(!CodexAdapter::hooks_confirmed_live(&json!({
            "type": "agent-turn-complete",
            "thread-id": "019fcdb3-60d8-7733-83a8-813720d5c490"
        })));
        assert!(!CodexAdapter::hooks_confirmed_live(&json!({})));
        assert!(!CodexAdapter::hooks_confirmed_live(
            &json!({ "hook_event_name": "  " })
        ));
        assert!(!CodexAdapter::hooks_confirmed_live(
            &json!({ "hook_event_name": 7 })
        ));
    }

    /// Hook trust may have been declined outright. Degrading must be honest about
    /// what was lost, not silent.
    #[test]
    fn without_hooks_the_launch_degrades_to_notify_and_says_so() {
        let plan = CodexAdapter::with_transport(CodexTransport::NotifyOnly)
            .prepare(&launch_ctx(Some("/usr/local/bin/turn-hook")))
            .unwrap();

        assert_eq!(plan.level, IntegrationLevel::Wrapper);
        assert!(!plan.args.iter().any(|a| a.starts_with("hooks={")));
        assert!(plan.args.iter().any(|a| a.starts_with("notify=[")));
        assert!(
            plan.note.contains("permission"),
            "the note must name what detection is missing: {}",
            plan.note
        );
    }

    /// A missing helper is Turn's problem, not a reason to refuse to launch the
    /// user's agent.
    #[test]
    fn a_missing_helper_still_launches_the_agent_with_no_detection() {
        let plan = CodexAdapter::new().prepare(&launch_ctx(None)).unwrap();

        assert_eq!(plan.command, "codex");
        assert_eq!(plan.args, vec!["--model".to_string(), "gpt-5".to_string()]);
        assert_eq!(plan.level, IntegrationLevel::GenericTerminal);
        assert!(plan.note.contains("turn-hook"));
    }

    #[test]
    fn a_path_with_quotes_cannot_break_out_of_the_toml_value() {
        let config = CodexAdapter::new().hooks_config("/tmp/a\"b\\c/turn-hook");
        assert!(
            config.contains("command=\"'/tmp/a\\\"b\\\\c/turn-hook'\""),
            "got {config}"
        );
        assert_eq!(toml_escape("plain"), "plain");
        assert_eq!(toml_escape("a\nb"), "a\\nb");
        assert_eq!(toml_escape("a\u{7}b"), "ab", "control bytes are dropped");
    }

    #[test]
    fn the_notify_turn_complete_payload_becomes_a_completed_turn() {
        // Exactly the key spellings Codex uses: hyphenated, `type` as the tag.
        let events = normalise(json!({
            "type": "agent-turn-complete",
            "thread-id": "th_9f2",
            "turn-id": "turn_17",
            "cwd": "/repo",
            "input-messages": ["fix the failing test"],
            "last-assistant-message": "Fixed   the\nassertion."
        }));

        assert_eq!(events.len(), 1);
        match &events[0].kind {
            EventKind::AgentTurnCompleted {
                last_message,
                background_tasks,
            } => {
                assert_eq!(last_message.as_deref(), Some("Fixed the assertion."));
                assert_eq!(
                    *background_tasks, 0,
                    "Codex does not report leftover work, so Turn must not invent any"
                );
            }
            other => panic!("unexpected {other:?}"),
        }
        // notify is a side channel we configured, so it is an explicit signal.
        assert_eq!(events[0].confidence, Confidence::Explicit);
        assert_eq!(
            events[0].source,
            EventSource::SideChannel {
                tool: "codex".into(),
                channel: "notify".into()
            }
        );
        assert_eq!(events[0].attention_reason(), None);
    }

    /// Codex reports the session id under `session_id` in every hook payload and
    /// under `thread-id` in the notify payload, and the two carry the same value —
    /// checked live within one session. It is what `codex resume` needs.
    #[test]
    fn session_start_records_the_session_id_for_resuming() {
        let events = normalise(json!({
            "hook_event_name": "SessionStart",
            "session_id": "019fcdb2-c194-7d10-810f-13075a093cab",
            "model": "gpt-5.6-sol",
            "source": "startup"
        }));
        match &events[0].kind {
            EventKind::AgentStarted {
                tool,
                model,
                external_id,
            } => {
                assert_eq!(tool, "codex");
                assert_eq!(model.as_deref(), Some("gpt-5.6-sol"));
                assert_eq!(
                    external_id.as_deref(),
                    Some("019fcdb2-c194-7d10-810f-13075a093cab")
                );
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(
            events[0].source,
            EventSource::Hook {
                tool: "codex".into(),
                event_name: "session_start".into()
            }
        );
    }

    #[test]
    fn a_prompt_submission_starts_a_turn() {
        let events = normalise(json!({
            "hook_event_name": "UserPromptSubmit",
            "prompt": "run the tests"
        }));
        match &events[0].kind {
            EventKind::AgentTurnStarted { prompt_excerpt } => {
                assert_eq!(prompt_excerpt.as_deref(), Some("run the tests"));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    /// An argv array was never seen live — the captured request used a plain
    /// string — but the branch stays, so this keeps it honest.
    #[test]
    fn a_permission_request_is_rated_and_summarised() {
        let events = normalise(json!({
            "hook_event_name": "PermissionRequest",
            "tool_name": "shell",
            "tool_input": { "command": ["git", "push", "--force", "origin", "main"] }
        }));

        assert_eq!(events.len(), 1);
        match &events[0].kind {
            EventKind::AgentPermissionRequired {
                summary,
                command,
                tool_name,
                risk,
            } => {
                assert_eq!(command.as_deref(), Some("git push --force origin main"));
                assert_eq!(summary, "Run `git push --force origin main`");
                assert_eq!(tool_name.as_deref(), Some("shell"));
                assert_eq!(*risk, Risk::High, "a force push must be flagged");
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(
            events[0].attention_reason(),
            Some(AwaitingReason::Permission)
        );
    }

    /// The captured shape: `tool_name` is `"Bash"` and `tool_input.command` is a
    /// plain string, not an argv array.
    #[test]
    fn the_captured_permission_shape_is_a_plain_command_string() {
        let events = normalise(json!({
            "hook_event_name": "PermissionRequest",
            "tool_name": "Bash",
            "tool_input": { "command": "touch approval-probe.txt" }
        }));
        match &events[0].kind {
            EventKind::AgentPermissionRequired {
                command,
                tool_name,
                risk,
                ..
            } => {
                assert_eq!(command.as_deref(), Some("touch approval-probe.txt"));
                assert_eq!(tool_name.as_deref(), Some("Bash"));
                assert_eq!(*risk, Risk::Medium);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn an_argv_permission_with_a_non_string_member_is_not_partially_reconstructed() {
        let events = normalise(json!({
            "hook_event_name": "PermissionRequest",
            "tool_name": "shell",
            "tool_input": { "command": ["printf", 7, "hidden"] }
        }));
        match &events[0].kind {
            EventKind::AgentPermissionRequired {
                command, summary, ..
            } => {
                assert_eq!(command, &None);
                assert_eq!(summary, "Use shell");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn an_oversized_permission_command_is_high_risk_and_not_excerpted() {
        let command = format!("{} --force", "x".repeat(text::MAX_COMMAND_CHARS));
        let events = normalise(json!({
            "hook_event_name": "PermissionRequest",
            "tool_name": "shell",
            "command": command
        }));
        match &events[0].kind {
            EventKind::AgentPermissionRequired {
                command,
                summary,
                risk,
                ..
            } => {
                assert_eq!(command, &None);
                assert_eq!(summary, text::COMMAND_TOO_LONG_SUMMARY);
                assert_eq!(*risk, Risk::High);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn a_permission_request_without_a_command_falls_back_to_its_reason() {
        let events = normalise(json!({
            "hook_event_name": "PermissionRequest",
            "reason": "Codex wants to apply a patch to src/main.rs"
        }));
        match &events[0].kind {
            EventKind::AgentPermissionRequired { summary, risk, .. } => {
                assert_eq!(summary, "Codex wants to apply a patch to src/main.rs");
                assert_eq!(*risk, Risk::Medium, "an unknown tool errs upward");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    /// Codex names the subagent, so Turn never has to guess at the hierarchy.
    /// `agent_type` was `"default"` in the recording; `agent_id` is the subagent's
    /// own session id.
    #[test]
    fn subagents_are_reported_as_confirmed_hierarchy() {
        let started = normalise(json!({
            "hook_event_name": "SubagentStart",
            "agent_id": "019fcdc0-1463-74b2-a80c-969dff2cdfae",
            "agent_type": "default"
        }));
        match &started[0].kind {
            EventKind::AgentSpawned {
                declared_name,
                agent_type,
                agent_id,
                task,
            } => {
                assert_eq!(declared_name, &None);
                assert_eq!(agent_type.as_deref(), Some("default"));
                assert_eq!(
                    agent_id.as_deref(),
                    Some("019fcdc0-1463-74b2-a80c-969dff2cdfae")
                );
                assert_eq!(task, &None);
            }
            other => panic!("unexpected {other:?}"),
        }

        let stopped = normalise(json!({
            "hook_event_name": "SubagentStop",
            "agent_id": "019fcdc0-1463-74b2-a80c-969dff2cdfae",
            "agent_type": "default",
            "last_assistant_message": "BANANA"
        }));
        match &stopped[0].kind {
            EventKind::AgentSubagentStopped { agent_id } => {
                assert_eq!(
                    agent_id.as_deref(),
                    Some("019fcdc0-1463-74b2-a80c-969dff2cdfae")
                );
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(stopped[0].node_id, None);
        assert_eq!(stopped[0].parent_node_id.as_ref(), Some(&ctx().node_id));
    }

    #[test]
    fn session_end_makes_the_agent_idle() {
        let events = normalise(json!({ "hook_event_name": "SessionEnd", "reason": "other" }));
        assert!(matches!(events[0].kind, EventKind::AgentIdle));
    }

    /// The `Stop` hook is a real turn completion, so it is translated — Turn just
    /// never subscribes to it, because `notify` already reports the same boundary
    /// and cannot be silenced by a missing trust grant.
    #[test]
    fn a_stop_hook_payload_is_a_completed_turn_even_though_turn_never_asks_for_one() {
        let events = normalise(json!({
            "hook_event_name": "Stop",
            "session_id": "019fcdb2-c194-7d10-810f-13075a093cab",
            "stop_hook_active": false,
            "last_assistant_message": "OK"
        }));
        assert_eq!(events.len(), 1);
        match &events[0].kind {
            EventKind::AgentTurnCompleted {
                last_message,
                background_tasks,
            } => {
                assert_eq!(last_message.as_deref(), Some("OK"));
                assert_eq!(*background_tasks, 0);
            }
            other => panic!("unexpected {other:?}"),
        }
        // It came over a hook, so it must be attributed to one and not to notify.
        assert_eq!(
            events[0].source,
            EventSource::Hook {
                tool: "codex".into(),
                event_name: "stop".into()
            }
        );
        let config = CodexAdapter::new().hooks_config("/usr/local/bin/turn-hook");
        assert!(!config.contains("{Stop=") && !config.contains(",Stop="));
    }

    /// Tool-call events carry no state Turn renders, and mapping them would risk
    /// implying Turn had a say in the call.
    #[test]
    fn tool_call_events_are_mapped_to_nothing_at_all() {
        for event in ["PreToolUse", "PostToolUse", "PreCompact", "PostCompact"] {
            assert!(
                normalise(json!({ "hook_event_name": event, "tool_name": "Bash" })).is_empty(),
                "{event} must produce no event"
            );
        }
    }

    #[test]
    fn an_unknown_event_is_dropped_rather_than_guessed_at() {
        assert!(normalise(json!({ "hook_event_name": "SomeNewEvent" })).is_empty());
        assert!(normalise(json!({ "type": "some-new-notification" })).is_empty());
        assert!(normalise(json!({ "nothing": "useful" })).is_empty());
    }

    /// `hook_event_name` is the captured key. The rest are tolerated so a rename
    /// degrades into "still recognised" rather than "silently blind".
    #[test]
    fn the_event_name_is_found_under_any_of_the_plausible_keys() {
        for key in [
            "hook_event_name",
            "hook_event",
            "event_name",
            "event",
            "type",
        ] {
            let events = normalise(json!({ key: "SessionEnd" }));
            assert_eq!(events.len(), 1, "the name must be readable from `{key}`");
            assert!(matches!(events[0].kind, EventKind::AgentIdle));
        }
    }

    #[test]
    fn malformed_payloads_do_not_panic() {
        for payload in [
            json!({}),
            json!(null),
            json!([1, 2, 3]),
            json!("a bare string"),
            json!({ "hook_event_name": 42 }),
            json!({ "hook_event_name": "PermissionRequest", "tool_input": "not an object" }),
            json!({ "hook_event_name": "PermissionRequest", "command": [] }),
            json!({ "type": "agent-turn-complete", "last-assistant-message": null }),
            json!({ "hook_event_name": "SessionStart", "session_id": 7 }),
            json!({ "hook_event_name": "Stop", "last_assistant_message": [] }),
            json!({ "hook_event_name": "SubagentStop", "agent_id": { "id": 1 } }),
        ] {
            let _ = normalise(payload);
        }
    }

    #[test]
    fn every_event_is_attributed_to_its_node_and_keeps_the_raw_payload() {
        let events = normalise(json!({ "type": "agent-turn-complete" }));
        assert_eq!(events[0].node_id.as_ref().unwrap().as_str(), "proc_codex01");
        assert_eq!(events[0].session_id.as_str(), "sess_codex01");
        assert_eq!(events[0].agent.provider.as_deref(), Some("openai"));
        assert!(events[0].raw.is_some());
    }

    #[test]
    fn a_very_long_assistant_message_is_trimmed_on_a_character_boundary() {
        let events = normalise(json!({
            "type": "agent-turn-complete",
            "last-assistant-message": "ñ".repeat(1_000)
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
}
