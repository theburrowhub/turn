//! The adapter interface every agent integration implements.
//!
//! An adapter has two jobs: decide *how to launch* a tool so it will report back
//! to Turn, and *translate* whatever it reports into the common event vocabulary.
//! Everything tool-specific lives behind this boundary, which is what lets the
//! attention manager stay ignorant of which CLI produced a given event.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use turn_core::event::TurnEvent;
use turn_core::ids::{NodeId, SessionId};

#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    #[error("could not write adapter configuration: {0}")]
    Config(#[from] std::io::Error),
    #[error("serialising adapter configuration failed: {0}")]
    Serialise(#[from] serde_json::Error),
    #[error("{tool} is not installed or not on PATH")]
    NotInstalled { tool: String },
}

/// How well Turn understands a given tool. The brief's four levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationLevel {
    /// Turn shows the terminal but understands nothing about it. Always available.
    GenericTerminal,
    /// State is inferred from output and the process table.
    Heuristic,
    /// Launched through something Turn controls, which reports lifecycle.
    Wrapper,
    /// The tool reports events itself over a documented contract.
    Structured,
}

impl IntegrationLevel {
    pub fn label(&self) -> &'static str {
        match self {
            IntegrationLevel::GenericTerminal => "terminal only",
            IntegrationLevel::Heuristic => "inferred",
            IntegrationLevel::Wrapper => "wrapped",
            IntegrationLevel::Structured => "native",
        }
    }
}

/// What an adapter can actually do, so the UI never offers an action that will
/// silently do nothing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    /// Reports turn boundaries.
    pub turn_events: bool,
    /// Reports pending permissions before they block.
    pub permission_events: bool,
    /// Reports subagents as they start and stop.
    pub subagent_events: bool,
    /// Can be resumed after the process ends.
    pub resumable: bool,
    /// Reports token usage or cost.
    pub usage_events: bool,
    /// Exposes its own session identifier.
    pub external_session_id: bool,
}

/// Where hook callbacks should be sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookEndpoint {
    /// Base URL of Turn's local hook server, e.g. `http://127.0.0.1:51234`.
    pub base_url: String,
    /// Per-node secret. A request without it is refused, so another process on
    /// the machine cannot forge events for a session.
    pub token: String,
    /// Absolute path to the `turn-hook` helper, for tools that shell out rather
    /// than posting directly.
    pub helper_path: Option<PathBuf>,
}

impl HookEndpoint {
    /// The full URL a hook posts to.
    pub fn url(&self) -> String {
        format!(
            "{}/hook/{}",
            self.base_url.trim_end_matches('/'),
            self.token
        )
    }
}

/// Everything an adapter needs to prepare a launch.
#[derive(Debug, Clone)]
pub struct LaunchContext {
    pub session_id: SessionId,
    pub node_id: NodeId,
    pub cwd: String,
    /// The command the user asked for, e.g. `claude` plus their own flags.
    pub command: String,
    pub user_args: Vec<String>,
    pub endpoint: HookEndpoint,
    /// Directory the adapter may write throwaway configuration into. Turn owns
    /// it and deletes it with the session, so the user's own config is never
    /// touched.
    pub scratch_dir: PathBuf,
}

/// The result of preparing a launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchPlan {
    pub command: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    /// The integration this plan actually achieves, which may be lower than the
    /// adapter's best if something was unavailable.
    pub level: IntegrationLevel,
    /// Human-readable note about what was set up, surfaced in the session
    /// details so the user knows why detection is or is not working.
    pub note: String,
}

/// Context handed to the translator alongside a raw payload.
#[derive(Debug, Clone)]
pub struct EventContext {
    pub session_id: SessionId,
    pub node_id: NodeId,
    pub timestamp_ms: i64,
}

/// A tool integration.
pub trait AgentAdapter: Send + Sync {
    /// Stable identifier, e.g. `claude-code`.
    fn id(&self) -> &'static str;

    /// Vendor, e.g. `anthropic`.
    fn provider(&self) -> &'static str;

    /// Command names this adapter claims.
    fn executables(&self) -> &'static [&'static str];

    /// The best integration this adapter can offer.
    fn best_level(&self) -> IntegrationLevel;

    fn capabilities(&self) -> Capabilities;

    /// Whether this adapter handles a given command line.
    fn handles(&self, command: &str) -> bool {
        let executable = command
            .split_whitespace()
            .next()
            .unwrap_or("")
            .rsplit('/')
            .next()
            .unwrap_or("");
        self.executables().contains(&executable)
    }

    /// Locates the executable for the command line the user typed.
    ///
    /// The name is a parameter because an adapter may claim several: `gemini` and
    /// `aider` are both the heuristic tier's, and answering with whichever of the
    /// nine happens to be installed would report a session as ready to run when
    /// the command the user actually typed does not exist — and would hand the UI
    /// a path to a different program than the one about to be launched.
    ///
    /// An adapter that runs something other than what was typed overrides this.
    fn detect(&self, executable: &str) -> Option<PathBuf> {
        self.executables()
            .contains(&executable)
            .then(|| which(executable))
            .flatten()
    }

    /// Builds the launch plan, writing any throwaway configuration needed.
    fn prepare(&self, ctx: &LaunchContext) -> Result<LaunchPlan, AdapterError>;

    /// Translates one raw callback payload into zero or more events.
    ///
    /// Returning a `Vec` because a single callback can mean two things at once —
    /// a `Notification` both marks the agent as waiting and identifies which
    /// session it came from.
    fn normalise(&self, payload: &serde_json::Value, ctx: &EventContext) -> Vec<TurnEvent>;
}

/// Finds an executable on `PATH`.
///
/// Hand-rolled rather than pulling in a crate: it is a dozen lines, and the
/// behaviour we need (respect PATH, require executability on unix) is exact.
pub fn which(name: &str) -> Option<PathBuf> {
    // An absolute or relative path is used as given.
    if name.contains('/') {
        let path = PathBuf::from(name);
        return is_executable(&path).then_some(path);
    }
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths).find_map(|dir| {
        let candidate = dir.join(name);
        is_executable(&candidate).then_some(candidate)
    })
}

fn is_executable(path: &std::path::Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integration_levels_are_ordered_from_worst_to_best() {
        assert!(IntegrationLevel::Structured > IntegrationLevel::Wrapper);
        assert!(IntegrationLevel::Wrapper > IntegrationLevel::Heuristic);
        assert!(IntegrationLevel::Heuristic > IntegrationLevel::GenericTerminal);
    }

    #[test]
    fn the_hook_url_includes_the_per_node_token() {
        let endpoint = HookEndpoint {
            base_url: "http://127.0.0.1:51234/".into(),
            token: "secret-token".into(),
            helper_path: None,
        };
        assert_eq!(endpoint.url(), "http://127.0.0.1:51234/hook/secret-token");
    }

    #[test]
    fn which_finds_a_real_binary_and_rejects_a_fictional_one() {
        assert!(which("sh").is_some(), "sh must exist on any unix");
        assert!(which("turn-definitely-not-real-xyz").is_none());
    }

    #[test]
    fn which_rejects_a_directory_that_shares_a_binarys_name() {
        // `/tmp` is a directory, not an executable, and must not be returned.
        assert!(!is_executable(std::path::Path::new("/tmp")));
    }
}
