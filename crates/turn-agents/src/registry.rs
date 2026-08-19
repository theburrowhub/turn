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
    LaunchPlan, LaunchProfileDefinition, LaunchProfileRole, ResolvedLaunchProfile,
};
use crate::claude::ClaudeCodeAdapter;
use crate::codex::CodexAdapter;
use crate::gemini::GeminiCliAdapter;
use crate::heuristic::HeuristicAdapter;
use crate::opencode::OpenCodeAdapter;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use turn_core::event::TurnEvent;
use turn_core::model::AgentLaunchProfileRef;

/// Provider-owned launch choices projected from the registry. Consumers can
/// render this without encoding Claude/Codex/Gemini/OpenCode flags themselves.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AdapterLaunchCatalogue {
    pub adapter_id: String,
    pub provider: String,
    pub executables: Vec<String>,
    pub profiles: Vec<LaunchProfileDefinition>,
}

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
        let args = self.resolve_context_launch_profile(ctx)?.args;
        Ok(LaunchPlan {
            command: ctx.command.clone(),
            args,
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
    /// Dedicated structured adapters, then output inference, with the generic
    /// terminal behind all of them.
    pub fn with_builtin() -> Self {
        Self {
            adapters: vec![
                Arc::new(ClaudeCodeAdapter::new()),
                Arc::new(CodexAdapter::new()),
                Arc::new(GeminiCliAdapter::new()),
                Arc::new(OpenCodeAdapter::new()),
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

    /// All adapters that offer an Autonomous profile. Generic/heuristic tools do
    /// not appear as false agent choices merely because their default Safe profile
    /// comes from the trait.
    pub fn launch_catalogue(&self) -> Vec<AdapterLaunchCatalogue> {
        self.adapters
            .iter()
            .filter_map(|adapter| {
                let profiles = adapter.launch_profiles();
                profiles
                    .iter()
                    .any(|profile| profile.role == LaunchProfileRole::Autonomous)
                    .then(|| AdapterLaunchCatalogue {
                        adapter_id: adapter.id().to_string(),
                        provider: adapter.provider().to_string(),
                        executables: adapter
                            .executables()
                            .iter()
                            .map(|executable| (*executable).to_string())
                            .collect(),
                        profiles,
                    })
            })
            .collect()
    }

    /// Resolves a persisted reference without requiring a caller to downcast the
    /// selected adapter or know its flags.
    pub fn resolve_launch_profile(
        &self,
        requested: &AgentLaunchProfileRef,
        user_args: &[String],
    ) -> Result<ResolvedLaunchProfile, AdapterError> {
        let adapter = self.by_id(&requested.adapter_id).ok_or_else(|| {
            AdapterError::UnknownLaunchAdapter {
                adapter_id: requested.adapter_id.clone(),
            }
        })?;
        adapter.resolve_launch_profile(&requested.profile_id, user_args)
    }

    /// Picks the adapter for a command line. Never fails.
    pub fn select(&self, command_line: &str) -> Selection {
        let executable = executable_of(command_line);

        self.select_executable(&executable)
    }

    /// Picks an adapter from one already-structured executable identity.
    ///
    /// Unlike [`Self::select`], this never tokenises the value. Process-table
    /// executable paths may legally contain spaces, and treating such a path as a
    /// shell command both loses its basename and makes mutable argv text authoritative.
    fn select_executable(&self, executable_path: &str) -> Selection {
        let executable = structured_executable_of(executable_path);

        for adapter in &self.adapters {
            if !adapter.handles(&executable) {
                continue;
            }
            let found = adapter.detect(&executable);
            let note = describe(adapter.as_ref(), &executable, found.is_some());
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
            executable: crate::adapter::which(&executable),
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

    /// Picks an adapter from structured process-table identity.
    ///
    /// Agent CLIs installed from npm/pip are often visible to the OS as `node` or
    /// `python`, even though the shell invoked `claude`, `codex`, `gemini` or another
    /// registered executable. Only the wrapper's first script/module operand is
    /// inspected, and only exact path components owned by an adapter are accepted.
    /// Arbitrary prompt arguments are deliberately ignored, so `node app.js --prompt
    /// codex` cannot become a Codex Agent.
    pub fn select_observed(
        &self,
        process_name: &str,
        argv: &[String],
        command_line: &str,
        cwd: Option<&str>,
    ) -> Selection {
        let named = self.select_executable(process_name);
        if named.level >= IntegrationLevel::Heuristic {
            return named;
        }

        let actual_executable = structured_executable_of(process_name);
        let invoked_executable = argv
            .first()
            .map(|argument| structured_executable_of(argument))
            .unwrap_or_default();
        for adapter in &self.adapters {
            if adapter.handles(&invoked_executable)
                && adapter
                    .observed_executable_aliases()
                    .iter()
                    .any(|alias| alias.eq_ignore_ascii_case(&actual_executable))
            {
                return self.select(adapter.executables().first().copied().unwrap_or(""));
            }
        }

        // Some kernels report the canonical interpreter (`/bin/bash`) even when
        // the launched executable was a registered symlink (`/bin/sh`). argv[0]
        // may recover that launch name only when the adapter's detected executable
        // and the kernel executable resolve to the same filesystem target. This
        // preserves legitimate aliases without allowing `exec -a codex node app.js`
        // to turn arbitrary Node code into Codex.
        if let Some(alias) = argv.first() {
            let aliased = self.select_executable(alias);
            if aliased.level >= IntegrationLevel::Heuristic
                && aliased.executable.as_deref().is_some_and(|candidate| {
                    same_executable_target(Path::new(process_name), candidate)
                })
            {
                return aliased;
            }
        }

        // Old stored process events did not carry the executable separately. Keep
        // their replay compatible, but never let argv/command-line spelling overrule
        // a real OS executable identity (for example `exec -a codex node app.js`).
        if process_name.is_empty() {
            return self.select(command_line);
        }

        let wrapper = structured_executable_of(process_name);
        if !matches!(
            wrapper.as_str(),
            "node" | "nodejs" | "bun" | "deno" | "python" | "python3" | "ruby"
        ) {
            return named;
        }
        let Some(subject) = wrapper_subject(&wrapper, argv) else {
            return named;
        };
        for adapter in &self.adapters {
            if adapter_owns_wrapper_subject(adapter.as_ref(), subject, cwd) {
                return self.select(adapter.executables().first().copied().unwrap_or(""));
            }
        }
        named
    }

    /// Whether two interpreter processes execute the exact same wrapper subject.
    ///
    /// Gemini deliberately relaunches its JavaScript bundle in a child Node process.
    /// That child is runtime scaffolding, not a second Agent. Equality is based on the
    /// canonical script/module identity rather than merely the provider name, so a
    /// genuine second Gemini invocation is never swallowed.
    pub fn same_observed_wrapper_subject(
        &self,
        left_executable: &str,
        left_argv: &[String],
        left_cwd: Option<&str>,
        right_executable: &str,
        right_argv: &[String],
        right_cwd: Option<&str>,
    ) -> bool {
        observed_wrapper_identity(left_executable, left_argv, left_cwd)
            .zip(observed_wrapper_identity(
                right_executable,
                right_argv,
                right_cwd,
            ))
            .is_some_and(|(left, right)| left == right)
    }
}

#[derive(Debug, Clone, Copy)]
enum WrapperSubject<'a> {
    Path(&'a str),
    Module(&'a str),
}

fn adapter_owns_wrapper_subject(
    adapter: &dyn AgentAdapter,
    subject: WrapperSubject<'_>,
    cwd: Option<&str>,
) -> bool {
    match subject {
        WrapperSubject::Path(path) => {
            let Some(identity) = normalised_wrapper_path(path, cwd) else {
                return false;
            };
            adapter
                .observed_wrapper_path_suffixes()
                .iter()
                .any(|suffix| path_ends_at_component(&identity, suffix))
        }
        WrapperSubject::Module(module) => adapter.observed_wrapper_modules().contains(&module),
    }
}

fn path_ends_at_component(path: &str, suffix: &str) -> bool {
    if path.len() < suffix.len() {
        return false;
    }
    let (prefix, tail) = path.split_at(path.len() - suffix.len());
    tail.eq_ignore_ascii_case(suffix) && (prefix.is_empty() || prefix.ends_with('/'))
}

fn path_is_absolute_like(path: &str) -> bool {
    Path::new(path).is_absolute()
        || path.starts_with('/')
        || path.starts_with('\\')
        || path
            .as_bytes()
            .get(1)
            .is_some_and(|separator| *separator == b':')
}

fn same_executable_target(observed: &Path, candidate: &Path) -> bool {
    let (Ok(observed), Ok(candidate)) = (
        std::fs::canonicalize(observed),
        std::fs::canonicalize(candidate),
    ) else {
        return false;
    };
    observed == candidate
}

fn normalised_wrapper_path(path: &str, cwd: Option<&str>) -> Option<String> {
    if path.contains("://") {
        return None;
    }
    let candidate = Path::new(path);
    let candidate = if path_is_absolute_like(path) {
        candidate.to_path_buf()
    } else {
        Path::new(cwd?).join(candidate)
    };
    match std::fs::canonicalize(&candidate) {
        Ok(canonical) => Some(canonical.to_string_lossy().replace('\\', "/")),
        // Synthetic/restored absolute paths can describe another platform or a
        // process that has already exited. Relative paths require an existing target.
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound && path_is_absolute_like(path) =>
        {
            // A broken symlink is still an existing, untrusted filesystem object;
            // never fall back to its package-shaped spelling.
            match std::fs::symlink_metadata(&candidate) {
                Err(metadata_error) if metadata_error.kind() == std::io::ErrorKind::NotFound => {
                    Some(path.replace('\\', "/"))
                }
                _ => None,
            }
        }
        Err(_) => None,
    }
}

fn observed_wrapper_identity(
    executable: &str,
    argv: &[String],
    cwd: Option<&str>,
) -> Option<String> {
    let wrapper = structured_executable_of(executable);
    if !matches!(
        wrapper.as_str(),
        "node" | "nodejs" | "bun" | "deno" | "python" | "python3" | "ruby"
    ) {
        return None;
    }
    let subject = wrapper_subject(&wrapper, argv)?;
    let subject = match subject {
        WrapperSubject::Path(path) => normalised_wrapper_path(path, cwd)?,
        WrapperSubject::Module(module) => format!("module:{module}"),
    };
    Some(format!("{wrapper}\0{}", subject.to_ascii_lowercase()))
}

/// Returns only an operand the wrapper itself will execute.
///
/// Unknown option grammar fails closed. Treating an option value or `-c` source as a
/// script would let arbitrary prompt/config text become Agent identity.
fn wrapper_subject<'a>(wrapper: &str, argv: &'a [String]) -> Option<WrapperSubject<'a>> {
    let mut args = argv;
    if args
        .first()
        .is_some_and(|argument| structured_executable_of(argument) == wrapper)
    {
        args = &args[1..];
    }
    match wrapper {
        "node" | "nodejs" => script_after_options(
            args,
            &[
                "-r",
                "--require",
                "--import",
                "--loader",
                "--experimental-loader",
            ],
            &["-e", "--eval", "-p", "--print", "-c", "--check"],
        )
        .filter(|candidate| path_like_script(candidate))
        .map(WrapperSubject::Path),
        "bun" => {
            if matches!(args.first().map(String::as_str), Some("run" | "x" | "exec")) {
                None
            } else {
                script_after_options(args, &["--cwd", "--config"], &["-e", "--eval"])
                    .filter(|candidate| path_like_script(candidate))
                    .map(WrapperSubject::Path)
            }
        }
        "deno" => {
            let rest = args.strip_prefix(&["run".to_string()])?;
            script_after_options(
                rest,
                &[
                    "--config",
                    "--import-map",
                    "--cert",
                    "--location",
                    "--v8-flags",
                ],
                &[],
            )
            .filter(|candidate| path_like_script(candidate))
            .map(WrapperSubject::Path)
        }
        "python" | "python3" => python_subject(args),
        "ruby" => script_after_options(args, &["-I", "-r"], &["-e"])
            .filter(|candidate| path_like_script(candidate))
            .map(WrapperSubject::Path),
        _ => None,
    }
}

fn script_after_options<'a>(
    args: &'a [String],
    options_with_value: &[&str],
    code_options: &[&str],
) -> Option<&'a str> {
    let mut index = 0;
    while index < args.len() {
        let argument = args[index].as_str();
        if argument == "--" {
            return args.get(index + 1).map(String::as_str);
        }
        if code_options
            .iter()
            .any(|option| is_inline_or_separate_code_option(argument, option))
        {
            return None;
        }
        if options_with_value.contains(&argument) {
            index += 2;
            continue;
        }
        if argument.starts_with('-') {
            // Long `--flag=value` and common boolean runtime flags are self-contained.
            // Any other unknown option could consume the following token, so stop.
            if argument.contains('=')
                || matches!(
                    argument,
                    "--quiet" | "--no-check" | "--unstable" | "--watch" | "-B" | "-E" | "-s"
                )
            {
                index += 1;
                continue;
            }
            return None;
        }
        return Some(argument);
    }
    None
}

fn is_inline_or_separate_code_option(argument: &str, option: &str) -> bool {
    if argument == option {
        return true;
    }
    let Some(rest) = argument.strip_prefix(option) else {
        return false;
    };
    if option.starts_with("--") {
        rest.starts_with('=')
    } else {
        !rest.is_empty()
    }
}

fn python_subject(args: &[String]) -> Option<WrapperSubject<'_>> {
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--" => {
                return args
                    .get(index + 1)
                    .map(String::as_str)
                    .map(WrapperSubject::Path)
            }
            "-c" => return None,
            "-m" => {
                return args
                    .get(index + 1)
                    .map(String::as_str)
                    .map(WrapperSubject::Module)
            }
            "-W" | "-X" => index += 2,
            "-B" | "-E" | "-I" | "-O" | "-OO" | "-P" | "-q" | "-s" | "-S" | "-u" | "-v" => {
                index += 1
            }
            argument if argument.starts_with('-') => return None,
            argument => {
                return path_like_script(argument).then_some(WrapperSubject::Path(argument))
            }
        }
    }
    None
}

fn path_like_script(candidate: &str) -> bool {
    candidate.contains(['/', '\\'])
        || [".js", ".mjs", ".cjs", ".ts", ".py", ".rb"]
            .iter()
            .any(|extension| candidate.ends_with(extension))
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
pub fn executable_of(command_line: &str) -> String {
    let executable = command_line
        .split_whitespace()
        .find(|token| !is_env_assignment(token))
        .unwrap_or("")
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("");
    let executable = executable
        .len()
        .checked_sub(4)
        .filter(|stem| *stem > 0)
        .and_then(|stem| {
            executable
                .get(stem..)
                .is_some_and(|suffix| suffix.eq_ignore_ascii_case(".exe"))
                .then_some(stem)
        })
        .and_then(|stem| executable.get(..stem))
        .unwrap_or(executable);
    executable.to_ascii_lowercase()
}

/// Basename of an OS-provided executable path, preserving spaces inside the path.
fn structured_executable_of(path: &str) -> String {
    let executable = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let executable = executable
        .len()
        .checked_sub(4)
        .filter(|stem| *stem > 0)
        .and_then(|stem| {
            executable
                .get(stem..)
                .is_some_and(|suffix| suffix.eq_ignore_ascii_case(".exe"))
                .then_some(stem)
        })
        .and_then(|stem| executable.get(..stem))
        .unwrap_or(executable);
    executable.to_ascii_lowercase()
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
    use crate::adapter::{LaunchPermissionPosture, AUTONOMOUS_PROFILE_ID, SAFE_PROFILE_ID};
    use turn_core::ids::{NodeId, SessionId};

    fn registry() -> AdapterRegistry {
        AdapterRegistry::with_builtin()
    }

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn the_launch_catalogue_is_provider_owned_and_semantic() {
        let catalogue = registry().launch_catalogue();
        assert_eq!(
            catalogue
                .iter()
                .map(|entry| entry.adapter_id.as_str())
                .collect::<Vec<_>>(),
            vec!["claude-code", "codex", "gemini-cli", "opencode"]
        );
        for entry in &catalogue {
            assert_eq!(
                entry
                    .profiles
                    .iter()
                    .map(|profile| profile.id.as_str())
                    .collect::<Vec<_>>(),
                vec![SAFE_PROFILE_ID, AUTONOMOUS_PROFILE_ID],
                "{} must expose product choices rather than CLI flags",
                entry.adapter_id
            );
            assert_eq!(entry.profiles[0].role, LaunchProfileRole::Safe);
            assert_eq!(entry.profiles[1].role, LaunchProfileRole::Autonomous);
        }
        let opencode = catalogue
            .iter()
            .find(|entry| entry.adapter_id == "opencode")
            .unwrap();
        assert_eq!(
            opencode.profiles[1].posture,
            LaunchPermissionPosture::AutoApproveUnlessDenied
        );
        assert!(opencode.profiles[1].description.contains("deny"));

        let encoded = serde_json::to_string(&catalogue).unwrap();
        assert_eq!(
            serde_json::from_str::<Vec<AdapterLaunchCatalogue>>(&encoded).unwrap(),
            catalogue
        );
    }

    #[test]
    fn claude_profiles_translate_exactly_and_refuse_ambiguous_policy() {
        let registry = registry();
        let autonomous = AgentLaunchProfileRef::new("claude-code", AUTONOMOUS_PROFILE_ID);
        let resolved = registry
            .resolve_launch_profile(&autonomous, &args(&["--model", "opus"]))
            .unwrap();
        assert_eq!(
            resolved.args,
            args(&["--dangerously-skip-permissions", "--model", "opus"])
        );
        assert_eq!(resolved.posture, LaunchPermissionPosture::BypassPermissions);

        let already_present = args(&["--dangerously-skip-permissions"]);
        let resolved = registry
            .resolve_launch_profile(&autonomous, &already_present)
            .unwrap();
        assert_eq!(
            resolved.args, already_present,
            "the flag must not be doubled"
        );

        let equivalent = args(&["--permission-mode=bypassPermissions"]);
        assert_eq!(
            registry
                .resolve_launch_profile(&autonomous, &equivalent)
                .unwrap()
                .args,
            equivalent
        );
        assert!(matches!(
            registry
                .resolve_launch_profile(&autonomous, &args(&["--permission-mode", "acceptEdits"])),
            Err(AdapterError::LaunchProfileConflict { .. })
        ));
        assert!(matches!(
            registry.resolve_launch_profile(
                &AgentLaunchProfileRef::new("claude-code", SAFE_PROFILE_ID),
                &args(&["--dangerously-skip-permissions"])
            ),
            Err(AdapterError::LaunchProfileConflict { .. })
        ));
    }

    #[test]
    fn codex_profiles_bypass_only_approvals_and_sandbox() {
        let registry = registry();
        let autonomous = AgentLaunchProfileRef::new("codex", AUTONOMOUS_PROFILE_ID);
        let resolved = registry
            .resolve_launch_profile(&autonomous, &args(&["--model", "gpt-5.6-codex"]))
            .unwrap();
        assert_eq!(
            resolved.args,
            args(&[
                "--dangerously-bypass-approvals-and-sandbox",
                "--model",
                "gpt-5.6-codex",
            ])
        );
        assert_eq!(
            resolved.posture,
            LaunchPermissionPosture::BypassApprovalsAndSandbox
        );
        assert!(!resolved
            .args
            .iter()
            .any(|arg| arg == "--dangerously-bypass-hook-trust"));

        let already_present = args(&["--dangerously-bypass-approvals-and-sandbox"]);
        assert_eq!(
            registry
                .resolve_launch_profile(&autonomous, &already_present)
                .unwrap()
                .args,
            already_present
        );
        assert!(matches!(
            registry.resolve_launch_profile(&autonomous, &args(&["--ask-for-approval", "never"])),
            Err(AdapterError::LaunchProfileConflict { .. })
        ));
        assert!(matches!(
            registry.resolve_launch_profile(
                &AgentLaunchProfileRef::new("codex", SAFE_PROFILE_ID),
                &args(&["--sandbox=danger-full-access"])
            ),
            Err(AdapterError::LaunchProfileConflict { .. })
        ));
    }

    #[test]
    fn gemini_profiles_use_the_typed_yolo_approval_mode() {
        let registry = registry();
        let autonomous = AgentLaunchProfileRef::new("gemini-cli", AUTONOMOUS_PROFILE_ID);
        let resolved = registry
            .resolve_launch_profile(&autonomous, &args(&["--model", "gemini-3"]))
            .unwrap();
        assert_eq!(
            resolved.args,
            args(&["--approval-mode", "yolo", "--model", "gemini-3"])
        );
        assert_eq!(resolved.posture, LaunchPermissionPosture::YoloApprovalMode);
        for equivalent in [
            args(&["--yolo"]),
            args(&["-y"]),
            args(&["--approval-mode=yolo"]),
        ] {
            assert_eq!(
                registry
                    .resolve_launch_profile(&autonomous, &equivalent)
                    .unwrap()
                    .args,
                equivalent
            );
        }
        assert!(matches!(
            registry.resolve_launch_profile(&autonomous, &args(&["--approval-mode", "plan"])),
            Err(AdapterError::LaunchProfileConflict { .. })
        ));
    }

    #[test]
    fn opencode_autonomous_keeps_explicit_denies_and_uses_auto_once() {
        let registry = registry();
        let autonomous = AgentLaunchProfileRef::new("opencode", AUTONOMOUS_PROFILE_ID);
        let resolved = registry
            .resolve_launch_profile(&autonomous, &args(&["--model", "openai/gpt-5.2"]))
            .unwrap();
        assert_eq!(
            resolved.args,
            args(&["--auto", "--model", "openai/gpt-5.2"])
        );
        assert_eq!(
            resolved.posture,
            LaunchPermissionPosture::AutoApproveUnlessDenied
        );
        let already_present = args(&["--auto"]);
        assert_eq!(
            registry
                .resolve_launch_profile(&autonomous, &already_present)
                .unwrap()
                .args,
            already_present
        );
        assert!(matches!(
            registry.resolve_launch_profile(
                &AgentLaunchProfileRef::new("opencode", SAFE_PROFILE_ID),
                &args(&["--auto"])
            ),
            Err(AdapterError::LaunchProfileConflict { .. })
        ));
    }

    #[test]
    fn every_autonomous_profile_keeps_prompt_like_flags_literal_after_the_terminator() {
        struct Case {
            adapter: &'static str,
            generated_flag: &'static str,
            generated_value: Option<&'static str>,
            literal_policy: &'static [&'static str],
        }
        let cases = [
            Case {
                adapter: "claude-code",
                generated_flag: "--dangerously-skip-permissions",
                generated_value: None,
                literal_policy: &["--permission-mode", "plan"],
            },
            Case {
                adapter: "codex",
                generated_flag: "--dangerously-bypass-approvals-and-sandbox",
                generated_value: None,
                literal_policy: &["--ask-for-approval", "never"],
            },
            Case {
                adapter: "gemini-cli",
                generated_flag: "--approval-mode",
                generated_value: Some("yolo"),
                literal_policy: &["--approval-mode", "plan"],
            },
            Case {
                adapter: "opencode",
                generated_flag: "--auto",
                generated_value: None,
                literal_policy: &["--auto", "--pure"],
            },
        ];
        let registry = registry();

        for case in cases {
            let mut requested = args(&["--model", "provider/model", "--", "literal prompt"]);
            requested.extend(case.literal_policy.iter().map(|value| (*value).to_string()));
            requested.extend(args(&[
                "--",
                "second literal terminator",
                "--model",
                "not-the-launch-model",
            ]));
            let prompt_start = requested.iter().position(|arg| arg == "--").unwrap();
            let resolved = registry
                .resolve_launch_profile(
                    &AgentLaunchProfileRef::new(case.adapter, AUTONOMOUS_PROFILE_ID),
                    &requested,
                )
                .unwrap_or_else(|error| panic!("{}: {error}", case.adapter));
            let effective_prompt_start = resolved
                .args
                .iter()
                .position(|arg| arg == "--")
                .expect("the literal prompt boundary");

            assert!(
                resolved.args.ends_with(&requested),
                "{} changed the requested argv: {:?}",
                case.adapter,
                resolved.args
            );
            assert_eq!(
                &resolved.args[effective_prompt_start..],
                &requested[prompt_start..],
                "{} reinterpreted or changed literal prompt argv",
                case.adapter
            );
            let flag_index = resolved
                .args
                .iter()
                .position(|arg| arg == case.generated_flag)
                .expect("the autonomous control");
            assert!(
                flag_index < effective_prompt_start,
                "{} placed its control in the prompt: {:?}",
                case.adapter,
                resolved.args
            );
            if let Some(value) = case.generated_value {
                assert_eq!(
                    resolved.args.get(flag_index + 1).map(String::as_str),
                    Some(value)
                );
            }
        }
    }

    #[test]
    fn repeated_existing_autonomous_controls_are_preserved_without_new_copies() {
        let cases = [
            (
                "claude-code",
                args(&[
                    "--dangerously-skip-permissions",
                    "--dangerously-skip-permissions",
                    "--",
                    "prompt",
                ]),
            ),
            (
                "codex",
                args(&[
                    "--dangerously-bypass-approvals-and-sandbox",
                    "--dangerously-bypass-approvals-and-sandbox",
                    "--",
                    "prompt",
                ]),
            ),
            (
                "gemini-cli",
                args(&[
                    "--approval-mode=yolo",
                    "--approval-mode",
                    "yolo",
                    "--",
                    "prompt",
                ]),
            ),
            ("opencode", args(&["--auto", "--auto", "--", "prompt"])),
        ];
        let registry = registry();
        for (adapter, requested) in cases {
            let resolved = registry
                .resolve_launch_profile(
                    &AgentLaunchProfileRef::new(adapter, AUTONOMOUS_PROFILE_ID),
                    &requested,
                )
                .unwrap_or_else(|error| panic!("{adapter}: {error}"));
            assert_eq!(resolved.args, requested, "{adapter} rewrote repeated argv");
        }
    }

    #[test]
    fn unknown_adapters_and_profiles_are_refused() {
        assert!(matches!(
            registry().resolve_launch_profile(
                &AgentLaunchProfileRef::new("unknown", SAFE_PROFILE_ID),
                &[]
            ),
            Err(AdapterError::UnknownLaunchAdapter { .. })
        ));
        assert!(matches!(
            registry()
                .resolve_launch_profile(&AgentLaunchProfileRef::new("codex", "reckless-ish"), &[]),
            Err(AdapterError::UnknownLaunchProfile { .. })
        ));
    }

    #[test]
    fn legacy_arguments_remain_custom_and_adapter_mismatches_fail_closed() {
        let adapter = ClaudeCodeAdapter::new();
        let mut ctx = LaunchContext {
            session_id: SessionId::from_stored("sess_profile_legacy"),
            node_id: NodeId::from_stored("proc_profile_legacy"),
            cwd: "/repo".into(),
            command: "claude".into(),
            user_args: args(&["--dangerously-skip-permissions"]),
            launch_profile: None,
            endpoint: crate::adapter::HookEndpoint {
                base_url: "http://127.0.0.1:1".into(),
                token: "t".into(),
                helper_path: None,
            },
            scratch_dir: std::path::PathBuf::from("/tmp/turn-profile-legacy"),
        };
        let legacy = adapter.resolve_context_launch_profile(&ctx).unwrap();
        assert_eq!(legacy.profile_id, "custom");
        assert_eq!(legacy.posture, LaunchPermissionPosture::Custom);
        assert_eq!(legacy.args, ctx.user_args);

        ctx.launch_profile = Some(AgentLaunchProfileRef::new("codex", AUTONOMOUS_PROFILE_ID));
        assert!(matches!(
            adapter.resolve_context_launch_profile(&ctx),
            Err(AdapterError::LaunchProfileAdapterMismatch { .. })
        ));
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
    fn gemini_and_opencode_have_dedicated_structured_adapters() {
        let selection = registry().select("gemini");
        assert_eq!(selection.adapter.id(), "gemini-cli");
        assert_eq!(selection.level, IntegrationLevel::Structured);
        assert!(selection.capabilities.permission_events);
        assert!(selection.capabilities.resumable);

        let selection = registry().select("opencode");
        assert_eq!(selection.adapter.id(), "opencode");
        assert_eq!(selection.level, IntegrationLevel::Structured);
        assert!(selection.capabilities.subagent_events);
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
        assert_eq!(executable_of(r"C:\Tools\CLAUDE.EXE"), "claude");
        assert_eq!(executable_of("node.exe"), "node");
        assert_eq!(executable_of("ééa"), "ééa");
        assert_eq!(executable_of("工具.EXE"), "工具");
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
    fn observed_script_wrappers_resolve_only_exact_adapter_owned_paths() {
        let registry = AdapterRegistry::with_builtin();
        for (path, adapter) in [
            (
                "/opt/lib/node_modules/@anthropic-ai/claude-code/cli-wrapper.cjs",
                "claude-code",
            ),
            (
                "/opt/lib/node_modules/@anthropic-ai/claude-code/cli.js",
                "claude-code",
            ),
            ("/opt/lib/node_modules/@openai/codex/bin/codex.js", "codex"),
            (
                "/opt/lib/node_modules/@google/gemini-cli/bundle/gemini.js",
                "gemini-cli",
            ),
            ("/opt/lib/node_modules/opencode-ai/bin/opencode", "opencode"),
        ] {
            let argv = vec!["node".to_string(), path.to_string(), "--flag".to_string()];
            assert_eq!(
                registry
                    .select_observed("node", &argv, &argv.join(" "), None)
                    .adapter
                    .id(),
                adapter,
                "wrapper path {path}"
            );
        }

        for (process, adapter) in [
            (
                r"C:\npm\node_modules\@anthropic-ai\claude-code\bin\CLAUDE.EXE",
                "claude-code",
            ),
            (
                r"C:\npm\node_modules\@openai\codex\vendor\x86_64-pc-windows-msvc\bin\CODEX.EXE",
                "codex",
            ),
            (
                r"C:\npm\node_modules\opencode-ai\bin\OPENCODE.EXE",
                "opencode",
            ),
        ] {
            assert_eq!(
                registry
                    .select_observed(process, &[], process, None)
                    .adapter
                    .id(),
                adapter,
                "native Windows executable {process}"
            );
        }

        let unrelated = vec![
            "node".to_string(),
            "/repo/app.js".to_string(),
            "--prompt".to_string(),
            "codex".to_string(),
        ];
        assert_eq!(
            registry
                .select_observed("node", &unrelated, &unrelated.join(" "), None)
                .level,
            IntegrationLevel::GenericTerminal,
            "an argument that merely mentions an Agent is not executable identity"
        );

        for argv in [
            vec!["node", "--require", "codex", "/repo/app.js"],
            vec![
                "node",
                "--eval='0'",
                "/opt/lib/node_modules/@openai/codex/bin/codex.js",
            ],
            vec![
                "node",
                "--print=process.version",
                "/opt/lib/node_modules/@openai/codex/bin/codex.js",
            ],
            vec![
                "node.exe",
                "-e0",
                r"C:\npm\node_modules\@openai\codex\bin\codex.js",
            ],
            vec!["python", "-c", "claude"],
            vec!["bun", "run", "opencode"],
            vec!["node", "/repo/codex/app.js"],
            vec!["node", "/repo/claude/index.js"],
            vec!["node", "/repo/gemini-cli/tool.js"],
            vec!["node", "/tmp/opencode/bin.js"],
            vec!["node", "/tmp/evilnode_modules/@openai/codex/bin/codex.js"],
            vec!["python3", "-m", "codex"],
        ] {
            let argv: Vec<String> = argv.into_iter().map(str::to_string).collect();
            assert_eq!(
                registry
                    .select_observed(&argv[0], &argv, &argv.join(" "), None)
                    .level,
                IntegrationLevel::GenericTerminal,
                "an option value or package-script name is not executable identity: {argv:?}"
            );
        }

        let deno = vec![
            "deno".to_string(),
            "run".to_string(),
            "/opt/lib/node_modules/@google/gemini-cli/dist/index.js".to_string(),
        ];
        assert_eq!(
            registry
                .select_observed("deno", &deno, &deno.join(" "), None)
                .adapter
                .id(),
            "gemini-cli"
        );

        let spoofed_argv = vec![
            "codex".to_string(),
            "/repo/app.js".to_string(),
            "--prompt".to_string(),
            "codex".to_string(),
        ];
        assert_eq!(
            registry
                .select_observed(
                    "/usr/local/bin/node",
                    &spoofed_argv,
                    &spoofed_argv.join(" "),
                    None,
                )
                .level,
            IntegrationLevel::GenericTerminal,
            "argv[0] cannot overrule the kernel executable identity"
        );

        assert_eq!(
            registry
                .select_observed(
                    "/Applications/Agent Tools/codex",
                    &["codex".into()],
                    "codex",
                    None,
                )
                .adapter
                .id(),
            "codex",
            "spaces inside an OS executable path are not shell token boundaries"
        );
    }

    #[cfg(unix)]
    #[test]
    fn observed_node_shims_are_identified_from_their_canonical_package_target() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let registry = AdapterRegistry::with_builtin();

        for (shim_name, package_path, expected) in [
            (
                "codex",
                "lib/node_modules/@openai/codex/bin/codex.js",
                "codex",
            ),
            (
                "gemini",
                "lib/node_modules/@google/gemini-cli/bundle/gemini.js",
                "gemini-cli",
            ),
        ] {
            let target = root.path().join(package_path);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(&target, "// package-owned fixture").unwrap();
            let shim = bin.join(shim_name);
            symlink(&target, &shim).unwrap();
            let argv = vec![
                "node".to_string(),
                shim.display().to_string(),
                "--yolo".to_string(),
            ];
            assert_eq!(
                registry
                    .select_observed("node", &argv, &argv.join(" "), None)
                    .adapter
                    .id(),
                expected,
                "the package-manager shim must resolve to {package_path}"
            );
        }

        let unrelated = root.path().join("evil.js");
        std::fs::write(&unrelated, "// unrelated fixture").unwrap();
        let misleading = bin.join("codex-unrelated");
        symlink(&unrelated, &misleading).unwrap();
        let argv = vec!["node".to_string(), misleading.display().to_string()];
        assert_eq!(
            registry
                .select_observed("node", &argv, &argv.join(" "), None)
                .level,
            IntegrationLevel::GenericTerminal,
            "a shim name is not evidence when its canonical target is unrelated"
        );

        let package_spelling = root.path().join("node_modules/@openai/codex/bin/codex.js");
        std::fs::create_dir_all(package_spelling.parent().unwrap()).unwrap();
        symlink(&unrelated, &package_spelling).unwrap();
        let argv = vec!["node".to_string(), package_spelling.display().to_string()];
        assert_eq!(
            registry
                .select_observed("node", &argv, &argv.join(" "), None)
                .level,
            IntegrationLevel::GenericTerminal,
            "an exact package-shaped symlink must be judged by its canonical target"
        );
    }

    #[test]
    fn wrapper_relaunch_identity_requires_the_same_canonical_subject() {
        let registry = AdapterRegistry::with_builtin();
        let outer = vec![
            "node".to_string(),
            "/opt/lib/node_modules/@google/gemini-cli/bundle/gemini.js".to_string(),
            "--yolo".to_string(),
        ];
        let child = vec![
            "/usr/local/bin/node".to_string(),
            "/opt/lib/node_modules/@google/gemini-cli/bundle/gemini.js".to_string(),
            "--yolo".to_string(),
        ];
        assert!(registry.same_observed_wrapper_subject(
            "/usr/local/bin/node",
            &outer,
            None,
            "/usr/local/bin/node",
            &child,
            None,
        ));

        let other = vec![
            "node".to_string(),
            "/another/lib/node_modules/@google/gemini-cli/bundle/gemini.js".to_string(),
        ];
        assert!(
            !registry.same_observed_wrapper_subject("node", &outer, None, "node", &other, None,)
        );
    }

    #[test]
    fn relative_wrapper_scripts_are_resolved_against_the_observed_process_cwd() {
        let root = tempfile::tempdir().unwrap();
        let package = root.path().join("node_modules/@google/gemini-cli");
        let script = package.join("dist/index.js");
        std::fs::create_dir_all(script.parent().unwrap()).unwrap();
        std::fs::write(&script, "// package-owned fixture").unwrap();
        let argv = vec!["node".to_string(), "dist/index.js".to_string()];
        let registry = AdapterRegistry::with_builtin();
        assert_eq!(
            registry
                .select_observed("node", &argv, &argv.join(" "), package.to_str(),)
                .adapter
                .id(),
            "gemini-cli"
        );

        let application = root.path().join("repo/app");
        let unrelated = application.join("dist/index.js");
        std::fs::create_dir_all(unrelated.parent().unwrap()).unwrap();
        std::fs::write(unrelated, "// unrelated fixture").unwrap();
        assert_eq!(
            registry
                .select_observed("node", &argv, &argv.join(" "), application.to_str(),)
                .level,
            IntegrationLevel::GenericTerminal
        );
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
        let gemini = registry().select("gemini");
        assert_eq!(gemini.adapter.id(), "gemini-cli");
        if let Some(path) = &gemini.executable {
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
            launch_profile: None,
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
