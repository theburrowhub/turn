//! Adapter selection.
//!
//! One question: given a command line the user typed, which adapter should run
//! it? The answer is always defined. Selection walks the registered adapters from
//! the strongest integration down and falls back to a terminal that makes no
//! claims, because "Turn does not recognise this command" must never mean "Turn
//! will not run this command".
//!
//! The selection is also *reported*, not just used. [`Selection::note`] and
//! [`Selection::level`] are what the session details panel shows, so a user
//! looking at a session can tell whether "waiting for you" will be a fact or a
//! guess — and if a tool is not installed, that it is not installed rather than
//! silently unrecognised.

use crate::adapter::{
    AdapterError, AgentAdapter, Capabilities, EventContext, IntegrationLevel, LaunchContext,
    LaunchPlan,
};
use crate::claude::ClaudeCodeAdapter;
use crate::codex::CodexAdapter;
use crate::heuristic::HeuristicAdapter;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use turn_core::event::TurnEvent;

/// The fallback: a terminal Turn renders and says nothing about.
///
/// Its `Turn` axis stays [`turn_core::state::Turn::Unknown`] for the whole session
/// — which is the honest answer for `make`, `vim` or a shell, and far better than
/// pointing output heuristics at a program whose output has no conversational
/// shape at all.
pub struct GenericTerminalAdapter;

impl Default for GenericTerminalAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl GenericTerminalAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl AgentAdapter for GenericTerminalAdapter {
    fn id(&self) -> &'static str {
        "generic-terminal"
    }

    fn provider(&self) -> &'static str {
        "none"
    }

    /// None. This adapter is reached by falling through, never by matching.
    fn executables(&self) -> &'static [&'static str] {
        &[]
    }

    fn handles(&self, _command: &str) -> bool {
        // Claiming everything here would shadow the real adapters depending on
        // iteration order. Selection reaches this adapter explicitly instead.
        false
    }

    fn detect(&self, _executable: &str) -> Option<PathBuf> {
        None
    }

    fn best_level(&self) -> IntegrationLevel {
        IntegrationLevel::GenericTerminal
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
    }

    fn prepare(&self, ctx: &LaunchContext) -> Result<LaunchPlan, AdapterError> {
        Ok(LaunchPlan {
            command: ctx.command.clone(),
            args: ctx.user_args.clone(),
            env: vec![
                ("TURN_SESSION_ID".into(), ctx.session_id.to_string()),
                ("TURN_NODE_ID".into(), ctx.node_id.to_string()),
            ],
            level: IntegrationLevel::GenericTerminal,
            note: "Turn will run and record this terminal but makes no claims about \
                   what it is doing."
                .to_string(),
        })
    }

    fn normalise(&self, _payload: &Value, _ctx: &EventContext) -> Vec<TurnEvent> {
        Vec::new()
    }
}

/// The adapter chosen for a command, plus everything the UI needs to explain it.
#[derive(Clone)]
pub struct Selection {
    pub adapter: Arc<dyn AgentAdapter>,
    /// The best integration this adapter can offer. The *achieved* level is only
    /// known after [`AgentAdapter::prepare`], which may degrade it.
    pub level: IntegrationLevel,
    pub capabilities: Capabilities,
    /// Where the executable was found, if it is installed.
    pub executable: Option<PathBuf>,
    /// Plain-language account of what detection the user is getting.
    pub note: String,
}

impl Selection {
    /// Whether the command was actually found on `PATH`.
    ///
    /// A false here with a `Structured` level is the interesting case: the user
    /// typed `claude` and Turn knows how to integrate with it, but it is not
    /// installed, so the session will fail for a reason that has nothing to do
    /// with Turn.
    pub fn is_installed(&self) -> bool {
        self.executable.is_some()
    }
}

impl std::fmt::Debug for Selection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Selection")
            .field("adapter", &self.adapter.id())
            .field("level", &self.level)
            .field("executable", &self.executable)
            .finish()
    }
}

/// The adapters Turn knows about, ordered strongest first.
pub struct AdapterRegistry {
    adapters: Vec<Arc<dyn AgentAdapter>>,
    fallback: Arc<dyn AgentAdapter>,
}

impl Default for AdapterRegistry {
    fn default() -> Self {
        Self::with_builtin()
    }
}

impl AdapterRegistry {
    /// Claude Code, then Codex, then output inference, with the generic terminal
    /// behind all of them.
    pub fn with_builtin() -> Self {
        Self {
            adapters: vec![
                Arc::new(ClaudeCodeAdapter::new()),
                Arc::new(CodexAdapter::new()),
                Arc::new(HeuristicAdapter::new()),
            ],
            fallback: Arc::new(GenericTerminalAdapter::new()),
        }
    }

    /// A registry with only the fallback, for tests and for a user who has turned
    /// every integration off.
    pub fn bare() -> Self {
        Self {
            adapters: Vec::new(),
            fallback: Arc::new(GenericTerminalAdapter::new()),
        }
    }

    /// Adds an adapter, keeping the list sorted by integration strength.
    ///
    /// Sorting on insert rather than trusting call order means a plugin cannot
    /// accidentally out-rank a native integration by registering first.
    pub fn register(&mut self, adapter: Arc<dyn AgentAdapter>) {
        self.adapters.push(adapter);
        self.adapters
            .sort_by_key(|adapter| std::cmp::Reverse(adapter.best_level()));
    }

    pub fn adapters(&self) -> &[Arc<dyn AgentAdapter>] {
        &self.adapters
    }

    pub fn by_id(&self, id: &str) -> Option<Arc<dyn AgentAdapter>> {
        self.adapters
            .iter()
            .chain(std::iter::once(&self.fallback))
            .find(|adapter| adapter.id() == id)
            .map(Arc::clone)
    }

    /// Picks the adapter for a command line. Never fails.
    pub fn select(&self, command_line: &str) -> Selection {
        let executable = executable_of(command_line);

        for adapter in &self.adapters {
            if !adapter.handles(executable) {
                continue;
            }
            let found = adapter.detect(executable);
            let note = describe(adapter.as_ref(), executable, found.is_some());
            return Selection {
                level: adapter.best_level(),
                capabilities: adapter.capabilities(),
                executable: found,
                note,
                adapter: Arc::clone(adapter),
            };
        }

        Selection {
            level: self.fallback.best_level(),
            capabilities: self.fallback.capabilities(),
            executable: crate::adapter::which(executable),
            note: if executable.is_empty() {
                "No command given.".to_string()
            } else {
                format!(
                    "Turn has no integration for `{executable}`, so this pane is a \
                     plain terminal: full output and history, no state detection."
                )
            },
            adapter: Arc::clone(&self.fallback),
        }
    }
}

/// A sentence about what the user is getting, for the session details panel.
fn describe(adapter: &dyn AgentAdapter, executable: &str, installed: bool) -> String {
    if !installed {
        return format!(
            "`{executable}` is not on your PATH. Turn knows how to integrate with \
             {} but cannot find it to run.",
            adapter.id()
        );
    }
    match adapter.best_level() {
        IntegrationLevel::Structured => format!(
            "{} reports its own state to Turn, so turn boundaries and permissions \
             are facts rather than guesses.",
            adapter.id()
        ),
        IntegrationLevel::Wrapper => format!(
            "{} is launched through Turn, which reports its lifecycle.",
            adapter.id()
        ),
        IntegrationLevel::Heuristic => format!(
            "{} has no way to report to Turn, so its state is inferred from output \
             and marked as a guess.",
            adapter.id()
        ),
        IntegrationLevel::GenericTerminal => {
            "A plain terminal: full output and history, no state detection.".to_string()
        }
    }
}

/// The program a command line will actually run.
///
/// Leading `VAR=value` assignments are skipped — `RUST_LOG=debug claude` is still
/// Claude Code — and a path is reduced to its file name. Anything cleverer (a
/// shell one-liner, a pipeline) is deliberately not unpicked: guessing which
/// program inside `sh -c '…'` matters would produce confident mistakes, and the
/// generic terminal is the right answer for a shell invocation.
pub fn executable_of(command_line: &str) -> &str {
    command_line
        .split_whitespace()
        .find(|token| !is_env_assignment(token))
        .unwrap_or("")
        .rsplit('/')
        .next()
        .unwrap_or("")
}

fn is_env_assignment(token: &str) -> bool {
    match token.split_once('=') {
        // `=value` is not an assignment, and neither is `--flag=x`.
        Some((name, _)) => {
            !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use turn_core::ids::{NodeId, SessionId};

    fn registry() -> AdapterRegistry {
        AdapterRegistry::with_builtin()
    }

    #[test]
    fn claude_code_wins_its_own_command() {
        let selection = registry().select("claude --resume");
        assert_eq!(selection.adapter.id(), "claude-code");
        assert_eq!(selection.level, IntegrationLevel::Structured);
        assert!(selection.capabilities.subagent_events);
    }

    #[test]
    fn codex_wins_its_own_command() {
        let selection = registry().select("codex --model gpt-5");
        assert_eq!(selection.adapter.id(), "codex");
        assert_eq!(selection.level, IntegrationLevel::Structured);
        assert!(selection.capabilities.permission_events);
    }

    /// Selection must not depend on what happens to be installed on the machine
    /// running the tests.
    ///
    /// The note does — an adapter that cannot find its executable says so instead
    /// of describing detection it will never perform — so the two are asserted
    /// separately. Conflating them made this test pass on a developer's laptop
    /// with `gemini` installed and fail in CI, which is the wrong way round for a
    /// test to behave.
    #[test]
    fn a_known_agent_cli_with_no_contract_gets_output_inference() {
        let selection = registry().select("gemini");
        assert_eq!(selection.adapter.id(), "terminal-heuristic");
        assert_eq!(selection.level, IntegrationLevel::Heuristic);

        if selection.is_installed() {
            assert!(
                selection.note.contains("guess"),
                "an installed tool with no contract must be told to the user as inferred: {}",
                selection.note
            );
        } else {
            assert!(
                selection.note.contains("PATH"),
                "a tool that is not installed must say so rather than promise detection: {}",
                selection.note
            );
        }
    }

    /// The heuristic wording itself, with no dependency on the environment: a
    /// command that is certain to exist on both platforms Turn targets.
    #[test]
    fn output_inference_is_always_presented_to_the_user_as_a_guess() {
        let selection = registry().select("sh");
        assert!(
            selection.is_installed(),
            "sh must exist for this test to mean anything"
        );
        if selection.level == IntegrationLevel::Heuristic {
            assert!(selection.note.contains("guess"), "{}", selection.note);
        }
    }

    /// The rule that keeps Turn usable: it runs whatever you give it.
    #[test]
    fn an_unknown_command_still_runs_as_a_plain_terminal() {
        for command in [
            "zsh",
            "make verify",
            "vim src/main.rs",
            "some-tool-nobody-has-heard-of --flag",
            "./scripts/deploy.sh",
        ] {
            let selection = registry().select(command);
            assert_eq!(
                selection.level,
                IntegrationLevel::GenericTerminal,
                "{command} should fall through"
            );
            assert_eq!(selection.adapter.id(), "generic-terminal");
            assert_eq!(selection.capabilities, Capabilities::default());
        }
    }

    /// A shell is a terminal, not an agent. Pointing inference at it would badge
    /// every idle prompt in the workspace.
    #[test]
    fn a_shell_never_gets_output_inference() {
        for shell in ["zsh", "bash", "fish", "/bin/sh", "sh -c 'gemini'"] {
            let selection = registry().select(shell);
            assert_eq!(
                selection.adapter.id(),
                "generic-terminal",
                "{shell} must not be treated as an agent"
            );
        }
    }

    #[test]
    fn an_empty_command_line_is_answered_rather_than_panicked_on() {
        for command in ["", "   ", "\t\n"] {
            let selection = registry().select(command);
            assert_eq!(selection.adapter.id(), "generic-terminal");
            assert_eq!(selection.note, "No command given.");
        }
    }

    #[test]
    fn an_absolute_path_and_leading_environment_variables_do_not_hide_the_tool() {
        assert_eq!(executable_of("/opt/homebrew/bin/claude"), "claude");
        assert_eq!(executable_of("RUST_LOG=debug claude --resume"), "claude");
        assert_eq!(
            executable_of("ANTHROPIC_API_KEY=sk-x FOO=1 /usr/local/bin/claude"),
            "claude"
        );
        assert_eq!(executable_of("--flag=value"), "--flag=value");
        assert_eq!(executable_of(""), "");

        let selection = registry().select("RUST_LOG=debug /opt/homebrew/bin/codex");
        assert_eq!(selection.adapter.id(), "codex");
    }

    #[test]
    fn selection_reports_whether_the_tool_is_actually_installed() {
        // `sh` exists everywhere, so a fallback selection finds its path.
        let shell = registry().select("sh");
        assert!(shell.is_installed());

        // A tool Turn integrates with but which is absent must say so plainly,
        // rather than looking like an unrecognised command.
        let mut registry = AdapterRegistry::bare();
        registry.register(Arc::new(AbsentToolAdapter));
        let selection = registry.select("turn-absent-tool-xyz");
        assert_eq!(selection.adapter.id(), "absent-tool");
        assert!(!selection.is_installed());
        assert!(
            selection.note.contains("not on your PATH"),
            "got {}",
            selection.note
        );
        // The level still describes what Turn *could* do, so the UI can offer to
        // install it rather than pretending the tool is unsupported.
        assert_eq!(selection.level, IntegrationLevel::Structured);
    }

    /// An adapter claiming several commands must answer about the one the user
    /// typed. Reporting a different installed sibling would tell them a session is
    /// ready to run when the program they asked for is not there, and would hand
    /// the UI the path of something else entirely.
    #[test]
    fn an_adapter_with_several_commands_reports_the_one_the_user_typed() {
        let mut two = AdapterRegistry::bare();
        two.register(Arc::new(TwoCommandAdapter));

        let absent = two.select("turn-absent-tool-xyz");
        assert_eq!(absent.adapter.id(), "two-command");
        assert_eq!(
            absent.executable, None,
            "the sibling `sh` being installed says nothing about this command"
        );
        assert!(!absent.is_installed());
        assert!(
            absent.note.contains("not on your PATH"),
            "got {}",
            absent.note
        );

        let present = two.select("sh --version");
        assert!(present.is_installed());
        assert_eq!(
            present
                .executable
                .as_ref()
                .and_then(|path| path.file_name())
                .map(|name| name.to_string_lossy().into_owned())
                .as_deref(),
            Some("sh")
        );

        // And the real multi-command adapter: whatever it finds is the command
        // that was asked for, on a machine with any subset of them installed.
        let inferred = registry().select("gemini");
        assert_eq!(inferred.adapter.id(), "terminal-heuristic");
        if let Some(path) = &inferred.executable {
            assert_eq!(
                path.file_name()
                    .map(|name| name.to_string_lossy().into_owned()),
                Some("gemini".to_string()),
                "selection reported {path:?} for `gemini`"
            );
        }
    }

    #[test]
    fn a_bare_registry_still_answers_every_command() {
        let selection = AdapterRegistry::bare().select("claude");
        assert_eq!(selection.adapter.id(), "generic-terminal");
        assert_eq!(selection.level, IntegrationLevel::GenericTerminal);
    }

    /// A late registration must not be able to out-rank a native integration by
    /// being first in the list.
    #[test]
    fn registering_a_weaker_adapter_cannot_shadow_a_stronger_one() {
        let mut registry = AdapterRegistry::bare();
        registry.register(Arc::new(HeuristicAdapter::new()));
        registry.register(Arc::new(ClaudeCodeAdapter::new()));

        let levels: Vec<IntegrationLevel> =
            registry.adapters().iter().map(|a| a.best_level()).collect();
        assert_eq!(
            levels,
            vec![IntegrationLevel::Structured, IntegrationLevel::Heuristic],
            "the list must stay ordered strongest first"
        );
        assert_eq!(registry.select("claude").adapter.id(), "claude-code");
    }

    #[test]
    fn adapters_can_be_looked_up_by_id_including_the_fallback() {
        let registry = registry();
        assert_eq!(
            registry.by_id("claude-code").map(|a| a.id()),
            Some("claude-code")
        );
        assert_eq!(registry.by_id("codex").map(|a| a.id()), Some("codex"));
        assert_eq!(
            registry.by_id("generic-terminal").map(|a| a.id()),
            Some("generic-terminal")
        );
        assert!(registry.by_id("nothing-like-this").is_none());
    }

    #[test]
    fn the_generic_terminal_runs_the_command_untouched_and_claims_nothing() {
        let ctx = LaunchContext {
            session_id: SessionId::from_stored("sess_gen01"),
            node_id: NodeId::from_stored("proc_gen01"),
            cwd: "/repo".into(),
            command: "make".into(),
            user_args: vec!["verify".into()],
            endpoint: crate::adapter::HookEndpoint {
                base_url: "http://127.0.0.1:1".into(),
                token: "t".into(),
                helper_path: None,
            },
            scratch_dir: std::path::PathBuf::from("/tmp/turn-scratch"),
        };
        let plan = GenericTerminalAdapter::new().prepare(&ctx).unwrap();
        assert_eq!(plan.command, "make");
        assert_eq!(plan.args, vec!["verify".to_string()]);
        assert_eq!(plan.level, IntegrationLevel::GenericTerminal);
        // No hook configuration is injected, so nothing can post as this node.
        assert!(!plan.args.iter().any(|a| a.contains("hook")));

        let events = GenericTerminalAdapter::new().normalise(
            &serde_json::json!({ "hook_event_name": "Stop" }),
            &EventContext {
                session_id: SessionId::from_stored("sess_gen01"),
                node_id: NodeId::from_stored("proc_gen01"),
                timestamp_ms: 0,
            },
        );
        assert!(
            events.is_empty(),
            "a terminal Turn understands nothing about must not produce agent events"
        );
    }

    /// An adapter claiming two commands: one every unix has, one no machine has.
    /// The pair is what makes "is it installed" answerable without depending on
    /// which agent CLIs the machine running the tests happens to have.
    struct TwoCommandAdapter;

    impl AgentAdapter for TwoCommandAdapter {
        fn id(&self) -> &'static str {
            "two-command"
        }
        fn provider(&self) -> &'static str {
            "test"
        }
        fn executables(&self) -> &'static [&'static str] {
            &["sh", "turn-absent-tool-xyz"]
        }
        fn best_level(&self) -> IntegrationLevel {
            IntegrationLevel::Heuristic
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities::default()
        }
        fn prepare(&self, ctx: &LaunchContext) -> Result<LaunchPlan, AdapterError> {
            Ok(LaunchPlan {
                command: ctx.command.clone(),
                args: ctx.user_args.clone(),
                env: Vec::new(),
                level: IntegrationLevel::Heuristic,
                note: String::new(),
            })
        }
        fn normalise(&self, _payload: &Value, _ctx: &EventContext) -> Vec<TurnEvent> {
            Vec::new()
        }
    }

    /// A structured adapter for a tool that is definitely not installed, so the
    /// "known but missing" path can be tested without depending on the machine.
    struct AbsentToolAdapter;

    impl AgentAdapter for AbsentToolAdapter {
        fn id(&self) -> &'static str {
            "absent-tool"
        }
        fn provider(&self) -> &'static str {
            "test"
        }
        fn executables(&self) -> &'static [&'static str] {
            &["turn-absent-tool-xyz"]
        }
        fn best_level(&self) -> IntegrationLevel {
            IntegrationLevel::Structured
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities::default()
        }
        fn prepare(&self, ctx: &LaunchContext) -> Result<LaunchPlan, AdapterError> {
            Ok(LaunchPlan {
                command: ctx.command.clone(),
                args: ctx.user_args.clone(),
                env: Vec::new(),
                level: IntegrationLevel::Structured,
                note: String::new(),
            })
        }
        fn normalise(&self, _payload: &Value, _ctx: &EventContext) -> Vec<TurnEvent> {
            Vec::new()
        }
    }
}
