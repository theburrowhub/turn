//! Push catalogue tests: one of each, and the product rules that must survive
//! the trip to the renderer.

use super::*;
use crate::screen::ScreenUpdate;
use turn_core::attention::{DeferReason, FocusDenial};
use turn_core::event::{Confidence, EventKind, EventSource};
use turn_core::ids::{CheckoutId, WorkspaceId};
use turn_core::model::{
    ActivityPreview, Pane, PaneKind, PaneNodeBinding, PreviewSource, ProcessNode, Relation,
    Session, SessionTree, WorkspaceWriteLease,
};
use turn_core::state::AwaitingReason;

const T0: i64 = 1_700_000_000_000;

fn session() -> Session {
    Session::new(
        WorkspaceId::from_stored("ws_evt00001"),
        "Fix the flaky test",
        "/repo",
        Layout::single(Pane::new(PaneKind::Agent).with_command("claude")),
        T0,
    )
}

/// The default terminal push: what changed, as cells, with a sequence number the
/// client uses to notice it missed something.
#[test]
fn a_screen_update_carries_the_changed_rows_and_a_sequence_number() {
    let before = crate::cells::Grid::blank(24, 80);
    let mut after = before.clone();
    for (col, ch) in "compiling".chars().enumerate() {
        if let Some(cell) = after.cell_mut(0, col as u16) {
            cell.text = ch.to_string();
            cell.fg = Some(crate::cells::Rgb::new(0xcd, 0xcd, 0x00));
        }
    }

    let event = ServerEvent::PaneScreen {
        session_id: SessionId::from_stored("sess_a"),
        pane_id: PaneId::from_stored("pane_a"),
        node_id: Some(NodeId::from_stored("proc_a")),
        seq: 1_234,
        update: ScreenUpdate::between(&before, &after),
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("\"event\":\"pane_screen\""), "got {json}");
    assert!(json.contains("\"mode\":\"rows\""), "got {json}");

    match serde_json::from_str::<ServerEvent>(&json).unwrap() {
        ServerEvent::PaneScreen { seq, update, .. } => {
            assert_eq!(seq, 1_234);
            assert_eq!(update.row_count(), 1, "only the row that changed");
            // And applying it to the screen the client had reproduces the daemon's.
            let mut client = before.clone();
            update.apply(&mut client).expect("the update applies");
            assert_eq!(client, after);
        }
        other => panic!("wrong variant: {other:?}"),
    }
    assert!(
        event.is_output(),
        "a screen update is terminal traffic and belongs on the renderer's path"
    );
}

#[test]
fn terminal_output_survives_as_bytes_with_a_sequence_number() {
    let raw = b"\x1b[2K\rcompiling turn-proto v0.1.0\xff".to_vec();
    let event = ServerEvent::PaneOutput {
        session_id: SessionId::from_stored("sess_a"),
        pane_id: PaneId::from_stored("pane_a"),
        node_id: Some(NodeId::from_stored("proc_a")),
        seq: 1_234,
        data: TerminalBytes::new(raw.clone()),
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("\"event\":\"pane_output\""), "got {json}");

    match serde_json::from_str::<ServerEvent>(&json).unwrap() {
        ServerEvent::PaneOutput { data, seq, .. } => {
            assert_eq!(data.as_slice(), raw.as_slice());
            assert_eq!(seq, 1_234);
        }
        other => panic!("wrong variant: {other:?}"),
    }
    assert!(event.is_output());
}

/// The daemon must be able to admit it dropped output rather than let a client
/// render a terminal that quietly missed a screenful.
#[test]
fn a_dropped_output_burst_is_reported_instead_of_hidden() {
    let event = ServerEvent::PaneOutputGap {
        session_id: SessionId::from_stored("sess_a"),
        pane_id: PaneId::from_stored("pane_a"),
        dropped: 512,
        resume_seq: 10_000,
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("\"dropped\":512"), "got {json}");
    assert_eq!(serde_json::from_str::<ServerEvent>(&json).unwrap(), event);
    assert!(event.is_output());
}

/// Case E on the wire: the turn ended, background work did not, and the push
/// carries both axes so the UI cannot collapse them.
#[test]
fn a_state_change_carries_both_axes_and_the_derived_projection() {
    let cause = TurnEvent::new(
        SessionId::from_stored("sess_a"),
        EventKind::AgentTurnCompleted {
            last_message: Some("tests are running".into()),
            background_tasks: 2,
        },
        EventSource::Hook {
            tool: "claude-code".into(),
            event_name: "Stop".into(),
        },
        Confidence::Explicit,
        T0,
    );
    let event = ServerEvent::NodeStateChanged {
        session_id: SessionId::from_stored("sess_a"),
        node_id: NodeId::from_stored("proc_a"),
        lifecycle: Lifecycle::Alive,
        turn: Some(Turn::Done),
        display_state: DisplayState::CompletedTurn,
        caused_by: Some(Box::new(cause)),
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(
        json.contains("\"display_state\":\"completed_turn\""),
        "got {json}"
    );
    assert!(json.contains("\"background_tasks\":2"));
    assert_eq!(serde_json::from_str::<ServerEvent>(&json).unwrap(), event);
}

#[test]
fn a_change_turn_made_itself_carries_no_cause_rather_than_a_fabricated_one() {
    let event = ServerEvent::NodeStateChanged {
        session_id: SessionId::from_stored("sess_a"),
        node_id: NodeId::from_stored("proc_a"),
        lifecycle: Lifecycle::Orphaned,
        turn: None,
        display_state: DisplayState::Running,
        caused_by: None,
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(!json.contains("caused_by"), "got {json}");
    assert!(
        !json.contains("\"turn\""),
        "a shell has no turn axis: {json}"
    );
    assert_eq!(serde_json::from_str::<ServerEvent>(&json).unwrap(), event);
}

/// A heuristic's opinion must arrive labelled as an opinion.
#[test]
fn a_heuristic_event_reaches_the_ui_still_marked_as_inferred() {
    let guessed = TurnEvent::new(
        SessionId::from_stored("sess_a"),
        EventKind::AgentWaitingForUser {
            reason: AwaitingReason::Permission,
            summary: Some("looks like a prompt".into()),
        },
        EventSource::PtyHeuristic {
            rule: "permission_box".into(),
        },
        Confidence::Explicit, // requested; the source caps it
        T0,
    );
    let event = ServerEvent::TurnEventEmitted {
        turn_event: guessed,
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(
        json.contains("\"confidence\":\"inferred_high\""),
        "a guess must not reach the UI dressed as a fact: {json}"
    );
    assert!(json.contains("\"pty_heuristic\""));

    match serde_json::from_str::<ServerEvent>(&json).unwrap() {
        ServerEvent::TurnEventEmitted { turn_event } => {
            assert!(turn_event.confidence.is_provisional());
            assert!(!turn_event.confidence.may_steal_focus());
        }
        other => panic!("wrong variant: {other:?}"),
    }
}

#[test]
fn a_subagent_appearing_pushes_the_whole_tree_with_its_confirmed_edge() {
    let s = session();
    let mut tree = SessionTree::new();
    let root = tree.insert(ProcessNode::agent(s.id.clone(), "claude", "/repo", T0));
    let mut sub = ProcessNode::agent(s.id.clone(), "explore", "/repo", T0);
    sub.kind = turn_core::model::NodeKind::Subagent;
    sub.link_to(root, Relation::Confirmed);
    tree.insert(sub);

    let event = ServerEvent::TreeChanged {
        session_id: s.id.clone(),
        nodes: TreeNodeView::flatten(&tree, T0),
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("\"event\":\"tree_changed\""), "got {json}");
    assert!(json.contains("\"kind\":\"spawned_by\""));
    assert!(json.contains("\"confidence\":\"explicit\""));
    assert_eq!(serde_json::from_str::<ServerEvent>(&json).unwrap(), event);
    assert_eq!(event.session_id(), Some(&s.id));
}

/// A restore reports what happened and offers; it must never have acted.
#[test]
fn a_restore_result_offers_a_relaunch_without_having_performed_one() {
    let s = session();
    let event = ServerEvent::RestoreResult {
        session_id: s.id.clone(),
        state: RestoreState::PartiallyRestored,
        needs_explanation: RestoreState::PartiallyRestored.needs_explanation(),
        panes: vec![
            PaneRestoreOutcome {
                pane_id: PaneId::from_stored("pane_alive"),
                node_id: NodeId::from_stored("proc_alive"),
                lifecycle: Lifecycle::Orphaned,
                can_relaunch: false,
                command: None,
                needs_checkout_write: true,
            },
            PaneRestoreOutcome {
                pane_id: PaneId::from_stored("pane_gone"),
                node_id: NodeId::from_stored("proc_gone"),
                lifecycle: Lifecycle::Lost,
                can_relaunch: true,
                command: Some("cargo watch -x test".into()),
                needs_checkout_write: true,
            },
        ],
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(
        json.contains("\"state\":\"partially_restored\""),
        "got {json}"
    );
    assert!(json.contains("\"needs_explanation\":true"));

    match serde_json::from_str::<ServerEvent>(&json).unwrap() {
        ServerEvent::RestoreResult { panes, .. } => {
            let lost = panes
                .iter()
                .find(|p| p.lifecycle == Lifecycle::Lost)
                .unwrap();
            assert!(lost.can_relaunch, "Turn offers");
            assert_eq!(lost.node_id, NodeId::from_stored("proc_gone"));
            assert_eq!(lost.command.as_deref(), Some("cargo watch -x test"));
        }
        other => panic!("wrong variant: {other:?}"),
    }
}

#[test]
fn attention_effects_are_routed_to_their_session() {
    let session_id = SessionId::from_stored("sess_a");
    let cases = [
        Effect::Badge {
            session_id: session_id.clone(),
            count: 3,
        },
        Effect::Focus {
            session_id: session_id.clone(),
            node_id: Some(NodeId::from_stored("proc_a")),
        },
        Effect::FocusDeferred {
            session_id: session_id.clone(),
            until_ms: T0 + 1_500,
            reason: DeferReason::UserTyping,
        },
        Effect::FocusDenied {
            session_id: session_id.clone(),
            reason: FocusDenial::PingPongGuard,
        },
        Effect::Cleared {
            session_id: session_id.clone(),
        },
    ];
    for effect in cases {
        let event = ServerEvent::AttentionEffect { effect };
        assert_eq!(event.session_id(), Some(&session_id));
        let json = serde_json::to_string(&event).unwrap();
        assert_eq!(serde_json::from_str::<ServerEvent>(&json).unwrap(), event);
    }
}

#[test]
fn every_push_round_trips_and_its_tag_matches_event_name() {
    for event in all_events() {
        let value = serde_json::to_value(&event).unwrap();
        let tag = value.get("event").and_then(|v| v.as_str()).unwrap();
        assert_eq!(tag, event.event_name(), "tag mismatch for {event:?}");

        let json = serde_json::to_string(&event).unwrap();
        assert_eq!(
            serde_json::from_str::<ServerEvent>(&json).unwrap(),
            event,
            "round trip lost information: {json}"
        );
    }
}

#[test]
fn only_terminal_traffic_is_classified_as_output() {
    let output = all_events().into_iter().filter(|e| e.is_output()).count();
    assert_eq!(
        output, 3,
        "only pane_screen, pane_output and pane_output_gap are output"
    );
}

/// One of each variant.
pub(crate) fn all_events() -> Vec<ServerEvent> {
    let s = session();
    let session_id = s.id.clone();
    let pane_id = s.layout.panes()[0].id.clone();
    let node_id = NodeId::from_stored("proc_evt0001");
    let preview = ActivityPreview {
        node_id: node_id.clone(),
        raw_source_sequence: Some(4),
        normalized_text: "Reviewing auth.rs".into(),
        source: PreviewSource::SemanticEvent,
        confidence: Confidence::Explicit,
        stable: true,
        contains_sensitive_data: false,
        redacted: false,
        updated_ms: T0 + 4,
    };
    let binding = PaneNodeBinding {
        pane_id: PaneId::from_stored("pane_temp_evt"),
        session_id: session_id.clone(),
        node_id: node_id.clone(),
        temporary: true,
        surface_id: Some("window-a".into()),
        opened_ms: T0 + 5,
    };
    let lease = WorkspaceWriteLease::active(
        s.workspace_id.clone(),
        session_id.clone(),
        CheckoutId::primary_for(&s.workspace_id),
        T0,
    );

    vec![
        ServerEvent::PaneScreen {
            session_id: session_id.clone(),
            pane_id: pane_id.clone(),
            node_id: Some(node_id.clone()),
            seq: 7,
            update: ScreenUpdate::full(crate::cells::Grid::from_lines(&["ready"], 20)),
        },
        ServerEvent::PaneOutput {
            session_id: session_id.clone(),
            pane_id: pane_id.clone(),
            node_id: Some(node_id.clone()),
            seq: 1,
            data: TerminalBytes::new(b"hello\r\n".to_vec()),
        },
        ServerEvent::PaneOutputGap {
            session_id: session_id.clone(),
            pane_id: pane_id.clone(),
            dropped: 4,
            resume_seq: 100,
        },
        ServerEvent::NodeStateChanged {
            session_id: session_id.clone(),
            node_id: node_id.clone(),
            lifecycle: Lifecycle::Signaled {
                signal: "Killed".into(),
            },
            turn: Some(Turn::Failed {
                reason: "killed".into(),
            }),
            display_state: DisplayState::Failed,
            caused_by: None,
        },
        ServerEvent::SessionStateChanged {
            session: Box::new(SessionSummary::from_session(&s, 1, false, T0)),
        },
        ServerEvent::SessionRemoved {
            session_id: session_id.clone(),
            workspace_id: s.workspace_id.clone(),
        },
        ServerEvent::TurnEventEmitted {
            turn_event: TurnEvent::new(
                session_id.clone(),
                EventKind::AgentSpawned {
                    declared_name: None,
                    agent_type: Some("Explore".into()),
                    agent_id: Some("sub-1".into()),
                    task: None,
                },
                EventSource::Hook {
                    tool: "claude-code".into(),
                    event_name: "SubagentStart".into(),
                },
                Confidence::Explicit,
                T0,
            ),
        },
        ServerEvent::AttentionEffect {
            effect: Effect::Badge {
                session_id: session_id.clone(),
                count: 2,
            },
        },
        ServerEvent::AttentionQueueChanged {
            entries: Vec::new(),
        },
        ServerEvent::TreeChanged {
            session_id: session_id.clone(),
            nodes: Vec::new(),
        },
        ServerEvent::HierarchyChanged {
            snapshot: Box::new(crate::HierarchySnapshot::empty("window-a", 8)),
        },
        ServerEvent::ActivityPreviewChanged {
            hierarchy_revision: 8,
            session_id: session_id.clone(),
            node_id: node_id.clone(),
            preview: Some(preview),
        },
        ServerEvent::PaneBindingsChanged {
            hierarchy_revision: 8,
            session_id: session_id.clone(),
            node_id: node_id.clone(),
            bindings: vec![binding],
        },
        ServerEvent::WorkspaceWriteLeaseChanged {
            hierarchy_revision: 8,
            workspace_id: s.workspace_id.clone(),
            lease: Some(lease),
        },
        ServerEvent::LayoutChanged {
            session_id: session_id.clone(),
            layout: s.layout.clone(),
        },
        ServerEvent::PtyResized {
            session_id: session_id.clone(),
            node_id,
            size: PtySize::new(50, 200),
        },
        ServerEvent::RestoreResult {
            session_id,
            state: RestoreState::Reattached,
            needs_explanation: false,
            panes: vec![PaneRestoreOutcome {
                pane_id,
                node_id: NodeId::from_stored("proc_back0001"),
                lifecycle: Lifecycle::Reconnected,
                can_relaunch: false,
                command: None,
                needs_checkout_write: true,
            }],
        },
    ]
}

#[test]
fn every_variant_is_covered_by_the_push_fixture() {
    let names: std::collections::HashSet<&'static str> =
        all_events().iter().map(|e| e.event_name()).collect();
    assert_eq!(
        names.len(),
        all_events().len(),
        "duplicate event in fixture"
    );
    assert_eq!(
        names.len(),
        17,
        "the push catalogue changed size: {names:?}"
    );
}
