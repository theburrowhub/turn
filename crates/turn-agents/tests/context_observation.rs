use std::io::Write;

use turn_agents::{
    parse_context_tail, read_context_tail, ContextObservation, TranscriptFormat,
    MAX_CONTEXT_TAIL_BYTES,
};

const CLAUDE: &[u8] = include_bytes!("fixtures/context/claude.jsonl");
const CODEX: &[u8] = include_bytes!("fixtures/context/codex.jsonl");
const GEMINI: &[u8] = include_bytes!("fixtures/context/gemini.jsonl");

#[test]
fn claude_uses_latest_input_side_usage_without_inventing_a_window() {
    assert_eq!(
        parse_context_tail(TranscriptFormat::Claude, CLAUDE),
        Some(ContextObservation {
            used_tokens: 15_000,
            window_tokens: None,
            model: Some("claude-opus-4-8".into()),
        })
    );
}

#[test]
fn claude_skips_invalid_newer_usage_and_checked_addition_cannot_wrap() {
    let mut transcript = CLAUDE.to_vec();
    transcript.extend_from_slice(
        br#"
{"type":"assistant","message":{"model":"false-latest","usage":{"input_tokens":18446744073709551615,"cache_read_input_tokens":1}}}
{"type":"assistant","message":{"model":"also-false","usage":{"input_tokens":-1}}}"#,
    );

    assert_eq!(
        parse_context_tail(TranscriptFormat::Claude, &transcript),
        Some(ContextObservation {
            used_tokens: 15_000,
            window_tokens: None,
            model: Some("claude-opus-4-8".into()),
        })
    );
}

#[test]
fn codex_uses_latest_request_not_cumulative_or_cached_tokens() {
    assert_eq!(
        parse_context_tail(TranscriptFormat::Codex, CODEX),
        Some(ContextObservation {
            used_tokens: 34_635,
            window_tokens: Some(258_400),
            model: Some("gpt-5.6-sol".into()),
        })
    );
}

#[test]
fn codex_does_not_borrow_an_older_window_or_clamp_raw_facts() {
    let transcript = br#"
{"type":"turn_context","payload":{"model":"gpt-test"}}
{"type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":80},"model_context_window":100}}}
{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":999999},"last_token_usage":{"input_tokens":300},"model_context_window":"unknown"}}}
"#;

    assert_eq!(
        parse_context_tail(TranscriptFormat::Codex, transcript),
        Some(ContextObservation {
            used_tokens: 300,
            window_tokens: None,
            model: Some("gpt-test".into()),
        })
    );
}

#[test]
fn gemini_uses_latest_top_level_message_and_cli_window_rule() {
    assert_eq!(
        parse_context_tail(TranscriptFormat::Gemini, GEMINI),
        Some(ContextObservation {
            used_tokens: 17_149,
            window_tokens: Some(1_048_576),
            model: Some("gemini-3.5-flash".into()),
        })
    );

    for model in ["gemma-4-31b-it", "gemma-4-26b-a4b-it"] {
        let transcript =
            format!(r#"{{"type":"gemini","tokens":{{"input":42}},"model":"{model}"}}"#);
        assert_eq!(
            parse_context_tail(TranscriptFormat::Gemini, transcript.as_bytes())
                .and_then(|observation| observation.window_tokens),
            Some(256_000)
        );
    }

    // The provider CLI deliberately applies its 1 Mi-token default to unknown
    // and newly released model ids; this is its rule, not Turn guessing a family.
    let future = br#"{"type":"gemini","tokens":{"input":42},"model":"future-model"}"#;
    assert_eq!(
        parse_context_tail(TranscriptFormat::Gemini, future)
            .and_then(|observation| observation.window_tokens),
        Some(1_048_576)
    );
}

#[test]
fn gemini_without_a_model_has_no_denominator() {
    let transcript = br#"{"type":"gemini","tokens":{"input":42,"total":1000}}"#;
    assert_eq!(
        parse_context_tail(TranscriptFormat::Gemini, transcript),
        Some(ContextObservation {
            used_tokens: 42,
            window_tokens: None,
            model: None,
        })
    );
}

#[test]
fn malformed_and_torn_latest_lines_do_not_hide_the_latest_valid_observation() {
    let mut transcript = GEMINI.to_vec();
    transcript.extend_from_slice(
        br#"
{"type":"gemini","tokens":{"input":0},"model":"invalid"}
{"type":"gemini","tokens":{"input":99999},"model":"torn""#,
    );
    assert_eq!(
        parse_context_tail(TranscriptFormat::Gemini, &transcript)
            .map(|observation| observation.used_tokens),
        Some(17_149)
    );
}

#[test]
fn direct_parsing_is_bounded_to_the_tail() {
    let mut transcript = br#"{"type":"gemini","tokens":{"input":42},"model":"gemini-old"}
"#
    .to_vec();
    transcript.resize(MAX_CONTEXT_TAIL_BYTES + transcript.len() + 32, b'x');

    assert_eq!(
        parse_context_tail(TranscriptFormat::Gemini, &transcript),
        None,
        "a record outside the final bounded tail must never be parsed"
    );
}

#[test]
fn file_reader_reports_and_enforces_its_one_mib_cap() {
    let mut transcript = tempfile::NamedTempFile::new().expect("temporary transcript");
    writeln!(
        transcript,
        r#"{{"type":"gemini","tokens":{{"input":7}},"model":"too-old"}}"#
    )
    .expect("write old observation");
    let padding = vec![b'x'; MAX_CONTEXT_TAIL_BYTES + 128];
    transcript
        .write_all(&padding)
        .expect("write bounded padding");
    transcript.write_all(b"\n").expect("finish padding line");
    transcript.flush().expect("flush transcript");

    let old = read_context_tail(transcript.path(), TranscriptFormat::Gemini)
        .expect("read bounded transcript");
    assert!(old.source_truncated);
    assert_eq!(old.bytes_read, MAX_CONTEXT_TAIL_BYTES);
    assert_eq!(old.observation, None);

    transcript
        .write_all(
            br#"{"type":"gemini","tokens":{"input":99},"model":"gemini-current"}
"#,
        )
        .expect("append current observation");
    transcript.flush().expect("flush current observation");

    let current = read_context_tail(transcript.path(), TranscriptFormat::Gemini)
        .expect("read current transcript");
    assert!(current.source_truncated);
    assert_eq!(current.bytes_read, MAX_CONTEXT_TAIL_BYTES);
    assert_eq!(
        current.observation,
        Some(ContextObservation {
            used_tokens: 99,
            window_tokens: Some(1_048_576),
            model: Some("gemini-current".into()),
        })
    );
}

#[test]
fn file_reader_rejects_non_files() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let error = read_context_tail(directory.path(), TranscriptFormat::Claude)
        .expect_err("directories are not transcripts");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
}
