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

/// The socket file inside the runtime (or data) directory.
pub const SOCKET_FILE: &str = "turnd.sock";

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
pub fn socket_dir() -> PathBuf {
    let dirs = directories::ProjectDirs::from("dev", "turn", "turn");
    match &dirs {
        Some(dirs) => dirs
            .runtime_dir()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| dirs.data_dir().to_path_buf()),
        // No home directory at all — a stripped CI container. `/tmp` keeps the
        // failure legible ("no daemon at /tmp/turnd.sock") instead of panicking.
        None => PathBuf::from("/tmp"),
    }
}

/// The socket this process will connect to, reading `TURN_SOCKET`.
pub fn socket_path_from_env() -> PathBuf {
    resolve_socket_path(None, std::env::var_os(SOCKET_ENV).as_deref(), &socket_dir())
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
        assert!(socket_dir().is_absolute(), "got {:?}", socket_dir());
        assert!(socket_path_from_env().is_absolute());
    }
}
