//! Where the database lives.
//!
//! Resolution is a pure function of two inputs — an explicit override and the
//! `TURN_DATA_DIR` environment variable — so the rules can be tested without a
//! test mutating process-global state that every other test in the binary also
//! reads. [`default_data_dir`] is the thin edge that actually looks at the
//! environment.

use crate::error::{Result, StoreError};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// The database file inside the data directory.
pub const DATABASE_FILE: &str = "turn.db";

/// The variable that overrides the platform data directory.
pub const DATA_DIR_ENV: &str = "TURN_DATA_DIR";

/// Resolves the data directory from explicit and environment inputs.
///
/// Precedence is override, then environment, then platform default. A variable
/// set to an empty or whitespace-only value counts as unset: an exported but
/// blank `TURN_DATA_DIR` is a shell accident, and honouring it would put the
/// user's sessions in the current working directory.
pub fn resolve_data_dir(explicit: Option<&Path>, env_value: Option<&OsStr>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(path.to_path_buf());
    }
    if let Some(value) = env_value {
        if !value.to_string_lossy().trim().is_empty() {
            return Ok(PathBuf::from(value));
        }
    }
    platform_data_dir()
}

/// The platform data directory: `~/Library/Application Support/dev.turn.turn`
/// on macOS, `$XDG_DATA_HOME/turn` on Linux.
pub fn platform_data_dir() -> Result<PathBuf> {
    directories::ProjectDirs::from("dev", "turn", "turn")
        .map(|dirs| dirs.data_dir().to_path_buf())
        .ok_or(StoreError::NoDataDir)
}

/// The data directory Turn would use right now, honouring [`DATA_DIR_ENV`].
pub fn default_data_dir() -> Result<PathBuf> {
    resolve_data_dir(None, std::env::var_os(DATA_DIR_ENV).as_deref())
}

/// The database file inside a data directory.
pub fn database_path(dir: &Path) -> PathBuf {
    dir.join(DATABASE_FILE)
}

/// Creates a directory and everything above it, if needed.
///
/// On unix the leaf is then narrowed to `0700`. The store holds every command line
/// an agent proposed, the working directories it ran in, and hook payloads — enough
/// to reconstruct someone's whole working day. The platform default of `0755` makes
/// all of that readable by every other account on the machine, which is a poor
/// default for a single-user desktop app.
///
/// Only the leaf is narrowed: tightening `~/Library/Application Support` or
/// `~/.local/share` would be Turn overreaching into directories it does not own.
pub fn ensure_dir(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).map_err(|cause| StoreError::data_dir(dir, cause))?;
    restrict_to_owner(dir, 0o700)
}

/// Narrows a path to owner-only access.
///
/// A no-op on non-unix targets, where the permission model is different and the
/// MVP does not ship anyway.
pub fn restrict_to_owner(path: &Path, mode: u32) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Read the metadata first so an already-correct path costs no write.
        let current = std::fs::metadata(path)
            .map_err(|cause| StoreError::data_dir(path, cause))?
            .permissions()
            .mode()
            & 0o777;
        if current != mode {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
                .map_err(|cause| StoreError::data_dir(path, cause))?;
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn an_explicit_directory_beats_the_environment() {
        let env = OsString::from("/tmp/from-env");
        let resolved =
            resolve_data_dir(Some(Path::new("/tmp/explicit")), Some(env.as_os_str())).unwrap();
        assert_eq!(resolved, PathBuf::from("/tmp/explicit"));
    }

    #[test]
    fn the_environment_variable_is_honoured_when_there_is_no_override() {
        let env = OsString::from("/tmp/from-env");
        let resolved = resolve_data_dir(None, Some(env.as_os_str())).unwrap();
        assert_eq!(resolved, PathBuf::from("/tmp/from-env"));
    }

    #[test]
    fn a_blank_environment_variable_is_treated_as_unset() {
        for blank in ["", "   ", "\t"] {
            let env = OsString::from(blank);
            let resolved = resolve_data_dir(None, Some(env.as_os_str())).unwrap();
            assert_ne!(
                resolved,
                PathBuf::from(blank),
                "an exported-but-empty variable must not become the data dir"
            );
            assert!(resolved.is_absolute(), "got {}", resolved.display());
        }
    }

    /// The same call must produce a usable absolute path on both platforms Turn
    /// ships on; only the value differs.
    #[test]
    fn the_platform_default_is_an_absolute_path_under_the_users_home() {
        let dir = platform_data_dir().expect("a developer machine has a home directory");
        assert!(dir.is_absolute(), "got {}", dir.display());
        let rendered = dir.display().to_string();
        assert!(
            rendered.contains("turn"),
            "the app should own its directory: {rendered}"
        );
        if cfg!(target_os = "macos") {
            assert!(rendered.contains("Application Support"), "got {rendered}");
        }
    }

    #[test]
    fn the_database_file_sits_directly_inside_the_data_directory() {
        let path = database_path(Path::new("/tmp/turn-data"));
        assert_eq!(path, PathBuf::from("/tmp/turn-data/turn.db"));
    }

    #[test]
    fn ensuring_a_directory_creates_missing_parents_and_is_idempotent() {
        let temp = tempfile::tempdir().unwrap();
        let nested = temp.path().join("a/b/c");
        ensure_dir(&nested).unwrap();
        assert!(nested.is_dir());
        ensure_dir(&nested).unwrap();
        assert!(nested.is_dir());
    }
}
