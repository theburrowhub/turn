//! Private filesystem inventory, log hygiene and offline installation purge.

use crate::error::{DaemonError, Result};
use crate::paths::{SCRATCH_DIR, TERMINAL_HISTORY_DIR, WORKTREES_DIR};
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use turn_core::ids::SessionId;
use turn_core::privacy::{InstallationPurgeReport, PrivacyDatum, PrivacyScope};

pub const DAEMON_LOG_FILE: &str = "turnd.log";

/// Bytes occupied by Turn-owned persistent files, excluding user checkout roots.
pub fn persistent_bytes(data_dir: &Path) -> Result<u64> {
    let mut bytes = 0u64;
    for path in database_paths(data_dir)
        .into_iter()
        .chain([data_dir.join(DAEMON_LOG_FILE)])
    {
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if !metadata.file_type().is_symlink() && metadata.is_file() => {
                bytes = bytes.saturating_add(metadata.len());
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(DaemonError::directory(&path, error)),
        }
    }
    Ok(bytes)
}

/// Files attached to one logical scope. Payload-bearing terminal/scratch/log files
/// are metadata only; exporting their bytes would defeat the privacy boundary.
pub fn file_rows(
    data_dir: &Path,
    scope: &PrivacyScope,
    workspace_sessions: &[SessionId],
) -> Result<Vec<PrivacyDatum>> {
    let mut rows = Vec::new();
    match scope {
        PrivacyScope::Installation => {
            for path in database_paths(data_dir) {
                collect_one(data_dir, &path, database_kind(&path), &mut rows)?;
            }
            for name in [
                DAEMON_LOG_FILE,
                ".turnd.lock",
                "turnd.sock",
                "turnd.sock.token",
            ] {
                collect_one(data_dir, &data_dir.join(name), file_kind(name), &mut rows)?;
            }
            collect_tree(
                data_dir,
                &data_dir.join(SCRATCH_DIR),
                "scratch_configuration",
                &mut rows,
            )?;
            collect_tree(
                data_dir,
                &data_dir.join(TERMINAL_HISTORY_DIR),
                "terminal_history",
                &mut rows,
            )?;
            collect_checkout_roots(data_dir, &mut rows)?;
            collect_unknown_entries(data_dir, &mut rows)?;
        }
        PrivacyScope::Workspace { .. } => {
            for session in workspace_sessions {
                collect_session(data_dir, session, &mut rows)?;
            }
            if let PrivacyScope::Workspace { workspace_id } = scope {
                collect_checkout_root(
                    data_dir,
                    &data_dir.join(WORKTREES_DIR).join(workspace_id.as_str()),
                    &mut rows,
                )?;
            }
        }
        PrivacyScope::Session { session_id } => collect_session(data_dir, session_id, &mut rows)?,
        PrivacyScope::Agent {
            session_id,
            node_id,
        } => {
            collect_tree(
                data_dir,
                &data_dir
                    .join(SCRATCH_DIR)
                    .join(session_id.as_str())
                    .join(node_id.as_str()),
                "scratch_configuration",
                &mut rows,
            )?;
            collect_tree(
                data_dir,
                &data_dir
                    .join(TERMINAL_HISTORY_DIR)
                    .join(session_id.as_str())
                    .join(node_id.as_str()),
                "terminal_history",
                &mut rows,
            )?;
        }
    }
    rows.sort_by(|left, right| left.origin.cmp(&right.origin));
    Ok(rows)
}

/// Keeps the owner-only daemon log bounded and removes known credential shapes
/// already present in it. The same inode is truncated/re-written so the companion's
/// open append descriptor continues to point at the governed file.
pub fn enforce_log_privacy(data_dir: &Path, max_bytes: u64) -> Result<()> {
    let path = data_dir.join(DAEMON_LOG_FILE);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(DaemonError::directory(&path, error)),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(DaemonError::directory(
            &path,
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "the daemon log is not a regular file",
            ),
        ));
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let mut file = options
        .open(&path)
        .map_err(|error| DaemonError::directory(&path, error))?;
    let length = file
        .metadata()
        .map_err(|error| DaemonError::directory(&path, error))?
        .len();
    let keep = length.min(max_bytes);
    file.seek(SeekFrom::Start(length.saturating_sub(keep)))
        .map_err(|error| DaemonError::directory(&path, error))?;
    let mut bytes = Vec::with_capacity(usize::try_from(keep).unwrap_or(0));
    let mut limited = file.take(keep);
    limited
        .read_to_end(&mut bytes)
        .map_err(|error| DaemonError::directory(&path, error))?;
    let mut file = limited.into_inner();
    let safe = turn_store::redact::redact_secrets(&String::from_utf8_lossy(&bytes));
    let safe = if safe.len() as u64 > max_bytes {
        let mut start = safe.len().saturating_sub(max_bytes as usize);
        while !safe.is_char_boundary(start) {
            start += 1;
        }
        &safe[start..]
    } else {
        &safe
    };
    file.set_len(0)
        .map_err(|error| DaemonError::directory(&path, error))?;
    file.seek(SeekFrom::Start(0))
        .and_then(|_| file.write_all(safe.as_bytes()))
        .and_then(|_| file.sync_data())
        .map_err(|error| DaemonError::directory(&path, error))?;
    Ok(())
}

/// Physically removes Turn's database/sidecars, logs, scratch and terminal
/// history. The caller must hold [`crate::instance::DataDirLock`]. Checkout roots
/// and the stable lock inode are retained deliberately.
pub fn purge_installation_data(data_dir: &Path, socket: &Path) -> Result<InstallationPurgeReport> {
    let mut report = InstallationPurgeReport::default();
    for path in database_paths(data_dir) {
        remove_owned(&path, &mut report)?;
    }
    for path in [
        data_dir.join(DAEMON_LOG_FILE),
        data_dir.join(SCRATCH_DIR),
        data_dir.join(TERMINAL_HISTORY_DIR),
        turn_proto::ipc_auth_token_path(socket),
    ] {
        remove_owned(&path, &mut report)?;
    }
    remove_unclassified_entries(data_dir, &mut report)?;
    remove_stale_socket(socket, &mut report)?;
    report.retained_checkout_roots = std::fs::read_dir(data_dir.join(WORKTREES_DIR))
        .map(|entries| entries.flatten().count() as u64)
        .unwrap_or(0);
    Ok(report)
}

fn collect_session(
    data_dir: &Path,
    session: &SessionId,
    rows: &mut Vec<PrivacyDatum>,
) -> Result<()> {
    collect_tree(
        data_dir,
        &data_dir.join(SCRATCH_DIR).join(session.as_str()),
        "scratch_configuration",
        rows,
    )?;
    collect_tree(
        data_dir,
        &data_dir.join(TERMINAL_HISTORY_DIR).join(session.as_str()),
        "terminal_history",
        rows,
    )
}

fn collect_tree(
    data_dir: &Path,
    path: &Path,
    kind: &str,
    rows: &mut Vec<PrivacyDatum>,
) -> Result<()> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(DaemonError::directory(path, error)),
    };
    if metadata.file_type().is_symlink() || metadata.is_file() {
        return push_metadata(data_dir, path, kind, metadata, rows);
    }
    if !metadata.is_dir() {
        return push_metadata(data_dir, path, kind, metadata, rows);
    }
    let entries = std::fs::read_dir(path).map_err(|error| DaemonError::directory(path, error))?;
    for entry in entries {
        let entry = entry.map_err(|error| DaemonError::directory(path, error))?;
        collect_tree(data_dir, &entry.path(), kind, rows)?;
    }
    Ok(())
}

fn collect_one(
    data_dir: &Path,
    path: &Path,
    kind: &str,
    rows: &mut Vec<PrivacyDatum>,
) -> Result<()> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(DaemonError::directory(path, error)),
    };
    push_metadata(data_dir, path, kind, metadata, rows)
}

fn push_metadata(
    data_dir: &Path,
    path: &Path,
    kind: &str,
    metadata: std::fs::Metadata,
    rows: &mut Vec<PrivacyDatum>,
) -> Result<()> {
    let relative = path.strip_prefix(data_dir).unwrap_or(path);
    let origin = turn_store::redact::redact_secrets(&relative.to_string_lossy());
    let timestamp_ms = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as i64);
    let bytes = if metadata.is_dir() { 0 } else { metadata.len() };
    rows.push(PrivacyDatum {
        origin: format!("file/{origin}"),
        data_type: kind.to_string(),
        timestamp_ms,
        bytes,
        content: serde_json::json!({
            "payload_exported": false,
            "symlink": metadata.file_type().is_symlink(),
            "reason": "filesystem payloads may contain terminal output, logs or injected configuration",
        }),
    });
    Ok(())
}

fn collect_checkout_roots(data_dir: &Path, rows: &mut Vec<PrivacyDatum>) -> Result<()> {
    let root = data_dir.join(WORKTREES_DIR);
    let entries = match std::fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(DaemonError::directory(&root, error)),
    };
    for entry in entries {
        let entry = entry.map_err(|error| DaemonError::directory(&root, error))?;
        collect_checkout_root(data_dir, &entry.path(), rows)?;
    }
    Ok(())
}

/// Accounts for additions made by future builds even before this build knows
/// their semantic type. The data directory is Turn-owned; checkout roots and the
/// stable lock inode are the only deliberate exclusions.
fn collect_unknown_entries(data_dir: &Path, rows: &mut Vec<PrivacyDatum>) -> Result<()> {
    let entries = match std::fs::read_dir(data_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(DaemonError::directory(data_dir, error)),
    };
    let known: Vec<PathBuf> = database_paths(data_dir)
        .into_iter()
        .chain([
            data_dir.join(DAEMON_LOG_FILE),
            data_dir.join(".turnd.lock"),
            data_dir.join("turnd.sock"),
            data_dir.join("turnd.sock.token"),
            data_dir.join(SCRATCH_DIR),
            data_dir.join(TERMINAL_HISTORY_DIR),
            data_dir.join(WORKTREES_DIR),
        ])
        .collect();
    for entry in entries {
        let entry = entry.map_err(|error| DaemonError::directory(data_dir, error))?;
        if !known.iter().any(|path| path == &entry.path()) {
            collect_tree(data_dir, &entry.path(), "unclassified_local_file", rows)?;
        }
    }
    Ok(())
}

fn collect_checkout_root(data_dir: &Path, path: &Path, rows: &mut Vec<PrivacyDatum>) -> Result<()> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(DaemonError::directory(path, error)),
    };
    let bytes = tree_size(path)?;
    let relative = path.strip_prefix(data_dir).unwrap_or(path);
    rows.push(PrivacyDatum {
        origin: format!(
            "checkout/{}",
            turn_store::redact::redact_secrets(&relative.to_string_lossy())
        ),
        data_type: "user_checkout_root".to_string(),
        timestamp_ms: metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_millis() as i64),
        bytes,
        content: serde_json::json!({
            "payload_exported": false,
            "deleted_by_privacy_purge": false,
            "reason": "checkout contents are user work, not Turn local records",
        }),
    });
    Ok(())
}

fn tree_size(path: &Path) -> Result<u64> {
    let metadata =
        std::fs::symlink_metadata(path).map_err(|error| DaemonError::directory(path, error))?;
    if metadata.file_type().is_symlink() || metadata.is_file() {
        return Ok(metadata.len());
    }
    if !metadata.is_dir() {
        return Ok(0);
    }
    let mut bytes = 0u64;
    for entry in std::fs::read_dir(path).map_err(|error| DaemonError::directory(path, error))? {
        let entry = entry.map_err(|error| DaemonError::directory(path, error))?;
        bytes = bytes.saturating_add(tree_size(&entry.path())?);
    }
    Ok(bytes)
}

fn database_paths(data_dir: &Path) -> Vec<PathBuf> {
    let database = turn_store::location::database_path(data_dir);
    let mut paths = vec![database.clone()];
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut path = database.as_os_str().to_os_string();
        path.push(suffix);
        paths.push(PathBuf::from(path));
    }
    paths
}

fn database_kind(path: &Path) -> &'static str {
    match path.file_name().and_then(|name| name.to_str()) {
        Some(name)
            if name.ends_with("-wal") || name.ends_with("-shm") || name.ends_with("-journal") =>
        {
            "sqlite_sidecar"
        }
        _ => "sqlite_database",
    }
}

fn file_kind(name: &str) -> &'static str {
    match name {
        DAEMON_LOG_FILE => "diagnostic_log",
        ".turnd.lock" | "turnd.sock" | "turnd.sock.token" => "runtime_control",
        _ => "local_file",
    }
}

fn remove_owned(path: &Path, report: &mut InstallationPurgeReport) -> Result<()> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(DaemonError::directory(path, error)),
    };
    if !metadata.is_dir() {
        report.bytes_freed = report.bytes_freed.saturating_add(metadata.len());
        std::fs::remove_file(path).map_err(|error| DaemonError::directory(path, error))?;
        report.files_deleted += 1;
        return Ok(());
    }
    let (files, directories, bytes) = tree_stats(path)?;
    // The standard recursive remover uses descriptor-relative traversal on Unix
    // and does not follow directory symlinks. Keeping deletion in that primitive
    // avoids a metadata/read_dir/remove sequence for every child.
    std::fs::remove_dir_all(path).map_err(|error| DaemonError::directory(path, error))?;
    report.files_deleted = report.files_deleted.saturating_add(files);
    report.directories_deleted = report.directories_deleted.saturating_add(directories);
    report.bytes_freed = report.bytes_freed.saturating_add(bytes);
    Ok(())
}

fn tree_stats(path: &Path) -> Result<(u64, u64, u64)> {
    let metadata =
        std::fs::symlink_metadata(path).map_err(|error| DaemonError::directory(path, error))?;
    if !metadata.is_dir() {
        return Ok((1, 0, metadata.len()));
    }
    let mut files = 0u64;
    let mut directories = 1u64;
    let mut bytes = 0u64;
    for entry in std::fs::read_dir(path).map_err(|error| DaemonError::directory(path, error))? {
        let entry = entry.map_err(|error| DaemonError::directory(path, error))?;
        let (child_files, child_directories, child_bytes) = tree_stats(&entry.path())?;
        files = files.saturating_add(child_files);
        directories = directories.saturating_add(child_directories);
        bytes = bytes.saturating_add(child_bytes);
    }
    Ok((files, directories, bytes))
}

fn remove_stale_socket(path: &Path, report: &mut InstallationPurgeReport) -> Result<()> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(DaemonError::directory(path, error)),
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        if !metadata.file_type().is_socket() && !metadata.file_type().is_symlink() {
            return Err(DaemonError::directory(
                path,
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "refusing to remove a non-socket runtime path",
                ),
            ));
        }
    }
    std::fs::remove_file(path).map_err(|error| DaemonError::directory(path, error))?;
    report.files_deleted += 1;
    Ok(())
}

fn remove_unclassified_entries(
    data_dir: &Path,
    report: &mut InstallationPurgeReport,
) -> Result<()> {
    let entries = match std::fs::read_dir(data_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(DaemonError::directory(data_dir, error)),
    };
    for entry in entries {
        let entry = entry.map_err(|error| DaemonError::directory(data_dir, error))?;
        let name = entry.file_name();
        if name == ".turnd.lock" || name == WORKTREES_DIR {
            continue;
        }
        remove_owned(&entry.path(), report)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";

    #[test]
    fn log_hygiene_bounds_the_file_and_scrubs_known_credentials() {
        let temp = tempfile::tempdir().unwrap();
        let log = temp.path().join(DAEMON_LOG_FILE);
        std::fs::write(&log, format!("before {TOKEN} after\n{}", "x".repeat(500))).unwrap();
        enforce_log_privacy(temp.path(), 128).unwrap();
        let content = std::fs::read_to_string(log).unwrap();
        assert!(content.len() <= 128);
        assert!(!content.contains(TOKEN));
    }

    #[cfg(unix)]
    #[test]
    fn inventory_and_purge_never_follow_symlinks_and_keep_checkout_work() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("data");
        std::fs::create_dir_all(data.join(SCRATCH_DIR).join("sess_a")).unwrap();
        std::fs::create_dir_all(data.join(TERMINAL_HISTORY_DIR).join("sess_a/proc_a")).unwrap();
        std::fs::create_dir_all(data.join(WORKTREES_DIR).join("ws_a")).unwrap();
        std::fs::write(data.join(WORKTREES_DIR).join("ws_a/keep.txt"), b"work").unwrap();
        std::fs::write(data.join("turn.db"), b"db").unwrap();
        std::fs::write(data.join("future-journal.bin"), b"future").unwrap();
        std::fs::write(
            data.join(TERMINAL_HISTORY_DIR)
                .join("sess_a/proc_a/journal.bin"),
            b"history",
        )
        .unwrap();
        let outside = temp.path().join("outside");
        std::fs::write(&outside, b"outside").unwrap();
        symlink(&outside, data.join(SCRATCH_DIR).join("sess_a/link")).unwrap();

        let rows = file_rows(data.as_path(), &PrivacyScope::Installation, &[]).unwrap();
        assert!(rows.iter().any(|row| row.data_type == "terminal_history"));
        assert!(rows.iter().any(|row| row.data_type == "user_checkout_root"));
        assert!(rows
            .iter()
            .any(|row| row.data_type == "unclassified_local_file"));
        let socket = data.join("turnd.sock");
        let report = purge_installation_data(&data, &socket).unwrap();
        assert!(report.files_deleted >= 3);
        assert_eq!(std::fs::read(&outside).unwrap(), b"outside");
        assert_eq!(
            std::fs::read(data.join(WORKTREES_DIR).join("ws_a/keep.txt")).unwrap(),
            b"work"
        );
        assert!(!data.join("turn.db").exists());
        assert!(!data.join(SCRATCH_DIR).exists());
        assert!(!data.join(TERMINAL_HISTORY_DIR).exists());
        assert!(!data.join("future-journal.bin").exists());
    }
}
