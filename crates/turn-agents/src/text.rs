//! Everything an adapter reads out of a payload, treated as hostile.
//!
//! A hook payload arrives over loopback from a process holding a valid token, so
//! Turn knows *which session* sent it. That is the only thing it knows. The
//! contents are written by a language model, which is in turn steered by whatever
//! it has been reading — a repository, a web page, a pasted stack trace. So the
//! fields Turn lifts out of a payload are attacker-controlled text that ends up in
//! four places that all matter:
//!
//! * a native label, where [`turn_pty::is_display_safe`] decides what may render;
//! * a notification, which is the same problem with a wider audience;
//! * a log line, where a newline forges a second record;
//! * an argv position, where a leading `-` becomes a flag.
//!
//! The last one is the sharpest. An agent reports its own session id, Turn stores
//! it, and resuming later runs `claude --resume <that id>`. An id of
//! `--dangerously-skip-permissions` would therefore be handed to the tool as a
//! flag, on the user's behalf, from a string the agent chose. [`identifier`]
//! exists so that cannot happen: an id that is not shaped like an id is refused,
//! and a refused id means "we never learned one", which the resume path already
//! knows how to say.

use serde_json::Value;
use turn_pty::sanitise_label;

/// Longest short field (a model name, an agent type) Turn keeps.
pub const MAX_FIELD_CHARS: usize = 160;

/// Longest external identifier accepted. Claude Code uses a UUID, Codex a short
/// thread id and Agent Team member id; nothing legitimate comes close to this.
pub const MAX_IDENTIFIER_CHARS: usize = 128;

/// Longest command Turn stores and shows. Commands are *not* excerpted for
/// display below this: a truncated command hides its own tail, and the tail is
/// where `&& rm -rf /` lives.
pub const MAX_COMMAND_CHARS: usize = 4_096;

/// Why a payload command can or cannot be represented faithfully.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandText {
    /// Nothing legible remained after normalisation.
    Empty,
    /// The complete normalised command, with no omitted suffix.
    Complete(String),
    /// The complete command cannot fit Turn's bounded permission model.
    TooLong,
}

/// Summary used when Turn deliberately refuses to show a partial command.
pub const COMMAND_TOO_LONG_SUMMARY: &str =
    "Command exceeds Turn's 4096-character display limit; inspect the agent terminal before responding";

/// Cap on the raw payload kept beside an event for debugging.
pub const MAX_RAW_BYTES: usize = 8 * 1024;

/// Payload members that carry bulk content rather than state.
///
/// A `Write` permission request contains the entire file the agent is about to
/// write, and an `Edit` contains both sides of the change. Keeping those in the
/// event log would copy the user's source — and any credential inside it — into a
/// second file with a different lifetime, in exchange for nothing: Turn renders
/// none of it. Matched case-insensitively at any depth.
const BULK_KEYS: &[&str] = &[
    "content",
    "old_string",
    "new_string",
    "file_text",
    "patch",
    "diff",
    "edits",
    "transcript",
    "messages",
    "stdout",
    "stderr",
    "output",
];

/// Written in place of a member dropped for bulk.
pub const OMITTED: &str = "[omitted by turn]";

/// Collapses whitespace, removes anything that could misrepresent the text, and
/// caps the length on a character boundary.
///
/// Sanitising happens before the whitespace collapse, because an escape sequence
/// has to be consumed whole: dropping the `ESC` alone would leave a readable
/// `[2J` in the middle of a notification.
pub fn excerpt(text: &str, limit: usize) -> String {
    // Whitespace first, and as a substitution rather than a removal: a newline is
    // a word boundary as well as a control character, and deleting it would weld
    // "the" and "bug" into one word. This also neutralises U+2028 and U+0085,
    // which are word boundaries the sanitiser would otherwise drop silently.
    let spaced: String = text
        .chars()
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .collect();
    let safe = sanitise_label(&spaced, usize::MAX).unwrap_or_default();
    let collapsed = safe.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= limit {
        return collapsed;
    }
    let truncated: String = collapsed.chars().take(limit).collect();
    format!("{truncated}…")
}

/// A short field — a model name, an agent type, a tool name — made safe to render.
///
/// `None` when nothing legible survives, so a caller stores no field at all
/// rather than an empty label.
pub fn field(text: &str) -> Option<String> {
    let cleaned = excerpt(text, MAX_FIELD_CHARS);
    (!cleaned.is_empty()).then_some(cleaned)
}

/// A command made safe to display, or an explicit refusal to shorten it.
pub fn command(text: &str) -> CommandText {
    // Ask `excerpt` for one character beyond the contract so it can tell us the
    // semantic command is too large without retaining an attacker-sized result.
    // Its ellipsis is never returned to a permission view.
    let cleaned = excerpt(text, MAX_COMMAND_CHARS + 1);
    if cleaned.is_empty() {
        CommandText::Empty
    } else if cleaned.chars().count() > MAX_COMMAND_CHARS {
        CommandText::TooLong
    } else {
        CommandText::Complete(cleaned)
    }
}

/// An identifier Turn may later hand to a tool as an argument.
///
/// Deliberately a whitelist. The set is what real session, thread and subagent
/// ids are made of — Claude Code's UUIDs, Codex's `th_…` threads, Claude Code's
/// `sub-42` — and nothing in it can turn into a flag, a path traversal, a shell
/// metacharacter or a second argument. Anything else is refused outright rather
/// than repaired, because a *partly* rewritten id would resume the wrong
/// conversation, which is worse than admitting we do not have one.
pub fn identifier(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.chars().count() > MAX_IDENTIFIER_CHARS {
        return None;
    }
    // A leading dash is the whole attack: `--resume --dangerously-skip-permissions`.
    if trimmed.starts_with('-') {
        return None;
    }
    let acceptable = trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':' | '@'));
    acceptable.then(|| trimmed.to_string())
}

/// The payload to keep beside an event, minimised and bounded.
///
/// Two things happen here. Bulk members are dropped, because the event log must
/// not become a second copy of the user's files. What is left is capped, because
/// the log is per-event and the sender chooses the size.
///
/// The result is always valid JSON: an over-long payload is wrapped as an excerpt
/// rather than cut mid-object, so redaction downstream still sees a document it
/// can walk rather than a string it has to give up on.
pub fn raw_for_storage(payload: &Value) -> String {
    let mut pruned = payload.clone();
    prune(&mut pruned);
    let text = pruned.to_string();
    if text.len() <= MAX_RAW_BYTES {
        return text;
    }

    // Budgeted in bytes and cut on a character boundary, because the cap is a
    // storage bound and a payload is not ASCII: `MAX_RAW_BYTES / 2` *characters*
    // of CJK or emoji is several times the cap the field exists to enforce.
    let mut excerpt = String::new();
    for character in text.chars() {
        if excerpt.len() + character.len_utf8() > MAX_RAW_BYTES / 2 {
            break;
        }
        excerpt.push(character);
    }
    excerpt.push('…');
    serde_json::json!({
        "turn_truncated": true,
        "original_bytes": text.len(),
        "excerpt": excerpt,
    })
    .to_string()
}

fn prune(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, child) in map.iter_mut() {
                if BULK_KEYS.iter().any(|bulk| key.eq_ignore_ascii_case(bulk)) {
                    *child = Value::String(OMITTED.to_string());
                } else {
                    prune(child);
                }
            }
        }
        Value::Array(items) => {
            for item in items.iter_mut() {
                prune(item);
            }
        }
        Value::String(text) => {
            let safe = strip_misleading(text);
            if safe != *text {
                *text = safe;
            }
        }
        _ => {}
    }
}

/// Removes only the characters that survive JSON encoding *and* lie about the text
/// they are in.
///
/// The stored payload is deliberately faithful — it is what a bad adapter gets
/// debugged from, so `` should still be visible as having been there. JSON
/// encoding already makes control characters inert: they are written as six
/// readable characters, not as a live escape sequence, so keeping them costs
/// nothing and losing them would cost the one thing this field is for.
///
/// Bidirectional and invisible formatting is different. JSON does not escape it,
/// so it stays live: an event inspector rendering the payload would show a command
/// backwards, and a `grep` over the log would not explain why. Those go, and only
/// those.
fn strip_misleading(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_control() || turn_pty::is_display_safe(*c))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_excerpt_collapses_whitespace_and_caps_on_a_character_boundary() {
        assert_eq!(excerpt("fixed   the\nbug", 100), "fixed the bug");
        let long = excerpt(&"á".repeat(500), 240);
        assert_eq!(long.chars().count(), 241);
        assert!(long.ends_with('…'));
    }

    #[test]
    fn a_permission_command_is_complete_or_explicitly_refused_never_excerpted() {
        let exact = "x".repeat(MAX_COMMAND_CHARS);
        assert_eq!(command(&exact), CommandText::Complete(exact));

        let hidden_tail = format!("{} && rm -rf /", "x".repeat(MAX_COMMAND_CHARS));
        assert_eq!(command(&hidden_tail), CommandText::TooLong);
        assert_eq!(command("  \n  "), CommandText::Empty);
    }

    /// The attack this module exists for: an agent puts an escape sequence in a
    /// field it controls, and Turn shows that field in a notification.
    #[test]
    fn an_escape_sequence_in_a_payload_field_never_survives_into_an_excerpt() {
        let hostile = "rm -rf ./build\x1b]52;c;cGF5bG9hZA==\x07\x1b[2J\x1b[1;31mHARMLESS";
        let cleaned = excerpt(hostile, 240);

        assert!(!cleaned.contains('\x1b'), "got {cleaned:?}");
        assert!(!cleaned.contains('\x07'), "got {cleaned:?}");
        assert!(
            !cleaned.contains("52;c") && !cleaned.contains("[2J") && !cleaned.contains("1;31m"),
            "the tail of a consumed sequence must not be left as text: {cleaned:?}"
        );
        // The visible text on either side survives, which is the point: the user
        // still sees what the agent said, minus its ability to redraw the screen.
        assert_eq!(cleaned, "rm -rf ./buildHARMLESS");
    }

    #[test]
    fn a_newline_in_a_payload_field_cannot_forge_a_second_log_line() {
        for hostile in [
            "ok\nWARN forged log record",
            "ok\r\nWARN forged log record",
            "ok\u{2028}WARN forged log record",
            "ok\u{0085}WARN forged log record",
        ] {
            let cleaned = excerpt(hostile, 240);
            assert!(
                !cleaned.contains('\n') && !cleaned.contains('\r'),
                "{hostile:?} became {cleaned:?}"
            );
        }
    }

    #[test]
    fn a_direction_override_cannot_reverse_a_summary() {
        let cleaned = excerpt("rm -rf /\u{202e}gpj.elif", 240);
        assert!(!cleaned.contains('\u{202e}'), "got {cleaned:?}");
    }

    /// The identifier rules, spelled out. These are what stop an agent choosing
    /// its own command-line flag.
    #[test]
    fn an_identifier_that_could_become_a_flag_or_a_path_is_refused() {
        for hostile in [
            "--dangerously-skip-permissions",
            "-r",
            "--resume=other",
            "../../etc/passwd",
            "a b",
            "id;rm -rf /",
            "id$(whoami)",
            "id\nsecond",
            "id\"quoted",
            "",
            "   ",
            "\u{202e}di",
        ] {
            assert_eq!(identifier(hostile), None, "{hostile:?} must be refused");
        }
        assert_eq!(
            identifier(&"a".repeat(MAX_IDENTIFIER_CHARS + 1)),
            None,
            "an absurdly long id is not an id"
        );
    }

    #[test]
    fn the_identifiers_real_tools_actually_use_are_accepted() {
        for real in [
            "84cde77e-f54f-41e7-bb05-2716cb61b6bf",
            "th_9f2",
            "sub-42",
            "claude-abc123",
            "session.1",
            "ns:thread:7",
            "reviewer@session-a1b2c3d4",
        ] {
            assert_eq!(
                identifier(real).as_deref(),
                Some(real),
                "{real} is a real id"
            );
        }
    }

    /// A `Write` permission request carries the whole file. It must not be copied
    /// into the event log.
    #[test]
    fn bulk_members_are_dropped_from_the_stored_payload() {
        let payload = json!({
            "hook_event_name": "PermissionRequest",
            "tool_name": "Write",
            "tool_input": {
                "file_path": "/repo/.env",
                "content": "ANTHROPIC_API_KEY=sk-ant-not-a-real-key\n"
            }
        });
        let stored = raw_for_storage(&payload);

        assert!(!stored.contains("sk-ant-not-a-real-key"), "got {stored}");
        assert!(stored.contains(OMITTED), "got {stored}");
        assert!(
            stored.contains("/repo/.env") && stored.contains("Write"),
            "the shape a bad adapter is debugged from survives: {stored}"
        );
    }

    #[test]
    fn an_oversized_payload_is_stored_as_valid_json_rather_than_cut_in_half() {
        let payload = json!({
            "hook_event_name": "Stop",
            "notes": "x".repeat(MAX_RAW_BYTES * 2)
        });
        let stored = raw_for_storage(&payload);

        assert!(stored.len() < MAX_RAW_BYTES);
        let parsed: Value =
            serde_json::from_str(&stored).expect("what is stored must still be JSON");
        assert_eq!(parsed["turn_truncated"], json!(true));
        assert!(parsed["original_bytes"].as_u64().unwrap() > MAX_RAW_BYTES as u64);
    }

    /// The same cap, against text that is not ASCII. A byte budget spent by the
    /// character stores several times what it was asked to: the events an agent
    /// produces contain prose, paths and prompts in every script there is.
    #[test]
    fn an_oversized_payload_of_multibyte_text_still_respects_the_byte_cap() {
        for filler in ["é", "字", "🙂"] {
            let payload = json!({
                "hook_event_name": "Stop",
                "last_assistant_message": filler.repeat(MAX_RAW_BYTES)
            });
            let stored = raw_for_storage(&payload);

            assert!(
                stored.len() < MAX_RAW_BYTES,
                "{filler} filled {} bytes against a {MAX_RAW_BYTES} byte cap",
                stored.len()
            );
            let parsed: Value =
                serde_json::from_str(&stored).expect("what is stored must still be JSON");
            assert_eq!(parsed["turn_truncated"], json!(true));

            let excerpt = parsed["excerpt"].as_str().unwrap();
            assert!(excerpt.ends_with('…'), "got {excerpt:?}");
            let kept = excerpt.trim_end_matches('…');
            assert!(
                kept.len() <= MAX_RAW_BYTES / 2,
                "{filler} excerpt was {} bytes against a {} byte budget",
                kept.len(),
                MAX_RAW_BYTES / 2
            );
            // Still cut between characters, and far enough in to have reached the
            // multibyte text rather than stopping inside the JSON preamble.
            assert!(kept.contains(filler), "got {kept:?}");
            assert!(kept.is_char_boundary(kept.len()));
        }
    }

    #[test]
    fn an_ordinary_payload_is_stored_unchanged() {
        let payload = json!({ "hook_event_name": "Stop", "cwd": "/repo" });
        let stored = raw_for_storage(&payload);
        assert_eq!(
            serde_json::from_str::<Value>(&stored).unwrap(),
            payload,
            "nothing is lost from a payload that is already small and plain"
        );
    }
}
