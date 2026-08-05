//! Finding the daemon's socket.
//!
//! This mirrors `turnd::paths` deliberately rather than importing it: the daemon
//! binary is not a library, and a window that could only find a socket by linking
//! the daemon would stop being a separate process in any meaningful sense. The
//! precedence rules are the same three in the same order, and they are written as
//! pure functions over their inputs so a test can check them without mutating
//! environment variables that every other test in the binary can see.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// Overrides the socket location. The same variable `turnd` reads.
pub const SOCKET_ENV: &str = "TURN_SOCKET";
/// Persistent-state override. The daemon reads the same variable.
pub const DATA_DIR_ENV: &str = "TURN_DATA_DIR";

/// The socket file inside the runtime (or data) directory.
pub const SOCKET_FILE: &str = "turnd.sock";

/// Paths resolved once at process start and then passed explicitly to `turnd`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartupPaths {
    pub data_dir: PathBuf,
    pub socket: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum PathResolutionError {
    #[error("no platform data directory could be resolved; set TURN_DATA_DIR")]
    NoDataDir,
    #[error("could not resolve the current directory needed for relative Turn paths: {0}")]
    CurrentDirectory(#[source] std::io::Error),
}

/// Resolves the socket path from an explicit override, the environment, and the
/// directory to fall back to.
///
/// A variable set to whitespace counts as unset. An exported but blank
/// `TURN_SOCKET` is a shell accident, and honouring it would have the window try to
/// connect to the current directory and report "no daemon" for ever.
pub fn resolve_socket_path(
    explicit: Option<&Path>,
    env_value: Option<&OsStr>,
    dir: &Path,
) -> PathBuf {
    if let Some(path) = explicit {
        return path.to_path_buf();
    }
    if let Some(value) = env_value {
        if !value.to_string_lossy().trim().is_empty() {
            return PathBuf::from(value);
        }
    }
    dir.join(SOCKET_FILE)
}

/// The directory the socket belongs in: the platform runtime directory where there
/// is one, otherwise the data directory. Matches `turnd::paths::socket_dir`.
pub fn socket_dir_for(data_dir: &Path) -> PathBuf {
    directories::ProjectDirs::from("dev", "turn", "turn")
        .and_then(|dirs| dirs.runtime_dir().map(Path::to_path_buf))
        .unwrap_or_else(|| data_dir.to_path_buf())
}

/// Resolves data and socket paths once. Relative overrides are anchored before a
/// source fallback can change directory, and the exact absolute values are passed to
/// the child. Unlike an implicit `/tmp` fallback, failure to find persistent storage is
/// visible and cannot silently put a user's session database in a shared directory.
pub fn startup_paths(explicit_socket: Option<&Path>) -> Result<StartupPaths, PathResolutionError> {
    let configured_data = std::env::var_os(DATA_DIR_ENV);
    let default_data = directories::ProjectDirs::from("dev", "turn", "turn")
        .map(|dirs| dirs.data_dir().to_path_buf());
    let chosen_data = configured_data
        .as_deref()
        .filter(|value| !value.to_string_lossy().trim().is_empty())
        .map(PathBuf::from)
        .or(default_data)
        .ok_or(PathResolutionError::NoDataDir)?;
    let configured_socket = std::env::var_os(SOCKET_ENV);
    let raw_socket = resolve_socket_path(
        explicit_socket,
        configured_socket.as_deref(),
        &socket_dir_for(&chosen_data),
    );
    let needs_cwd = chosen_data.is_relative() || raw_socket.is_relative();
    let cwd = needs_cwd
        .then(std::env::current_dir)
        .transpose()
        .map_err(PathResolutionError::CurrentDirectory)?;
    Ok(resolve_startup_paths(
        chosen_data,
        raw_socket,
        cwd.as_deref(),
    ))
}

fn resolve_startup_paths(data_dir: PathBuf, socket: PathBuf, cwd: Option<&Path>) -> StartupPaths {
    let absolute = |path: PathBuf| {
        if path.is_absolute() {
            path
        } else {
            cwd.expect("relative paths require a current directory")
                .join(path)
        }
    };
    StartupPaths {
        data_dir: absolute(data_dir),
        socket: absolute(socket),
    }
}

/// The directory this process uses for its default socket.
pub fn socket_dir() -> Result<PathBuf, PathResolutionError> {
    startup_paths(None).map(|paths| socket_dir_for(&paths.data_dir))
}

/// The socket this process will connect to, reading `TURN_SOCKET`.
pub fn socket_path_from_env() -> Result<PathBuf, PathResolutionError> {
    startup_paths(None).map(|paths| paths.socket)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn an_explicit_path_beats_both_the_environment_and_the_default() {
        let chosen = resolve_socket_path(
            Some(Path::new("/tmp/explicit.sock")),
            Some(OsStr::new("/tmp/from-env.sock")),
            Path::new("/var/run/turn"),
        );
        assert_eq!(chosen, PathBuf::from("/tmp/explicit.sock"));
    }

    #[test]
    fn the_environment_beats_the_default_directory() {
        let chosen = resolve_socket_path(
            None,
            Some(OsStr::new("/tmp/from-env.sock")),
            Path::new("/var/run/turn"),
        );
        assert_eq!(chosen, PathBuf::from("/tmp/from-env.sock"));
    }

    #[test]
    fn with_nothing_set_the_socket_is_the_conventional_file_in_the_runtime_directory() {
        let chosen = resolve_socket_path(None, None, Path::new("/var/run/turn"));
        assert_eq!(chosen, PathBuf::from("/var/run/turn/turnd.sock"));
    }

    /// `export TURN_SOCKET=` in a shell profile must not send the window looking for
    /// a socket named after the empty string.
    #[test]
    fn a_blank_environment_variable_counts_as_unset() {
        for blank in ["", "   ", "\t\n"] {
            let chosen = resolve_socket_path(
                None,
                Some(&OsString::from(blank)),
                Path::new("/var/run/turn"),
            );
            assert_eq!(
                chosen,
                PathBuf::from("/var/run/turn/turnd.sock"),
                "blank {blank:?} should have been ignored"
            );
        }
    }

    #[test]
    fn the_resolved_directory_is_absolute_so_the_socket_does_not_move_with_the_cwd() {
        assert!(socket_dir().unwrap().is_absolute());
        assert!(socket_path_from_env().unwrap().is_absolute());
    }

    #[test]
    fn relative_data_and_socket_paths_are_anchored_once() {
        assert_eq!(
            resolve_startup_paths(
                PathBuf::from("state"),
                PathBuf::from("run/turnd.sock"),
                Some(Path::new("/repo")),
            ),
            StartupPaths {
                data_dir: PathBuf::from("/repo/state"),
                socket: PathBuf::from("/repo/run/turnd.sock"),
            }
        );
    }

    #[test]
    fn socket_directory_falls_back_to_the_same_data_directory_as_turnd() {
        let data = Path::new("/private/turn-state");
        let expected = directories::ProjectDirs::from("dev", "turn", "turn")
            .and_then(|dirs| dirs.runtime_dir().map(Path::to_path_buf))
            .unwrap_or_else(|| data.to_path_buf());
        assert_eq!(socket_dir_for(data), expected);
    }
}
