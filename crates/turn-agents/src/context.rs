//! Bounded, read-only observations of agent context usage.
//!
//! Each supported CLI records context usage in a different transcript shape. This
//! module keeps those wire details at the adapter boundary and returns only facts
//! the transcript can establish: used input tokens, an optional context window,
//! and an optional model id. It deliberately does not calculate a percentage. A
//! missing denominator is different from a zero-percent session and must remain
//! visible to callers as [`None`].

use serde_json::{Map, Value};
use std::{
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom},
    path::Path,
};

/// Maximum transcript data parsed in one observation.
///
/// Only the latest usage record matters. Bounding the tail prevents an old or
/// hostile transcript from turning a status refresh into an unbounded read and
/// parse. If the cap cuts through a JSONL record, that first fragment is ignored.
pub const MAX_CONTEXT_TAIL_BYTES: usize = 1024 * 1024;

/// Provider transcript format to parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptFormat {
    Claude,
    Codex,
    Gemini,
}

/// Context facts established from a provider's transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextObservation {
    /// Input-side tokens occupying the most recently reported live context.
    pub used_tokens: u64,
    /// Context window stated by the transcript or deterministically resolved by
    /// the provider CLI. `None` means the available evidence has no denominator.
    pub window_tokens: Option<u64>,
    /// Model id reported for the observation, sanitised for display/storage.
    pub model: Option<String>,
}

/// Result of a bounded transcript file read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextTailRead {
    pub observation: Option<ContextObservation>,
    /// Actual bytes read from disk. Never exceeds [`MAX_CONTEXT_TAIL_BYTES`].
    pub bytes_read: usize,
    /// Whether older source bytes were deliberately left unread.
    pub source_truncated: bool,
}

/// Parse the latest trustworthy observation from transcript bytes.
///
/// Inputs larger than [`MAX_CONTEXT_TAIL_BYTES`] are treated exactly like a file
/// tail: only the final bounded region is considered and its possibly partial
/// first line is discarded.
pub fn parse_context_tail(
    format: TranscriptFormat,
    transcript: &[u8],
) -> Option<ContextObservation> {
    let (tail, source_truncated) = if transcript.len() > MAX_CONTEXT_TAIL_BYTES {
        (
            &transcript[transcript.len() - MAX_CONTEXT_TAIL_BYTES..],
            true,
        )
    } else {
        (transcript, false)
    };
    parse_bounded_tail(format, tail, source_truncated)
}

/// Read and parse at most the final [`MAX_CONTEXT_TAIL_BYTES`] of a regular file.
///
/// A non-file path is rejected before opening it, avoiding an ordinary FIFO or
/// device path that could otherwise block a synchronous status refresh. The
/// opened handle is checked again before it is read, and `Read::take` enforces the
/// byte cap even if the file grows concurrently.
pub fn read_context_tail(
    path: impl AsRef<Path>,
    format: TranscriptFormat,
) -> io::Result<ContextTailRead> {
    let path = path.as_ref();
    let path_metadata = fs::metadata(path)?;
    if !path_metadata.is_file() {
        return Err(not_regular_file(path));
    }

    let mut file = File::open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(not_regular_file(path));
    }

    let length = metadata.len();
    let start = length.saturating_sub(MAX_CONTEXT_TAIL_BYTES as u64);
    file.seek(SeekFrom::Start(start))?;

    let expected = length.min(MAX_CONTEXT_TAIL_BYTES as u64) as usize;
    let mut tail = Vec::with_capacity(expected);
    (&mut file)
        .take(MAX_CONTEXT_TAIL_BYTES as u64)
        .read_to_end(&mut tail)?;

    let source_truncated = start > 0;
    let observation = parse_bounded_tail(format, &tail, source_truncated);
    Ok(ContextTailRead {
        observation,
        bytes_read: tail.len(),
        source_truncated,
    })
}

fn not_regular_file(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("transcript is not a regular file: {}", path.display()),
    )
}

fn parse_bounded_tail(
    format: TranscriptFormat,
    tail: &[u8],
    source_truncated: bool,
) -> Option<ContextObservation> {
    debug_assert!(tail.len() <= MAX_CONTEXT_TAIL_BYTES);
    let complete_tail = discard_partial_first_line(tail, source_truncated);
    match format {
        TranscriptFormat::Claude => parse_claude(complete_tail),
        TranscriptFormat::Codex => parse_codex(complete_tail),
        TranscriptFormat::Gemini => parse_gemini(complete_tail),
    }
}

fn discard_partial_first_line(tail: &[u8], source_truncated: bool) -> &[u8] {
    if !source_truncated {
        return tail;
    }
    tail.iter()
        .position(|byte| *byte == b'\n')
        .map_or(&[], |newline| &tail[newline + 1..])
}

fn parse_claude(tail: &[u8]) -> Option<ContextObservation> {
    let mut used_tokens = None;
    let mut model = None;

    for line in tail.rsplit(|byte| *byte == b'\n') {
        if !contains(line, b"\"assistant\"") || !contains(line, b"\"usage\"") {
            continue;
        }
        let Some(value) = json_value(line) else {
            continue;
        };
        let Some(root) = value.as_object() else {
            continue;
        };
        if string_member(root, "type") != Some("assistant") {
            continue;
        }
        let Some(message) = object_member(root, "message") else {
            continue;
        };
        let Some(usage) = object_member(message, "usage") else {
            continue;
        };
        let Some(used) = claude_input_tokens(usage) else {
            continue;
        };

        if used_tokens.is_none() {
            used_tokens = Some(used);
        }
        if model.is_none() {
            model = safe_model(message.get("model"));
        }
        if model.is_some() {
            break;
        }
    }

    used_tokens.map(|used_tokens| ContextObservation {
        used_tokens,
        // Claude transcripts do not state the effective account/session window.
        // A model-family capability is not evidence that this session received it.
        window_tokens: None,
        model,
    })
}

fn claude_input_tokens(usage: &Map<String, Value>) -> Option<u64> {
    let mut saw_input_field = false;
    let mut total = 0_u64;
    for key in [
        "input_tokens",
        "cache_read_input_tokens",
        "cache_creation_input_tokens",
    ] {
        let Some(value) = usage.get(key) else {
            continue;
        };
        saw_input_field = true;
        total = total.checked_add(value.as_u64()?)?;
    }
    (saw_input_field && total > 0).then_some(total)
}

fn parse_codex(tail: &[u8]) -> Option<ContextObservation> {
    let mut usage = None;
    let mut model = None;

    for line in tail.rsplit(|byte| *byte == b'\n') {
        let may_have_usage = usage.is_none() && contains(line, b"\"last_token_usage\"");
        let may_have_model = model.is_none() && contains(line, b"\"turn_context\"");
        if !may_have_usage && !may_have_model {
            continue;
        }
        let Some(value) = json_value(line) else {
            continue;
        };
        let Some(root) = value.as_object() else {
            continue;
        };

        if may_have_usage {
            usage = codex_usage(root);
        }
        if may_have_model && string_member(root, "type") == Some("turn_context") {
            model =
                object_member(root, "payload").and_then(|payload| safe_model(payload.get("model")));
        }
        if usage.is_some() && model.is_some() {
            break;
        }
    }

    usage.map(|(used_tokens, window_tokens)| ContextObservation {
        used_tokens,
        window_tokens,
        model,
    })
}

fn codex_usage(root: &Map<String, Value>) -> Option<(u64, Option<u64>)> {
    if string_member(root, "type") != Some("event_msg") {
        return None;
    }
    let payload = object_member(root, "payload")?;
    if string_member(payload, "type") != Some("token_count") {
        return None;
    }
    let info = object_member(payload, "info")?;
    let last = object_member(info, "last_token_usage")?;
    let used_tokens = positive_u64(last.get("input_tokens"))?;
    let window_tokens = positive_u64(info.get("model_context_window"));
    Some((used_tokens, window_tokens))
}

fn parse_gemini(tail: &[u8]) -> Option<ContextObservation> {
    for line in tail.rsplit(|byte| *byte == b'\n') {
        if !contains(line, b"\"tokens\"") {
            continue;
        }
        let Some(value) = json_value(line) else {
            continue;
        };
        let Some(root) = value.as_object() else {
            continue;
        };
        if string_member(root, "type") != Some("gemini") {
            continue;
        }
        let Some(tokens) = object_member(root, "tokens") else {
            continue;
        };
        let Some(used_tokens) = positive_u64(tokens.get("input")) else {
            continue;
        };
        let model = safe_model(root.get("model"));
        let window_tokens = model.as_deref().map(gemini_window);
        return Some(ContextObservation {
            used_tokens,
            window_tokens,
            model,
        });
    }
    None
}

fn gemini_window(model: &str) -> u64 {
    match model {
        // These are the only exceptions in the CLI's token-limit resolver.
        "gemma-4-31b-it" | "gemma-4-26b-a4b-it" => 256_000,
        // The CLI itself applies this catch-all to known, unknown and future ids.
        _ => 1_048_576,
    }
}

fn json_value(line: &[u8]) -> Option<Value> {
    serde_json::from_slice(line).ok()
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn object_member<'a>(object: &'a Map<String, Value>, key: &str) -> Option<&'a Map<String, Value>> {
    object.get(key)?.as_object()
}

fn string_member<'a>(object: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    object.get(key)?.as_str()
}

fn positive_u64(value: Option<&Value>) -> Option<u64> {
    value?.as_u64().filter(|number| *number > 0)
}

fn safe_model(value: Option<&Value>) -> Option<String> {
    value?.as_str().and_then(crate::text::field)
}
