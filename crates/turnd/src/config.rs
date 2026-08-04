//! Everything the daemon needs to know before it starts.

use crate::error::{DaemonError, Result};
use crate::paths;
use std::path::{Path, PathBuf};
use turn_agents::AdapterRegistry;

/// Resolved start-up settings.
///
/// Built either from the command line ([`Config::from_options`]) or directly
/// ([`Config::in_dir`]). Constructing it never touches the filesystem, so a caller
/// can inspect what would happen before it happens.
pub struct Config {
    /// Where the database and the scratch space live.
    pub data_dir: PathBuf,
    /// The unix socket clients connect to.
    pub socket_path: PathBuf,
    /// Whether state is written to disk at all. `false` gives an in-memory store,
    /// which is what `--no-persist` and most tests want.
    pub persist: bool,
    /// The adapters this daemon will select from.
    ///
    /// A field rather than a constant so integrations can be turned off, and so a
    /// test can register an adapter of its own instead of launching the user's real
    /// agent — which would mean a test suite that talks to a paid API.
    pub registry: AdapterRegistry,
    /// Absolute path to the `turn-hook` helper, for tools that shell out rather
    /// than posting to Turn directly.
    pub hook_helper: Option<PathBuf>,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("data_dir", &self.data_dir)
            .field("socket_path", &self.socket_path)
            .field("persist", &self.persist)
            .field("adapters", &self.registry.adapters().len())
            .field("hook_helper", &self.hook_helper)
            .finish()
    }
}

impl Config {
    /// Resolves the data directory and socket the way a real start-up does,
    /// honouring `TURN_DATA_DIR` and `TURN_SOCKET`.
    pub fn from_options(options: &crate::options::Options) -> Result<Self> {
        let data_dir = match &options.data_dir {
            Some(dir) => dir.clone(),
            None => turn_store::location::default_data_dir().map_err(DaemonError::Store)?,
        };
        let socket_dir = paths::socket_dir(&data_dir);
        let socket_path = paths::socket_path_from_env(options.socket.as_deref(), &socket_dir);
        Ok(Self {
            data_dir,
            socket_path,
            persist: !options.no_persist,
            registry: AdapterRegistry::with_builtin(),
            hook_helper: locate_hook_helper(),
        })
    }

    /// A self-contained daemon whose socket, database and scratch space all live in
    /// one directory. This is what a test wants: no environment, no shared state.
    pub fn in_dir(dir: impl AsRef<Path>) -> Self {
        let dir = dir.as_ref().to_path_buf();
        Self {
            socket_path: dir.join(paths::SOCKET_FILE),
            data_dir: dir,
            persist: true,
            registry: AdapterRegistry::with_builtin(),
            hook_helper: locate_hook_helper(),
        }
    }

    /// Replaces the adapter registry.
    pub fn with_registry(mut self, registry: AdapterRegistry) -> Self {
        self.registry = registry;
        self
    }

    pub fn without_persistence(mut self) -> Self {
        self.persist = false;
        self
    }
}

/// Finds the `turn-hook` helper next to the running binary.
///
/// Looked for beside `turnd` rather than on `PATH`: the two are built and shipped
/// together, and picking up a different build's helper would point hooks at a URL
/// scheme that may have moved on. `None` is not fatal — adapters that need it
/// degrade and say so.
fn locate_hook_helper() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let candidate = exe.with_file_name("turn-hook");
    candidate.is_file().then_some(candidate)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_directory_config_keeps_everything_inside_that_directory() {
        let config = Config::in_dir("/tmp/turn-test");
        assert_eq!(config.data_dir, PathBuf::from("/tmp/turn-test"));
        assert_eq!(
            config.socket_path,
            PathBuf::from("/tmp/turn-test/turnd.sock")
        );
        assert!(config.persist);
    }

    #[test]
    fn the_registry_is_replaceable_so_tests_need_not_launch_a_real_agent() {
        let config = Config::in_dir("/tmp/turn-test").with_registry(AdapterRegistry::bare());
        assert!(
            config.registry.adapters().is_empty(),
            "a bare registry keeps only the fallback"
        );
        // The fallback is still reachable, so an unrecognised command still runs.
        assert_eq!(
            config.registry.select("zsh").adapter.id(),
            "generic-terminal"
        );
    }
}
