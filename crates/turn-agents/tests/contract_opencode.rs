//! Contract tests for OpenCode 1.18.16.
//!
//! The versioned payloads follow the official event bridge and schema sources
//! at tag `v1.18.16`. Reproduction and upgrade steps live in
//! `docs/ADAPTER_ACCEPTANCE.md`.

use serde_json::Value;
use tempfile::TempDir;
use turn_agents::adapter::{AgentAdapter, EventContext, HookEndpoint, LaunchContext};
use turn_agents::opencode::CONTRACT_VERSION;
use turn_agents::{IntegrationLevel, OpenCodeAdapter};
use turn_core::event::{Confidence, EventKind, EventSource};
use turn_core::ids::{NodeId, SessionId};

const FIXTURE: &str = include_str!("fixtures/opencode-1.18.16.json");

fn fixtures() -> Value {
    serde_json::from_str(FIXTURE).unwrap()
}

fn context() -> EventContext {
    EventContext {
        session_id: SessionId::from_stored("sess_opencode_contract"),
        node_id: NodeId::from_stored("node_opencode_contract"),
        timestamp_ms: 1_723_000_000_000,
    }
}

fn normalise(name: &str) -> Vec<turn_core::event::TurnEvent> {
    OpenCodeAdapter::new().normalise(&fixtures()[name], &context())
}

#[test]
fn fixture_and_payload_record_the_supported_tool_version() {
    assert_eq!(CONTRACT_VERSION, "1.18.16");
    assert_eq!(
        fixtures()["SessionCreated"]["properties"]["info"]["version"],
        "1.18.16"
    );
}

#[test]
fn official_session_permission_question_failure_and_subagent_shapes_normalise() {
    match &normalise("SessionCreated")[0].kind {
        EventKind::AgentStarted {
            model, external_id, ..
        } => {
            assert_eq!(model.as_deref(), Some("gpt-5.2"));
            assert_eq!(external_id.as_deref(), Some("ses_root"));
        }
        other => panic!("unexpected {other:?}"),
    }
    assert!(matches!(
        normalise("SessionBusy")[0].kind,
        EventKind::AgentTurnStarted { .. }
    ));
    assert!(matches!(
        normalise("PermissionAsked")[0].kind,
        EventKind::AgentPermissionRequired { .. }
    ));
    assert!(matches!(
        normalise("PermissionReplied")[0].kind,
        EventKind::AgentPermissionResolved { allowed: true }
    ));
    assert!(matches!(
        normalise("QuestionAsked")[0].kind,
        EventKind::AgentQuestionAsked { .. }
    ));
    match &normalise("ChildCreated")[0].kind {
        EventKind::AgentSpawned { agent_id, .. } => {
            assert_eq!(agent_id.as_deref(), Some("ses_child"));
        }
        other => panic!("unexpected {other:?}"),
    }
    assert!(matches!(
        normalise("SessionError")[0].kind,
        EventKind::AgentFailed { .. }
    ));
    assert!(matches!(
        normalise("SessionIdle")[0].kind,
        EventKind::AgentTurnCompleted { .. }
    ));
}

#[test]
fn every_emitted_event_records_explicit_confidence_and_its_plugin_source() {
    for name in fixtures().as_object().unwrap().keys() {
        for event in normalise(name) {
            assert_eq!(event.confidence, Confidence::Explicit, "{name}");
            assert!(matches!(
                event.source,
                EventSource::Hook { ref tool, .. } if tool == "opencode"
            ));
        }
    }
}

#[test]
fn merged_plugin_config_is_fire_and_forget_and_pure_mode_degrades_safely() {
    let temp = TempDir::new().unwrap();
    let mut ctx = LaunchContext {
        session_id: SessionId::from_stored("sess_opencode_contract"),
        node_id: NodeId::from_stored("node_opencode_contract"),
        cwd: "/repo".into(),
        command: "opencode".into(),
        user_args: vec!["--model".into(), "openai/gpt-5.2".into()],
        endpoint: HookEndpoint {
            base_url: "http://127.0.0.1:51234".into(),
            token: "tok_opencode".into(),
            helper_path: None,
        },
        scratch_dir: temp.path().join("scratch"),
    };
    let adapter = OpenCodeAdapter::new();
    let plan = adapter.prepare(&ctx).unwrap();
    assert_eq!(plan.level, IntegrationLevel::Heuristic);
    let config = plan
        .env
        .iter()
        .find(|(key, _)| key == "OPENCODE_CONFIG_DIR")
        .map(|(_, value)| value)
        .unwrap();
    let plugin =
        std::fs::read_to_string(std::path::Path::new(config).join("plugins/turn-observer.js"))
            .unwrap();
    assert!(plugin.contains("void fetch"));
    assert!(plugin.contains(".catch(() => {})"));
    assert!(!plugin.contains("tok_opencode"));

    ctx.user_args.push("--pure".into());
    let fallback = adapter.prepare(&ctx).unwrap();
    assert_eq!(fallback.level, IntegrationLevel::Heuristic);
    assert!(!fallback
        .env
        .iter()
        .any(|(key, _)| key == "OPENCODE_CONFIG_DIR"));
}

#[test]
fn resume_uses_the_official_session_flag_and_rejects_flag_injection() {
    let adapter = OpenCodeAdapter::new();
    assert_eq!(
        adapter.resume_args("ses_123"),
        Some(vec!["--session".into(), "ses_123".into()])
    );
    assert_eq!(adapter.resume_args("--pure"), None);
}
