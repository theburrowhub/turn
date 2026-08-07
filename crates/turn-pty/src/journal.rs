//! Durable, bounded terminal journals.
//!
//! A checkpoint is an atomic snapshot of the current visible terminal and its input
//! modes. The append-only journal after it contains every PTY output chunk and resize,
//! each with a sequence number, length and CRC. Recovery starts at the checkpoint and
//! replays complete records only; a torn final write is truncated to the last valid
//! boundary.

use crate::{ScreenSize, TerminalBuffer};
use crc32fast::Hasher;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

const CHECKPOINT_MAGIC: &[u8; 8] = b"TURNPTC1";
const JOURNAL_MAGIC: &[u8; 8] = b"TURNPTJ1";
const FORMAT_VERSION: u16 = 1;
const JOURNAL_HEADER_BYTES: u64 = 10;
const RECORD_HEADER_BYTES: u64 = 17;
const RECORD_OUTPUT: u8 = 1;
const RECORD_RESIZE: u8 = 2;
const MAX_RECORD_BYTES: usize = 256 * 1024;

pub const CHECKPOINT_FILE: &str = "checkpoint.bin";
pub const JOURNAL_FILE: &str = "journal.bin";

/// On-disk bounds for one pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JournalConfig {
    pub max_journal_bytes: u64,
    pub max_checkpoint_bytes: usize,
}

impl Default for JournalConfig {
    fn default() -> Self {
        Self {
            // Enough for useful history while keeping thirty chatty panes bounded.
            max_journal_bytes: 8 * 1024 * 1024,
            // A maximally styled 65,536-cell screen stays below this in practice.
            max_checkpoint_bytes: 4 * 1024 * 1024,
        }
    }
}

/// A terminal reconstructed from the last checkpoint and every valid later record.
pub struct RecoveredTerminal {
    pub buffer: TerminalBuffer,
    pub last_seq: u64,
}

/// The writer owned by a live PTY.
pub struct TerminalJournal {
    dir: PathBuf,
    file: File,
    seq: u64,
    bytes: u64,
    config: JournalConfig,
}

impl TerminalJournal {
    /// Starts a new journal for a newly-created process node.
    pub fn create(
        dir: impl Into<PathBuf>,
        initial: &TerminalBuffer,
        config: JournalConfig,
    ) -> io::Result<Self> {
        let dir = dir.into();
        secure_directory_tree(&dir)?;
        write_checkpoint(&dir, initial, 0, false, config)?;
        let file = reset_journal(&dir)?;
        Ok(Self {
            dir,
            file,
            seq: 0,
            bytes: JOURNAL_HEADER_BYTES,
            config,
        })
    }

    /// Appends one authoritative PTY read, or rotates before the pane exceeds its cap.
    ///
    /// The caller has already applied `data` to `buffer`. On rotation the checkpoint is
    /// therefore written at the next sequence and the same bytes are not appended again.
    pub fn record_output(&mut self, data: &[u8], buffer: &mut TerminalBuffer) -> io::Result<()> {
        if data.len() > MAX_RECORD_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "PTY read is larger than one journal record",
            ));
        }
        let next = self.seq.saturating_add(1);
        let record_bytes = RECORD_HEADER_BYTES + data.len() as u64;
        if self.bytes.saturating_add(record_bytes) > self.config.max_journal_bytes {
            write_checkpoint(&self.dir, buffer, next, true, self.config)?;
            self.file = reset_journal(&self.dir)?;
            self.bytes = JOURNAL_HEADER_BYTES;
            self.seq = next;
            buffer.mark_truncated();
            return Ok(());
        }
        self.append(RECORD_OUTPUT, next, data)
    }

    /// Records a geometry change after it has been applied to the in-memory parser.
    pub fn record_resize(
        &mut self,
        size: ScreenSize,
        buffer: &mut TerminalBuffer,
    ) -> io::Result<()> {
        let payload = [
            size.rows.to_le_bytes()[0],
            size.rows.to_le_bytes()[1],
            size.cols.to_le_bytes()[0],
            size.cols.to_le_bytes()[1],
        ];
        let next = self.seq.saturating_add(1);
        let record_bytes = RECORD_HEADER_BYTES + payload.len() as u64;
        if self.bytes.saturating_add(record_bytes) > self.config.max_journal_bytes {
            write_checkpoint(&self.dir, buffer, next, true, self.config)?;
            self.file = reset_journal(&self.dir)?;
            self.bytes = JOURNAL_HEADER_BYTES;
            self.seq = next;
            buffer.mark_truncated();
            return Ok(());
        }
        self.append(RECORD_RESIZE, next, &payload)
    }

    fn append(&mut self, kind: u8, seq: u64, payload: &[u8]) -> io::Result<()> {
        let len = u32::try_from(payload.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "journal record too large"))?;
        let checksum = record_checksum(kind, seq, len, payload);
        self.file.write_all(&[kind])?;
        self.file.write_all(&seq.to_le_bytes())?;
        self.file.write_all(&len.to_le_bytes())?;
        self.file.write_all(&checksum.to_le_bytes())?;
        self.file.write_all(payload)?;
        self.file.flush()?;
        self.seq = seq;
        self.bytes = self
            .bytes
            .saturating_add(RECORD_HEADER_BYTES + payload.len() as u64);
        Ok(())
    }

    /// Reconstructs one pane and repairs a partial/corrupt journal tail in place.
    pub fn recover(
        dir: impl AsRef<Path>,
        config: JournalConfig,
    ) -> io::Result<Option<RecoveredTerminal>> {
        let dir = dir.as_ref();
        let checkpoint_path = dir.join(CHECKPOINT_FILE);
        if !checkpoint_path.exists() {
            return Ok(None);
        }
        secure_directory_tree(dir)?;
        let checkpoint = read_checkpoint(&checkpoint_path, config)?;
        let mut buffer = TerminalBuffer::from_replay(
            checkpoint.size,
            &checkpoint.replay,
            checkpoint.bytes_seen,
            checkpoint.truncated,
        );
        let journal_path = dir.join(JOURNAL_FILE);
        let mut file = secure_open_read_write(&journal_path)?;
        let mut header = [0u8; JOURNAL_HEADER_BYTES as usize];
        let valid_header = file.read_exact(&mut header).is_ok()
            && &header[..8] == JOURNAL_MAGIC
            && u16::from_le_bytes([header[8], header[9]]) == FORMAT_VERSION;
        if !valid_header {
            buffer.mark_truncated();
            let _ = reset_journal(dir)?;
            return Ok(Some(RecoveredTerminal {
                buffer,
                last_seq: checkpoint.seq,
            }));
        }

        let mut valid_end = JOURNAL_HEADER_BYTES;
        let mut last_seq = checkpoint.seq;
        let damaged = loop {
            let record_start = valid_end;
            let mut record_header = [0u8; RECORD_HEADER_BYTES as usize];
            match file.read_exact(&mut record_header) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
                    let len = file.metadata()?.len();
                    break len != record_start;
                }
                Err(error) => return Err(error),
            }
            let kind = record_header[0];
            let seq = u64::from_le_bytes(record_header[1..9].try_into().expect("fixed slice"));
            let len = u32::from_le_bytes(record_header[9..13].try_into().expect("fixed slice"));
            let checksum =
                u32::from_le_bytes(record_header[13..17].try_into().expect("fixed slice"));
            if len as usize > MAX_RECORD_BYTES {
                break true;
            }
            let mut payload = vec![0u8; len as usize];
            if file.read_exact(&mut payload).is_err()
                || checksum != record_checksum(kind, seq, len, &payload)
            {
                break true;
            }
            valid_end = record_start + RECORD_HEADER_BYTES + len as u64;
            if seq <= checkpoint.seq {
                // A crash after checkpoint rename but before journal reset leaves the
                // old prefix behind. The sequence makes replay idempotent.
                continue;
            }
            if seq != last_seq.saturating_add(1) {
                valid_end = record_start;
                break true;
            }
            match kind {
                RECORD_OUTPUT => buffer.write(&payload),
                RECORD_RESIZE if payload.len() == 4 => {
                    buffer.resize(ScreenSize::new(
                        u16::from_le_bytes([payload[0], payload[1]]),
                        u16::from_le_bytes([payload[2], payload[3]]),
                    ));
                }
                _ => {
                    valid_end = record_start;
                    break true;
                }
            }
            last_seq = seq;
        };
        if damaged {
            buffer.mark_truncated();
        }
        if file.metadata()?.len() != valid_end {
            file.set_len(valid_end)?;
            file.seek(SeekFrom::Start(valid_end))?;
            file.flush()?;
        }
        Ok(Some(RecoveredTerminal { buffer, last_seq }))
    }
}

struct Checkpoint {
    size: ScreenSize,
    seq: u64,
    bytes_seen: u64,
    truncated: bool,
    replay: Vec<u8>,
}

fn write_checkpoint(
    dir: &Path,
    buffer: &TerminalBuffer,
    seq: u64,
    force_truncated: bool,
    config: JournalConfig,
) -> io::Result<()> {
    let replay = buffer.state_replay();
    if replay.len() > config.max_checkpoint_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "terminal checkpoint is {} bytes, over the {} byte limit",
                replay.len(),
                config.max_checkpoint_bytes
            ),
        ));
    }
    let len = replay.len() as u32;
    let checksum = checkpoint_checksum(
        buffer.size(),
        seq,
        buffer.bytes_seen(),
        buffer.is_truncated() || force_truncated,
        len,
        &replay,
    );
    let tmp = dir.join("checkpoint.tmp");
    let final_path = dir.join(CHECKPOINT_FILE);
    let mut file = secure_create(&tmp)?;
    file.write_all(CHECKPOINT_MAGIC)?;
    file.write_all(&FORMAT_VERSION.to_le_bytes())?;
    file.write_all(&buffer.size().rows.to_le_bytes())?;
    file.write_all(&buffer.size().cols.to_le_bytes())?;
    file.write_all(&seq.to_le_bytes())?;
    file.write_all(&buffer.bytes_seen().to_le_bytes())?;
    file.write_all(&[u8::from(buffer.is_truncated() || force_truncated)])?;
    file.write_all(&len.to_le_bytes())?;
    file.write_all(&checksum.to_le_bytes())?;
    file.write_all(&replay)?;
    file.sync_all()?;
    std::fs::rename(&tmp, &final_path)?;
    secure_file_permissions(&final_path)?;
    if let Ok(parent) = File::open(dir) {
        let _ = parent.sync_all();
    }
    Ok(())
}

fn read_checkpoint(path: &Path, config: JournalConfig) -> io::Result<Checkpoint> {
    let mut file = secure_open_read(path)?;
    let mut fixed = [0u8; 39];
    file.read_exact(&mut fixed)?;
    if &fixed[..8] != CHECKPOINT_MAGIC || u16::from_le_bytes([fixed[8], fixed[9]]) != FORMAT_VERSION
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unsupported terminal checkpoint",
        ));
    }
    let size = ScreenSize::new(
        u16::from_le_bytes([fixed[10], fixed[11]]),
        u16::from_le_bytes([fixed[12], fixed[13]]),
    );
    let seq = u64::from_le_bytes(fixed[14..22].try_into().expect("fixed slice"));
    let bytes_seen = u64::from_le_bytes(fixed[22..30].try_into().expect("fixed slice"));
    let truncated = fixed[30] != 0;
    let len = u32::from_le_bytes(fixed[31..35].try_into().expect("fixed slice"));
    let checksum = u32::from_le_bytes(fixed[35..39].try_into().expect("fixed slice"));
    if len as usize > config.max_checkpoint_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "terminal checkpoint exceeds its configured limit",
        ));
    }
    let mut replay = vec![0u8; len as usize];
    file.read_exact(&mut replay)?;
    if checksum != checkpoint_checksum(size, seq, bytes_seen, truncated, len, &replay) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "terminal checkpoint checksum mismatch",
        ));
    }
    Ok(Checkpoint {
        size,
        seq,
        bytes_seen,
        truncated,
        replay,
    })
}

fn checkpoint_checksum(
    size: ScreenSize,
    seq: u64,
    bytes_seen: u64,
    truncated: bool,
    len: u32,
    replay: &[u8],
) -> u32 {
    let mut hasher = Hasher::new();
    hasher.update(&size.rows.to_le_bytes());
    hasher.update(&size.cols.to_le_bytes());
    hasher.update(&seq.to_le_bytes());
    hasher.update(&bytes_seen.to_le_bytes());
    hasher.update(&[u8::from(truncated)]);
    hasher.update(&len.to_le_bytes());
    hasher.update(replay);
    hasher.finalize()
}

fn record_checksum(kind: u8, seq: u64, len: u32, payload: &[u8]) -> u32 {
    let mut hasher = Hasher::new();
    hasher.update(&[kind]);
    hasher.update(&seq.to_le_bytes());
    hasher.update(&len.to_le_bytes());
    hasher.update(payload);
    hasher.finalize()
}

fn reset_journal(dir: &Path) -> io::Result<File> {
    let path = dir.join(JOURNAL_FILE);
    let mut file = secure_create(&path)?;
    file.write_all(JOURNAL_MAGIC)?;
    file.write_all(&FORMAT_VERSION.to_le_bytes())?;
    file.sync_all()?;
    Ok(file)
}

fn secure_directory_tree(dir: &Path) -> io::Result<()> {
    // The caller supplies `<history root>/<session>/<node>`. All three contain raw
    // terminal data or names that identify it, so all three are private.
    for path in [dir.parent().and_then(Path::parent), dir.parent(), Some(dir)]
        .into_iter()
        .flatten()
    {
        match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "terminal history path is not a private directory: {}",
                        path.display()
                    ),
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => std::fs::create_dir(path)?,
            Err(error) => return Err(error),
        }
        #[cfg(unix)]
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn secure_create(path: &Path) -> io::Result<File> {
    reject_symlink(path)?;
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).read(true).write(true);
    #[cfg(unix)]
    options.mode(0o600);
    let file = options.open(path)?;
    secure_file_permissions(path)?;
    Ok(file)
}

fn secure_open_read(path: &Path) -> io::Result<File> {
    reject_symlink(path)?;
    let file = OpenOptions::new().read(true).open(path)?;
    secure_file_permissions(path)?;
    Ok(file)
}

fn secure_open_read_write(path: &Path) -> io::Result<File> {
    reject_symlink(path)?;
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    options.mode(0o600);
    let file = options.open(path)?;
    secure_file_permissions(path)?;
    Ok(file)
}

fn secure_file_permissions(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

fn reject_symlink(path: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("terminal history file is a symlink: {}", path.display()),
        )),
        Ok(metadata) if !metadata.is_file() => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("terminal history path is not a file: {}", path.display()),
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn append(journal: &mut TerminalJournal, buffer: &mut TerminalBuffer, bytes: &[u8]) {
        buffer.write(bytes);
        journal.record_output(bytes, buffer).unwrap();
    }

    #[test]
    fn a_terminal_reopens_with_unicode_colour_cursor_modes_and_scrollback() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("terminal-history/sess_test/proc_test");
        let mut buffer = TerminalBuffer::new(ScreenSize::new(4, 18));
        let mut journal = TerminalJournal::create(&dir, &buffer, JournalConfig::default()).unwrap();
        append(
            &mut journal,
            &mut buffer,
            b"one\r\ntwo\r\n\x1b[31mred\x1b[0m \xf0\x9f\xa6\x80\r\nfour\r\nfive",
        );
        append(&mut journal, &mut buffer, b"\x1b[?1h\x1b[?2004h\x1b[2;3H");
        let resized = ScreenSize::new(6, 24);
        buffer.resize(resized);
        journal.record_resize(resized, &mut buffer).unwrap();
        append(&mut journal, &mut buffer, b" after resize");
        let expected_state = buffer.state_replay();
        let expected_snapshot = buffer.snapshot();
        let expected_scrollback = buffer.screen().scrollback();
        drop(journal);

        let recovered = TerminalJournal::recover(&dir, JournalConfig::default())
            .unwrap()
            .unwrap();
        assert_eq!(recovered.buffer.state_replay(), expected_state);
        assert_eq!(recovered.buffer.snapshot(), expected_snapshot);
        assert_eq!(recovered.buffer.screen().scrollback(), expected_scrollback);
        assert_eq!(recovered.buffer.size(), resized);
        assert!(recovered.buffer.screen().application_cursor());
        assert!(recovered.buffer.screen().bracketed_paste());
    }

    #[test]
    fn an_active_alternate_screen_reopens_as_the_same_terminal_state() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("terminal-history/sess_test/proc_alt");
        let mut buffer = TerminalBuffer::new(ScreenSize::new(5, 24));
        let config = JournalConfig {
            max_journal_bytes: JOURNAL_HEADER_BYTES + RECORD_HEADER_BYTES + 8,
            max_checkpoint_bytes: 16 * 1024,
        };
        let mut journal = TerminalJournal::create(&dir, &buffer, config).unwrap();
        append(
            &mut journal,
            &mut buffer,
            b"normal history\r\nsecond line\r\n",
        );
        append(
            &mut journal,
            &mut buffer,
            b"\x1b[?1049h\x1b[?1h\x1b[?2004h\x1b[32mALT \xf0\x9f\xa6\x80\x1b[0m\x1b[3;7H",
        );
        let expected = buffer.state_replay();
        drop(journal);

        let recovered = TerminalJournal::recover(&dir, config).unwrap().unwrap();
        assert_eq!(recovered.buffer.state_replay(), expected);
        assert!(recovered.buffer.screen().alternate_screen());
        assert!(recovered.buffer.screen().application_cursor());
        assert!(recovered.buffer.screen().bracketed_paste());
        assert_eq!(recovered.buffer.screen().cursor_position(), (2, 6));
        assert!(recovered.buffer.snapshot().text().contains("ALT"));
        assert!(recovered.buffer.is_truncated());
    }

    #[test]
    fn a_truncated_last_write_recovers_to_the_last_valid_record() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("terminal-history/sess_test/proc_test");
        let mut buffer = TerminalBuffer::new(ScreenSize::new(3, 20));
        let mut journal = TerminalJournal::create(&dir, &buffer, JournalConfig::default()).unwrap();
        append(&mut journal, &mut buffer, b"complete line");
        let expected = buffer.snapshot();
        drop(journal);

        let path = dir.join(JOURNAL_FILE);
        let original = std::fs::metadata(&path).unwrap().len();
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(&[RECORD_OUTPUT]).unwrap();
        file.write_all(&2u64.to_le_bytes()[..3]).unwrap();
        drop(file);

        let recovered = TerminalJournal::recover(&dir, JournalConfig::default())
            .unwrap()
            .unwrap();
        assert_eq!(recovered.buffer.snapshot(), expected);
        assert!(recovered.buffer.is_truncated());
        assert_eq!(std::fs::metadata(path).unwrap().len(), original);
    }

    #[test]
    fn rotation_bounds_disk_and_preserves_the_visible_terminal() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("terminal-history/sess_test/proc_test");
        let config = JournalConfig {
            max_journal_bytes: JOURNAL_HEADER_BYTES + RECORD_HEADER_BYTES + 24,
            max_checkpoint_bytes: 16 * 1024,
        };
        let mut buffer = TerminalBuffer::new(ScreenSize::new(3, 24));
        let mut journal = TerminalJournal::create(&dir, &buffer, config).unwrap();
        for index in 0..20 {
            append(
                &mut journal,
                &mut buffer,
                format!("line {index:02}\r\n").as_bytes(),
            );
        }
        let expected = buffer.snapshot();
        drop(journal);

        assert!(
            std::fs::metadata(dir.join(JOURNAL_FILE)).unwrap().len() <= config.max_journal_bytes
        );
        assert!(
            std::fs::metadata(dir.join(CHECKPOINT_FILE)).unwrap().len()
                <= config.max_checkpoint_bytes as u64 + 39
        );
        let recovered = TerminalJournal::recover(&dir, config).unwrap().unwrap();
        assert_eq!(recovered.buffer.snapshot().lines, expected.lines);
        assert!(recovered.buffer.is_truncated());
    }

    #[cfg(unix)]
    #[test]
    fn terminal_history_is_owner_only() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("terminal-history/sess_test/proc_test");
        let buffer = TerminalBuffer::new(ScreenSize::default());
        let _journal = TerminalJournal::create(&dir, &buffer, JournalConfig::default()).unwrap();

        for private_dir in [
            dir.parent().unwrap().parent().unwrap(),
            dir.parent().unwrap(),
            &dir,
        ] {
            assert_eq!(
                std::fs::metadata(private_dir).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        for file in [dir.join(CHECKPOINT_FILE), dir.join(JOURNAL_FILE)] {
            assert_eq!(
                std::fs::metadata(file).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn creating_a_journal_refuses_a_symlinked_node_directory() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("terminal-history");
        let session = root.join("sess_test");
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(&session).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let node = session.join("proc_test");
        symlink(&outside, &node).unwrap();

        let buffer = TerminalBuffer::new(ScreenSize::default());
        assert!(TerminalJournal::create(&node, &buffer, JournalConfig::default()).is_err());
        assert!(std::fs::read_dir(&outside).unwrap().next().is_none());
    }
}
