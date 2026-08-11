//! Contract tests for Gemini CLI 0.46.0.
//!
//! The shapes in the versioned fixture are transcribed from the hook reference
//! bundled with that exact installed release. Re-recording instructions live in
//! `docs/ADAPTER_ACCEPTANCE.md`; changing the supported version requires a new
//! fixture rather than silently editing this one.

use serde_json::Value;
use tempfile::TempDir;
use turn_agents::adapter::{AgentAdapter, EventContext, HookEndpoint, LaunchContext};
use turn_agents::gemini::CONTRACT_VERSION;
use turn_agents::{GeminiCliAdapter, IntegrationLevel};
use turn_core::event::{Confidence, EventKind, EventSource};
use turn_core::ids::{NodeId, SessionId};

const FIXTURE: &str = include_str!("fixtures/gemini-cli-0.46.0.json");

fn fixtures() -> Value {
    serde_json::from_str(FIXTURE).unwrap()
}

fn context() -> EventContext {
    EventContext {
        session_id: SessionId::from_stored("sess_gemini_contract"),
        node_id: NodeId::from_stored("node_gemini_contract"),
        timestamp_ms: 1_723_000_000_000,
    }
}

fn normalise(name: &str) -> Vec<turn_core::event::TurnEvent> {
    GeminiCliAdapter::new().normalise(&fixtures()[name], &context())
}

#[test]
fn fixture_name_records_the_supported_tool_version() {
    assert_eq!(CONTRACT_VERSION, "0.46.0");
}

#[test]
fn documented_lifecycle_question_permission_and_model_shapes_normalise() {
    assert!(matches!(
        normalise("SessionStart")[0].kind,
        EventKind::AgentStarted { .. }
    ));
    assert!(matches!(
        normalise("BeforeAgent")[0].kind,
        EventKind::AgentTurnStarted { .. }
    ));
    match &normalise("BeforeModel")[0].kind {
        EventKind::AgentStarted { model, .. } => {
            assert_eq!(model.as_deref(), Some("gemini-2.5-pro"));
        }
        other => panic!("unexpected {other:?}"),
    }
    match &normalise("BeforeToolAskUser")[0].kind {
        EventKind::AgentQuestionAsked { question } => {
            assert_eq!(question, "Which implementation should I keep?");
        }
        other => panic!("unexpected {other:?}"),
    }
    match &normalise("NotificationToolPermission")[0].kind {
        EventKind::AgentPermissionRequired { command, .. } => {
            assert_eq!(command.as_deref(), Some("cargo test"));
        }
        other => panic!("unexpected {other:?}"),
    }
    assert!(matches!(
        normalise("AfterAgent")[0].kind,
        EventKind::AgentTurnCompleted { .. }
    ));
    assert!(matches!(
        normalise("SessionEnd")[0].kind,
        EventKind::AgentIdle
    ));
}

#[test]
fn every_emitted_event_records_explicit_confidence_and_its_hook_source() {
    for name in fixtures().as_object().unwrap().keys() {
        for event in normalise(name) {
            assert_eq!(event.confidence, Confidence::Explicit, "{name}");
            assert!(matches!(
                event.source,
                EventSource::Hook { ref tool, .. } if tool == "gemini-cli"
            ));
        }
    }
}

#[test]
fn launch_is_non_blocking_and_waits_for_live_evidence_before_claiming_structured() {
    let temp = TempDir::new().unwrap();
    let helper = temp.path().join("turn hook 'quoted'");
    std::fs::write(&helper, b"helper").unwrap();
    let ctx = LaunchContext {
        session_id: SessionId::from_stored("sess_gemini_contract"),
        node_id: NodeId::from_stored("node_gemini_contract"),
        cwd: "/repo".into(),
        command: "gemini".into(),
        user_args: vec!["--model".into(), "gemini-2.5-pro".into()],
        endpoint: HookEndpoint {
            base_url: "http://127.0.0.1:51234".into(),
            token: "tok_gemini".into(),
            helper_path: Some(helper),
        },
        scratch_dir: temp.path().join("scratch"),
    };
    let plan = GeminiCliAdapter::new().prepare(&ctx).unwrap();
    assert_eq!(plan.level, IntegrationLevel::Heuristic);
    assert_eq!(plan.args, ctx.user_args);
    let settings = plan
        .env
        .iter()
        .find(|(key, _)| key == "GEMINI_CLI_SYSTEM_DEFAULTS_PATH")
        .map(|(_, value)| value)
        .unwrap();
    let document: Value = serde_json::from_slice(&std::fs::read(settings).unwrap()).unwrap();
    assert!(document["hooks"]["BeforeTool"].is_array());
    let command = document["hooks"]["SessionStart"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    assert!(command.ends_with("; printf '{}'"));
    assert!(!command.contains("tok_gemini"));

    let mut without_helper = ctx;
    without_helper.endpoint.helper_path = None;
    let fallback = GeminiCliAdapter::new().prepare(&without_helper).unwrap();
    assert_eq!(fallback.level, IntegrationLevel::Heuristic);
    assert!(!fallback
        .env
        .iter()
        .any(|(key, _)| key == "GEMINI_CLI_SYSTEM_DEFAULTS_PATH"));
}
