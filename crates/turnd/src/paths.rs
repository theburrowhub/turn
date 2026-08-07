//! Where the socket and the throwaway agent configuration live.
//!
//! Resolution is written as pure functions over their inputs, with one thin edge
//! ([`socket_path_from_env`]) that actually reads the environment. Tests then check
//! the precedence rules without mutating process-global state that every other test
//! in the binary can see.

use crate::error::{DaemonError, Result};
use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use turn_core::ids::{NodeId, SessionId};

/// Overrides the socket location.
pub const SOCKET_ENV: &str = "TURN_SOCKET";

/// The socket file inside the runtime (or data) directory.
pub const SOCKET_FILE: &str = "turnd.sock";

/// Directory under the data dir that holds per-session scratch configuration.
pub const SCRATCH_DIR: &str = "scratch";

/// Daemon-owned default root for isolated Git worktrees.
pub const WORKTREES_DIR: &str = "worktrees";

/// Private per-pane terminal checkpoints and journals.
pub const TERMINAL_HISTORY_DIR: &str = "terminal-history";

/// Longest socket path we will attempt.
///
/// `sun_path` is 104 bytes on macOS and 108 on Linux, and the byte after the path
/// must be a NUL. 100 is under both with room to spare, and being refused with an
/// explanation beats `bind()` returning `EINVAL` for reasons nobody guesses.
pub const MAX_SOCKET_PATH_BYTES: usize = 100;

/// Resolves the socket path from an explicit flag, the environment, and the
/// directory to fall back to.
///
/// Precedence is flag, then `TURN_SOCKET`, then `<dir>/turnd.sock`. A variable set
/// to whitespace counts as unset: an exported but blank `TURN_SOCKET` is a shell
/// accident, and honouring it would try to bind the current directory.
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

/// The socket path this process would use, reading `TURN_SOCKET`.
pub fn socket_path_from_env(explicit: Option<&Path>, dir: &Path) -> PathBuf {
    resolve_socket_path(explicit, std::env::var_os(SOCKET_ENV).as_deref(), dir)
}

/// The directory a socket belongs in: the platform runtime directory when there is
/// one, otherwise the data directory.
///
/// Linux has `$XDG_RUNTIME_DIR`, which is tmpfs and cleared on logout — exactly
/// right for a socket. macOS has no equivalent, so the data directory is used and
/// a stale socket is cleaned up on start instead.
pub fn socket_dir(data_dir: &Path) -> PathBuf {
    directories::ProjectDirs::from("dev", "turn", "turn")
        .and_then(|dirs| dirs.runtime_dir().map(Path::to_path_buf))
        .unwrap_or_else(|| data_dir.to_path_buf())
}

/// Rejects a path the kernel would refuse for a reason that is hard to read.
pub fn check_socket_path(socket: &Path) -> Result<()> {
    let length = socket.as_os_str().as_encoded_bytes().len();
    if length > MAX_SOCKET_PATH_BYTES {
        return Err(DaemonError::SocketPathTooLong {
            socket: socket.to_path_buf(),
            length,
            limit: MAX_SOCKET_PATH_BYTES,
        });
    }
    Ok(())
}

/// The root of Turn's own scratch space.
pub fn scratch_root(data_dir: &Path) -> PathBuf {
    data_dir.join(SCRATCH_DIR)
}

pub fn worktree_root(data_dir: &Path, workspace: &turn_core::ids::WorkspaceId) -> PathBuf {
    data_dir.join(WORKTREES_DIR).join(workspace.as_str())
}

pub fn terminal_history_root(data_dir: &Path) -> PathBuf {
    data_dir.join(TERMINAL_HISTORY_DIR)
}

pub fn session_terminal_history(data_dir: &Path, session: &SessionId) -> PathBuf {
    terminal_history_root(data_dir).join(session.as_str())
}

pub fn node_terminal_history(data_dir: &Path, session: &SessionId, node: &NodeId) -> PathBuf {
    session_terminal_history(data_dir, session).join(node.as_str())
}

/// Where a session's injected agent configuration lives.
///
/// Keyed by session so closing one takes its configuration with it, and inside
/// Turn's own data directory so the user's `~/.claude` is never written to.
pub fn session_scratch(data_dir: &Path, session: &SessionId) -> PathBuf {
    scratch_root(data_dir).join(session.as_str())
}

/// Where one node's injected configuration lives.
pub fn node_scratch(data_dir: &Path, session: &SessionId, node: &NodeId) -> PathBuf {
    session_scratch(data_dir, session).join(node.as_str())
}

/// Creates a directory and everything above it.
pub fn ensure_dir(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).map_err(|cause| DaemonError::directory(dir, cause))
}

/// Deletes a session's scratch directory. Missing is success.
pub fn remove_session_scratch(data_dir: &Path, session: &SessionId) {
    let dir = session_scratch(data_dir, session);
    if let Err(error) = std::fs::remove_dir_all(&dir) {
        if error.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(dir = %dir.display(), %error, "could not remove session scratch");
        }
    }
}

/// Deletes one node's scratch directory. Missing is success.
///
/// Wanted when a node is retired while its session lives on — a relaunch, which mints a
/// new node and a new directory. What is left behind otherwise is a settings file naming
/// a hook URL whose token has just been revoked, and a user who finds one should not have
/// to wonder whether it is live.
pub fn remove_node_scratch(data_dir: &Path, session: &SessionId, node: &NodeId) {
    let dir = node_scratch(data_dir, session, node);
    if let Err(error) = std::fs::remove_dir_all(&dir) {
        if error.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(dir = %dir.display(), %error, "could not remove node scratch");
        }
    }
}

/// Deletes one retired node's terminal history without following a symlink planted
/// inside the data directory.
pub fn remove_node_terminal_history(data_dir: &Path, session: &SessionId, node: &NodeId) {
    let root = terminal_history_root(data_dir);
    if !private_directory_or_remove(&root, "terminal history root") {
        return;
    }
    let session_dir = session_terminal_history(data_dir, session);
    if !private_directory_or_remove(&session_dir, "session terminal history") {
        return;
    }
    remove_private_entry(
        &node_terminal_history(data_dir, session, node),
        "node terminal history",
    );
}

/// Deletes all terminal history for a session that opted out of persistence.
pub fn remove_session_terminal_history(data_dir: &Path, session: &SessionId) {
    let root = terminal_history_root(data_dir);
    if !private_directory_or_remove(&root, "terminal history root") {
        return;
    }
    remove_private_entry(
        &session_terminal_history(data_dir, session),
        "session terminal history",
    );
}

/// Returns true only for a real directory. Unexpected files and symlinks are removed
/// as entries themselves, never followed, so callers can safely inspect below it.
fn private_directory_or_remove(path: &Path, label: &str) -> bool {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => true,
        Ok(_) => {
            remove_private_entry(path, label);
            false
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "could not inspect {label}");
            false
        }
    }
}

fn remove_private_entry(path: &Path, label: &str) {
    let result = match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || metadata.is_file() => {
            std::fs::remove_file(path)
        }
        Ok(_) => std::fs::remove_dir_all(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => Err(error),
    };
    if let Err(error) = result {
        tracing::warn!(path = %path.display(), %error, "could not remove {label}");
    }
}

/// Removes scratch directories belonging to sessions that no longer exist.
///
/// A daemon killed with `SIGKILL` leaves these behind. They are small, but they
/// contain a settings file naming a hook URL that will never answer again, and a
/// user finding one should not have to wonder whether it is live.
pub fn prune_scratch(data_dir: &Path, known: &HashSet<String>) -> usize {
    let root = scratch_root(data_dir);
    let Ok(entries) = std::fs::read_dir(&root) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if known.contains(&name) {
            continue;
        }
        if std::fs::remove_dir_all(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// Removes journals for sessions and nodes no longer present in the durable model.
///
/// Directory entries are treated as hostile filesystem input: symlinks are unlinked,
/// never traversed.
pub fn prune_terminal_history(data_dir: &Path, known: &HashMap<String, HashSet<String>>) -> usize {
    let root = terminal_history_root(data_dir);
    if !private_directory_or_remove(&root, "terminal history root") {
        return 0;
    }
    let Ok(sessions) = std::fs::read_dir(&root) else {
        return 0;
    };
    let mut removed = 0;
    for session in sessions.flatten() {
        let session_name = session.file_name().to_string_lossy().to_string();
        if !private_directory_or_remove(&session.path(), "terminal history session") {
            removed += 1;
            continue;
        }
        let Some(nodes) = known.get(&session_name) else {
            remove_private_entry(&session.path(), "stale terminal history");
            removed += 1;
            continue;
        };
        let Ok(entries) = std::fs::read_dir(session.path()) else {
            continue;
        };
        for node in entries.flatten() {
            let node_name = node.file_name().to_string_lossy().to_string();
            let is_directory = node.file_type().is_ok_and(|kind| kind.is_dir());
            if !is_directory || !nodes.contains(&node_name) {
                remove_private_entry(&node.path(), "stale node terminal history");
                removed += 1;
            }
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn an_explicit_socket_path_beats_the_environment() {
        let chosen = resolve_socket_path(
            Some(Path::new("/tmp/explicit.sock")),
            Some(&OsString::from("/tmp/from-env.sock")),
            Path::new("/data"),
        );
        assert_eq!(chosen, PathBuf::from("/tmp/explicit.sock"));
    }

    #[test]
    fn the_environment_beats_the_default_directory() {
        let chosen = resolve_socket_path(
            None,
            Some(&OsString::from("/tmp/from-env.sock")),
            Path::new("/data"),
        );
        assert_eq!(chosen, PathBuf::from("/tmp/from-env.sock"));
    }

    #[test]
    fn a_blank_environment_variable_is_treated_as_unset() {
        // An exported but empty TURN_SOCKET would otherwise resolve to a relative
        // path in whatever directory the daemon happened to start in.
        for blank in ["", "   ", "\t"] {
            let chosen =
                resolve_socket_path(None, Some(&OsString::from(blank)), Path::new("/data"));
            assert_eq!(
                chosen,
                PathBuf::from("/data/turnd.sock"),
                "blank {blank:?} must not be honoured"
            );
        }
    }

    #[test]
    fn an_over_long_socket_path_is_refused_with_the_escape_hatch_named() {
        let long = PathBuf::from(format!("/tmp/{}/turnd.sock", "x".repeat(120)));
        let error = check_socket_path(&long).expect_err("must be refused");
        let message = error.to_string();
        assert!(
            message.contains("TURN_SOCKET"),
            "unhelpful message: {message}"
        );
        assert!(check_socket_path(Path::new("/tmp/turnd.sock")).is_ok());
    }

    #[test]
    fn scratch_directories_are_keyed_by_session_then_node() {
        let session = SessionId::from_stored("sess_abc");
        let node = NodeId::from_stored("proc_def");
        let dir = node_scratch(Path::new("/data"), &session, &node);
        assert_eq!(dir, PathBuf::from("/data/scratch/sess_abc/proc_def"));
        assert!(dir.starts_with(session_scratch(Path::new("/data"), &session)));
    }

    #[test]
    fn removing_one_nodes_scratch_leaves_the_rest_of_the_session_alone() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path();
        let session = SessionId::from_stored("sess_live");
        let retired = NodeId::from_stored("proc_old");
        let other = NodeId::from_stored("proc_other");
        ensure_dir(&node_scratch(data, &session, &retired)).unwrap();
        ensure_dir(&node_scratch(data, &session, &other)).unwrap();

        remove_node_scratch(data, &session, &retired);
        assert!(!node_scratch(data, &session, &retired).exists());
        assert!(
            node_scratch(data, &session, &other).exists(),
            "the session's other processes are still running with theirs"
        );
        // Removing it twice is not a failure: a node that never wrote one is normal.
        remove_node_scratch(data, &session, &retired);
    }

    #[test]
    fn pruning_scratch_keeps_the_sessions_that_still_exist() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path();
        let live = SessionId::from_stored("sess_live");
        let gone = SessionId::from_stored("sess_gone");
        ensure_dir(&session_scratch(data, &live)).unwrap();
        ensure_dir(&session_scratch(data, &gone)).unwrap();

        let known: HashSet<String> = [live.to_string()].into_iter().collect();
        assert_eq!(prune_scratch(data, &known), 1);
        assert!(session_scratch(data, &live).exists());
        assert!(!session_scratch(data, &gone).exists());
    }

    #[test]
    fn pruning_terminal_history_keeps_only_durable_sessions_and_nodes() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path();
        let live = SessionId::from_stored("sess_live");
        let keep = NodeId::from_stored("proc_keep");
        let retired = NodeId::from_stored("proc_retired");
        let gone = SessionId::from_stored("sess_gone");
        ensure_dir(&node_terminal_history(data, &live, &keep)).unwrap();
        ensure_dir(&node_terminal_history(data, &live, &retired)).unwrap();
        ensure_dir(&node_terminal_history(data, &gone, &NodeId::new())).unwrap();

        let known = HashMap::from([(live.to_string(), HashSet::from([keep.to_string()]))]);
        assert_eq!(prune_terminal_history(data, &known), 2);
        assert!(node_terminal_history(data, &live, &keep).exists());
        assert!(!node_terminal_history(data, &live, &retired).exists());
        assert!(!session_terminal_history(data, &gone).exists());
    }

    #[cfg(unix)]
    #[test]
    fn deleting_terminal_history_unlinks_a_symlink_without_following_it() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let data = temp.path();
        let session = SessionId::from_stored("sess_live");
        let node = NodeId::from_stored("proc_link");
        let outside = temp.path().join("outside");
        ensure_dir(&outside).unwrap();
        std::fs::write(outside.join("keep"), b"safe").unwrap();
        ensure_dir(&session_terminal_history(data, &session)).unwrap();
        symlink(&outside, node_terminal_history(data, &session, &node)).unwrap();

        remove_node_terminal_history(data, &session, &node);
        assert!(outside.join("keep").exists());
        assert!(!node_terminal_history(data, &session, &node).exists());
    }

    #[cfg(unix)]
    #[test]
    fn pruning_unlinks_a_known_session_symlink_without_touching_its_target() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let data = temp.path();
        let session = SessionId::from_stored("sess_live");
        let node = NodeId::from_stored("proc_keep");
        let outside = temp.path().join("outside");
        ensure_dir(&terminal_history_root(data)).unwrap();
        ensure_dir(&outside).unwrap();
        std::fs::write(outside.join(node.as_str()), b"safe").unwrap();
        symlink(&outside, session_terminal_history(data, &session)).unwrap();

        let known = HashMap::from([(session.to_string(), HashSet::from([node.to_string()]))]);
        assert_eq!(prune_terminal_history(data, &known), 1);
        assert_eq!(std::fs::read(outside.join(node.as_str())).unwrap(), b"safe");
        assert!(!session_terminal_history(data, &session).exists());
    }
}
