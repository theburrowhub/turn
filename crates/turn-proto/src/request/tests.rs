//! One-of-each fixtures and the tests that pin the request catalogue.
//!
//! Kept in its own file so the catalogue itself stays readable: the enum is the
//! thing a reader comes here for.

use super::*;
use crate::screen::PaneStream;

fn session() -> SessionId {
    SessionId::from_stored("sess_req00001")
}

fn node() -> NodeId {
    NodeId::from_stored("proc_req00001")
}

#[test]
fn a_request_serialises_with_its_operation_as_a_tag() {
    let request = Request::WritePty {
        session_id: session(),
        node_id: node(),
        data: TerminalBytes::new(b"y\r".to_vec()),
    };
    let json = serde_json::to_string(&request).unwrap();
    assert!(json.contains("\"op\":\"write_pty\""), "got {json}");
    assert_eq!(serde_json::from_str::<Request>(&json).unwrap(), request);
    assert_eq!(request.op(), "write_pty");
}

#[test]
fn attention_focus_names_the_semantic_subject_not_a_guessed_pane_owner() {
    let request = Request::FocusPaneForAttention {
        surface_id: "window-a".into(),
        session_id: session(),
        subject_node_id: NodeId::from_stored("proc_reviewer"),
    };
    let json = serde_json::to_string(&request).unwrap();
    assert!(json.contains("\"op\":\"focus_pane_for_attention\""));
    assert!(json.contains("\"subject_node_id\":\"proc_reviewer\""));
    assert_eq!(request.expected_result(), "pane_focus");
    assert_eq!(request.session_id(), Some(&session()));
    assert_eq!(serde_json::from_str::<Request>(&json).unwrap(), request);
}

/// The five zones are the whole vocabulary of a pane drag, so their wire names are
/// part of the contract a second frontend would be written against.
#[test]
fn a_relocation_names_the_drop_zone_it_was_dropped_on() {
    for (zone, wire) in [
        (DropZone::Left, "left"),
        (DropZone::Right, "right"),
        (DropZone::Above, "above"),
        (DropZone::Below, "below"),
        (DropZone::Centre, "centre"),
    ] {
        let request = Request::RelocatePane {
            session_id: session(),
            moved: PaneId::from_stored("pane_req0001"),
            target: PaneId::from_stored("pane_req0002"),
            zone,
        };
        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("\"op\":\"relocate_pane\""), "got {json}");
        assert!(json.contains(&format!("\"zone\":\"{wire}\"")), "got {json}");
        assert_eq!(serde_json::from_str::<Request>(&json).unwrap(), request);
        assert_eq!(request.op(), "relocate_pane");
        assert_eq!(request.expected_result(), "layout");
        assert!(request.is_mutating());
        assert_eq!(request.session_id(), Some(&session()));
    }
}

/// `swap_panes` is the older spelling of a `centre` relocation. It stays on the wire
/// for the clients that already send it, and it answers with the same result — so a
/// client can migrate one call at a time rather than all at once.
#[test]
fn swapping_panes_is_the_older_spelling_of_a_centre_relocation() {
    let swap = Request::SwapPanes {
        session_id: session(),
        a: PaneId::from_stored("pane_req0001"),
        b: PaneId::from_stored("pane_req0002"),
    };
    let relocate = Request::RelocatePane {
        session_id: session(),
        moved: PaneId::from_stored("pane_req0001"),
        target: PaneId::from_stored("pane_req0002"),
        zone: DropZone::Centre,
    };
    assert_ne!(swap.op(), relocate.op(), "two wire names, not one");
    assert_eq!(swap.expected_result(), relocate.expected_result());
    assert_eq!(swap.session_id(), relocate.session_id());
    assert_eq!(swap.is_mutating(), relocate.is_mutating());
}

#[test]
fn keystrokes_survive_the_wire_byte_for_byte() {
    // A control character, a paste with a newline, and a non-UTF-8 byte.
    let raw = b"\x03echo hi\ncaf\xc3\xa9\xff".to_vec();
    let request = Request::WritePty {
        session_id: session(),
        node_id: node(),
        data: TerminalBytes::new(raw.clone()),
    };
    let json = serde_json::to_string(&request).unwrap();
    match serde_json::from_str::<Request>(&json).unwrap() {
        Request::WritePty { data, .. } => assert_eq!(data.as_slice(), raw.as_slice()),
        other => panic!("wrong variant back: {other:?}"),
    }
}

#[test]
fn every_operation_name_matches_its_serialised_tag() {
    for request in all_requests() {
        let value = serde_json::to_value(&request).unwrap();
        let tag = value.get("op").and_then(|v| v.as_str()).unwrap();
        assert_eq!(
            tag,
            request.op(),
            "the wire tag and op() disagree for {request:?}"
        );
    }
}

#[test]
fn every_request_round_trips_unchanged() {
    for request in all_requests() {
        let json = serde_json::to_string(&request).unwrap();
        let back: Request = serde_json::from_str(&json).unwrap();
        assert_eq!(back, request, "round trip lost information: {json}");
    }
}

#[test]
fn optional_fields_are_omitted_rather_than_sent_as_null() {
    let json = serde_json::to_string(&Request::ListSessions {
        workspace_id: None,
        include_archived: false,
    })
    .unwrap();
    assert_eq!(
        json,
        "{\"op\":\"list_sessions\",\"include_archived\":false}"
    );

    // And a client that omits the defaults entirely is accepted.
    let parsed: Request = serde_json::from_str("{\"op\":\"list_sessions\"}").unwrap();
    assert_eq!(
        parsed,
        Request::ListSessions {
            workspace_id: None,
            include_archived: false
        }
    );
}

/// Forward compatibility: a newer daemon adds a field, an older client keeps
/// working. `deny_unknown_fields` is deliberately absent everywhere.
#[test]
fn an_unknown_field_is_ignored_rather_than_rejected() {
    let parsed: Request =
        serde_json::from_str("{\"op\":\"next_attention\",\"something_from_the_future\":true}")
            .unwrap();
    assert_eq!(parsed, Request::NextAttention);
}

/// Cells are what a client gets when it does not say. The whole point of the change
/// that introduced this field: a renderer without a terminal emulator should not have
/// to know it needs to ask.
#[test]
fn attaching_without_naming_a_stream_asks_for_cells() {
    let parsed: Request = serde_json::from_str(
        "{\"op\":\"attach_pane\",\"session_id\":\"sess_a\",\"pane_id\":\"pane_a\",\
         \"size\":{\"rows\":40,\"cols\":120}}",
    )
    .expect("a client may omit the stream");
    match parsed {
        Request::AttachPane { stream, size, .. } => {
            assert_eq!(stream, PaneStream::Cells);
            assert_eq!(size, PtySize::new(40, 120));
        }
        other => panic!("wrong variant: {other:?}"),
    }

    // And a client that wants the escape stream itself has to say so.
    let bytes: Request = serde_json::from_str(
        "{\"op\":\"attach_pane\",\"session_id\":\"sess_a\",\"pane_id\":\"pane_a\",\
         \"size\":{\"rows\":40,\"cols\":120},\"stream\":\"bytes\"}",
    )
    .expect("bytes stay available");
    assert!(matches!(
        bytes,
        Request::AttachPane {
            stream: PaneStream::Bytes,
            ..
        }
    ));
}

#[test]
fn read_only_requests_are_distinguished_from_mutating_ones() {
    assert!(!Request::NextAttention.is_mutating());
    assert!(!Request::GetSession {
        session_id: session()
    }
    .is_mutating());
    assert!(!Request::ResyncPane {
        session_id: session(),
        pane_id: PaneId::from_stored("pane_a"),
    }
    .is_mutating());
    // Reading history and searching it are reads. A search borrows the parser's
    // scrollback offset and puts it back, so it changes nothing another client can see.
    assert!(!Request::GetPaneHistory {
        session_id: session(),
        pane_id: PaneId::from_stored("pane_a"),
        offset: 40,
    }
    .is_mutating());
    assert!(!Request::SearchPane {
        session_id: session(),
        pane_id: PaneId::from_stored("pane_a"),
        query: crate::search::SearchQuery::literal("error"),
    }
    .is_mutating());
    assert!(!Request::GetHierarchy {
        surface_id: "window-a".into(),
        include_archived: false,
    }
    .is_mutating());
    assert!(!Request::GetInspector {
        key: HierarchyKey::process(node()),
    }
    .is_mutating());
    assert!(!Request::GetPreviewHistory {
        session_id: session(),
        node_id: node(),
        limit: Some(20),
    }
    .is_mutating());
    assert!(Request::PrepareContextHandoff {
        session_id: session(),
        source_node_id: node(),
        target_node_id: NodeId::from_stored("proc_target"),
        mode: crate::ContextHandoffMode::ContinueWith,
        instruction: None,
    }
    .is_mutating());
    assert!(Request::SetTreeExpanded {
        surface_id: "window-a".into(),
        key: HierarchyKey::session(session()),
        expanded: true,
    }
    .is_mutating());
    assert!(Request::GotoAttention { attention_id: None }.is_mutating());
    assert!(Request::KillNode {
        session_id: session(),
        node_id: node()
    }
    .is_mutating());
}

#[test]
fn a_session_scoped_request_reports_the_session_it_targets() {
    assert_eq!(
        Request::ZoomPane {
            session_id: session(),
            pane_id: PaneId::from_stored("pane_a"),
        }
        .session_id(),
        Some(&session())
    );
    assert_eq!(Request::ListTemplates.session_id(), None);
    assert_eq!(
        Request::ListAttention { session_id: None }.session_id(),
        None
    );
}

/// Closing has no default disposition, because guessing would either kill the
/// user's work or leak processes they thought were gone.
#[test]
fn closing_requires_an_explicit_decision_about_the_processes() {
    let missing =
        serde_json::from_str::<Request>("{\"op\":\"close_session\",\"session_id\":\"sess_a\"}");
    assert!(
        missing.is_err(),
        "the daemon must not have to guess whether to kill anything"
    );

    let explicit: Request = serde_json::from_str(
        "{\"op\":\"close_session\",\"session_id\":\"sess_a\",\"disposition\":\"keep_processes\"}",
    )
    .unwrap();
    assert_eq!(
        explicit,
        Request::CloseSession {
            session_id: SessionId::from_stored("sess_a"),
            disposition: CloseDisposition::KeepProcesses,
        }
    );
}

#[test]
fn releasing_a_write_lease_requires_its_fencing_generation() {
    let missing = serde_json::from_str::<Request>(
        "{\"op\":\"release_workspace_write_lease\",\"workspace_id\":\"ws_a\",\
         \"lease_id\":\"lease_a\"}",
    );
    assert!(
        missing.is_err(),
        "a stale client must not release a newer lease generation"
    );

    let request = Request::ReleaseWorkspaceWriteLease {
        workspace_id: WorkspaceId::from_stored("ws_a"),
        lease_id: LeaseId::from_stored("lease_a"),
        expected_generation: 9,
    };
    let json = serde_json::to_string(&request).unwrap();
    assert!(json.contains("\"expected_generation\":9"), "got {json}");
    assert_eq!(serde_json::from_str::<Request>(&json).unwrap(), request);
}

/// The protocol has no way to say "approve that permission", and no way to say
/// "run this command you found in the output". Both are absences by design, so
/// a test pins them: a future variant matching these names should have to
/// argue with this test first.
#[test]
fn the_catalogue_contains_no_way_to_approve_or_to_run_inferred_commands() {
    let names: Vec<&'static str> = all_requests().iter().map(|r| r.op()).collect();
    for forbidden in [
        "approve_permission",
        "deny_permission",
        "respond_permission",
        "run_command",
        "run_suggested_command",
        "execute",
    ] {
        assert!(
            !names.contains(&forbidden),
            "{forbidden} must not exist: answering an agent is the user typing, \
             and Turn never runs a command it inferred"
        );
    }
    // The only way to answer an agent is to type at it.
    assert!(names.contains(&"write_pty"));
    // And the only way to start something again is to be asked to.
    assert!(names.contains(&"relaunch_node"));
}

#[test]
fn a_user_correction_carries_either_axis_or_both() {
    let request = Request::CorrectState {
        session_id: session(),
        node_id: node(),
        lifecycle: Some(Lifecycle::Alive),
        turn: Some(Turn::Idle),
        note: Some("it is not waiting on me".into()),
    };
    let json = serde_json::to_string(&request).unwrap();
    assert!(json.contains("\"lifecycle\""), "got {json}");
    assert_eq!(serde_json::from_str::<Request>(&json).unwrap(), request);

    // Correcting only the turn axis is valid and leaves the process alone.
    let turn_only: Request = serde_json::from_str(
        "{\"op\":\"correct_state\",\"session_id\":\"sess_a\",\"node_id\":\"proc_a\",\
         \"turn\":{\"kind\":\"done\"}}",
    )
    .unwrap();
    match turn_only {
        Request::CorrectState {
            lifecycle, turn, ..
        } => {
            assert!(lifecycle.is_none());
            assert_eq!(turn, Some(Turn::Done));
        }
        other => panic!("wrong variant: {other:?}"),
    }
}

#[test]
fn user_activity_reuses_the_domains_own_context_type() {
    let request = Request::UpdateUserActivity {
        context: UserContext {
            last_keystroke_ms: Some(1_700_000_000_000),
            app_foreground: true,
            active_session: Some(session()),
            sensitive_operation: false,
        },
    };
    let json = serde_json::to_string(&request).unwrap();
    assert!(
        json.contains("\"last_keystroke_ms\":1700000000000"),
        "got {json}"
    );
    assert_eq!(serde_json::from_str::<Request>(&json).unwrap(), request);
}

#[test]
fn a_new_pane_omits_everything_it_was_not_given() {
    let json = serde_json::to_string(&NewPane::new(PaneKind::Shell)).unwrap();
    assert_eq!(
        json, "{\"kind\":\"shell\",\"restore\":\"reattach_only\"}",
        "a pane with no command must not carry empty arrays"
    );
}

#[test]
fn a_legacy_handoff_request_defaults_to_continue_with() {
    let request: Request = serde_json::from_value(serde_json::json!({
        "op": "prepare_context_handoff",
        "session_id": "sess_legacy_handoff",
        "source_node_id": "proc_legacy_source",
        "target_node_id": "proc_legacy_target"
    }))
    .expect("the additive mode field must preserve older clients");
    assert!(matches!(
        request,
        Request::PrepareContextHandoff {
            mode: crate::ContextHandoffMode::ContinueWith,
            ..
        }
    ));
}

/// One of each variant, used by the catalogue-wide tests above. Adding a
/// variant without adding it here makes `every_variant_is_covered` fail.
pub(crate) fn all_requests() -> Vec<Request> {
    let session_id = session();
    let workspace_id = WorkspaceId::from_stored("ws_req00001");
    let node_id = node();
    let pane_id = PaneId::from_stored("pane_req0001");
    let template_id = TemplateId::from_stored("tpl_req00001");
    let attention_id = AttentionId::from_stored("attn_req0001");
    let checkout_id = CheckoutId::from_stored("checkout_req001");
    let lease_id = LeaseId::from_stored("lease_req0001");
    let handoff_id = HandoffId::from_stored("handoff_req001");

    vec![
        Request::ListWorkspaces {
            include_archived: true,
        },
        Request::CreateWorkspace {
            name: "turn".into(),
            root: "/Users/x/turn".into(),
        },
        Request::RenameWorkspace {
            workspace_id: workspace_id.clone(),
            name: "renamed".into(),
        },
        Request::ArchiveWorkspace {
            workspace_id: workspace_id.clone(),
            archived: true,
        },
        Request::DuplicateWorkspace {
            workspace_id: workspace_id.clone(),
            name: Some("copy".into()),
        },
        Request::CloseWorkspace {
            workspace_id: workspace_id.clone(),
            disposition: CloseDisposition::KeepProcesses,
        },
        Request::DeleteWorkspace {
            workspace_id: workspace_id.clone(),
            disposition: CloseDisposition::Terminate,
        },
        Request::GetHierarchy {
            surface_id: "window-a".into(),
            include_archived: false,
        },
        Request::GetInspector {
            key: HierarchyKey::process(node_id.clone()),
        },
        Request::SetTreeExpanded {
            surface_id: "window-a".into(),
            key: HierarchyKey::workspace(workspace_id.clone()),
            expanded: true,
        },
        Request::SelectTreeNode {
            surface_id: "window-a".into(),
            selected: Some(HierarchyKey::process(node_id.clone())),
        },
        Request::SetTreeExpandedAll {
            surface_id: "window-a".into(),
            expanded: true,
        },
        Request::SetTreePresentation {
            surface_id: "window-a".into(),
            filters: vec![turn_core::model::TreeFilter::Attention],
            visibility_mode: turn_core::model::TreeVisibilityMode::Technical,
            scroll_anchor: Some(HierarchyKey::process(node_id.clone())),
        },
        Request::MoveTreeNode {
            surface_id: "window-a".into(),
            key: HierarchyKey::process(node_id.clone()),
            before: None,
        },
        Request::RenameNode {
            session_id: session_id.clone(),
            node_id: node_id.clone(),
            name: "Reviewer".into(),
        },
        Request::CorrectRelationship {
            session_id: session_id.clone(),
            node_id: node_id.clone(),
            parent_node_id: None,
            relationship_kind: turn_core::model::RelationshipKind::Unknown,
        },
        Request::GetWorkspaceWriteLease {
            workspace_id: workspace_id.clone(),
        },
        Request::AcquireWorkspaceWriteLease {
            workspace_id: workspace_id.clone(),
            session_id: session_id.clone(),
            checkout_id: checkout_id.clone(),
        },
        Request::ReleaseWorkspaceWriteLease {
            workspace_id: workspace_id.clone(),
            lease_id,
            expected_generation: 4,
        },
        Request::ListSessions {
            workspace_id: Some(workspace_id.clone()),
            include_archived: false,
        },
        Request::CreateSession {
            workspace_id: workspace_id.clone(),
            name: "Fix the flaky test".into(),
            cwd: Some("/repo".into()),
            panes: Some(vec![NewPane::new(PaneKind::Agent).with_command("claude")]),
            note: None,
            tags: vec!["bug".into()],
        },
        Request::CreateReadOnlySession {
            workspace_id: workspace_id.clone(),
            name: "Review the change".into(),
            cwd: None,
            panes: Some(vec![NewPane::new(PaneKind::Agent).with_command("claude")]),
            note: Some("No writes".into()),
            tags: vec!["review".into()],
        },
        Request::CreateReadOnlySessionFromTemplate {
            workspace_id: workspace_id.clone(),
            template_id: template_id.clone(),
            name: None,
            cwd: Some("crates/turn-core".into()),
            branch: Some("feat/attention".into()),
            task: Some("Review the queue".into()),
        },
        Request::CreateWorktreeSession {
            workspace_id: workspace_id.clone(),
            name: "Fix independently".into(),
            branch: "turn/fix-independently".into(),
            worktree_path: None,
            panes: Some(vec![NewPane::new(PaneKind::Agent).with_command("codex")]),
            note: None,
            tags: vec!["isolated".into()],
        },
        Request::CreateWorktreeSessionFromTemplate {
            workspace_id: workspace_id.clone(),
            template_id: template_id.clone(),
            name: Some("Fix independently".into()),
            cwd: Some("crates/turn-core".into()),
            template_branch: Some("feat/attention".into()),
            task: Some("Fix the queue".into()),
            branch: "turn/fix-independently".into(),
            worktree_path: None,
        },
        Request::CreateSessionFromTemplate {
            workspace_id: workspace_id.clone(),
            template_id: template_id.clone(),
            name: None,
            cwd: None,
            branch: Some("feat/attention".into()),
            task: None,
        },
        Request::RenameSession {
            session_id: session_id.clone(),
            name: "New name".into(),
        },
        Request::ArchiveSession {
            session_id: session_id.clone(),
            archived: false,
        },
        Request::DuplicateSession {
            session_id: session_id.clone(),
        },
        Request::SetSessionFavourite {
            session_id: session_id.clone(),
            favourite: true,
        },
        Request::SetSessionPinned {
            session_id: session_id.clone(),
            pinned: true,
        },
        Request::CloseSession {
            session_id: session_id.clone(),
            disposition: CloseDisposition::Terminate,
        },
        Request::DeleteSession {
            session_id: session_id.clone(),
            disposition: CloseDisposition::Terminate,
        },
        Request::GetSession {
            session_id: session_id.clone(),
        },
        Request::GetProcessTree {
            session_id: session_id.clone(),
        },
        Request::GetPreviewHistory {
            session_id: session_id.clone(),
            node_id: node_id.clone(),
            limit: Some(20),
        },
        Request::SetPreviewVisibility {
            session_id: session_id.clone(),
            node_id: node_id.clone(),
            visibility: turn_core::model::PreviewVisibility::Hide,
        },
        Request::PrepareContextHandoff {
            session_id: session_id.clone(),
            source_node_id: node_id.clone(),
            target_node_id: NodeId::from_stored("proc_req00002"),
            mode: crate::ContextHandoffMode::ReviewHandoff,
            instruction: Some(ContextHandoffText::new("Summarise the reviewed findings")),
        },
        Request::DeliverContextHandoff {
            session_id: session_id.clone(),
            handoff_id,
        },
        Request::ListTemplates,
        Request::GetTemplate {
            template_id: template_id.clone(),
        },
        Request::CreateLayoutTemplate {
            name: "Two tools".into(),
            layout: Box::new(turn_core::model::Layout::single(
                turn_core::model::Pane::new(PaneKind::Shell),
            )),
            description: Some("A reusable draft".into()),
        },
        Request::CreateTemplate {
            template: Box::new(turn_core::model::Template::two_shells(1_700_000_000_000)),
        },
        Request::SaveLayoutAsTemplate {
            session_id: session_id.clone(),
            name: "My shape".into(),
            description: None,
            hotkey: Some("cmd+shift+4".into()),
        },
        Request::UpdateTemplate {
            template_id: template_id.clone(),
            template: Box::new(turn_core::model::Template::two_shells(1_700_000_000_000)),
        },
        Request::DuplicateTemplate {
            template_id: template_id.clone(),
            name: "Two shells copy".into(),
        },
        Request::DeleteTemplate {
            template_id: template_id.clone(),
        },
        Request::SetWorkspaceDefaultTemplate {
            workspace_id: workspace_id.clone(),
            template_id: Some(template_id.clone()),
        },
        Request::ApplyTemplateToSession {
            session_id: session_id.clone(),
            template_id: template_id.clone(),
        },
        Request::SplitPane {
            session_id: session_id.clone(),
            pane_id: pane_id.clone(),
            direction: Direction::Horizontal,
            pane: NewPane::new(PaneKind::Shell).with_command("zsh"),
        },
        Request::ClosePane {
            session_id: session_id.clone(),
            pane_id: pane_id.clone(),
            disposition: CloseDisposition::Kill,
        },
        Request::ResizePane {
            session_id: session_id.clone(),
            pane_id: pane_id.clone(),
            delta: 0.15,
        },
        Request::ResizeDivider {
            session_id: session_id.clone(),
            before: pane_id.clone(),
            after: PaneId::from_stored("pane_req0002"),
            delta: 0.15,
        },
        Request::EqualizeDivider {
            session_id: session_id.clone(),
            before: pane_id.clone(),
            after: PaneId::from_stored("pane_req0002"),
        },
        Request::ApplyLayoutPreset {
            session_id: session_id.clone(),
            preset: turn_core::model::LayoutPreset::Grid,
        },
        Request::FocusPane {
            session_id: session_id.clone(),
            target: FocusTarget::Pane {
                pane_id: pane_id.clone(),
            },
        },
        Request::RelocatePane {
            session_id: session_id.clone(),
            moved: pane_id.clone(),
            target: PaneId::from_stored("pane_req0002"),
            zone: DropZone::Below,
        },
        Request::SwapPanes {
            session_id: session_id.clone(),
            a: pane_id.clone(),
            b: PaneId::from_stored("pane_req0002"),
        },
        Request::ZoomPane {
            session_id: session_id.clone(),
            pane_id: pane_id.clone(),
        },
        Request::OpenNodeAsTemporaryPane {
            surface_id: "window-a".into(),
            session_id: session_id.clone(),
            node_id: node_id.clone(),
        },
        Request::FocusPaneForNode {
            surface_id: "window-a".into(),
            session_id: session_id.clone(),
            node_id: node_id.clone(),
        },
        Request::FocusPaneForAttention {
            surface_id: "window-a".into(),
            session_id: session_id.clone(),
            subject_node_id: node_id.clone(),
        },
        Request::AttachPane {
            session_id: session_id.clone(),
            pane_id: pane_id.clone(),
            size: PtySize::new(48, 160),
            stream: PaneStream::Cells,
        },
        Request::ResyncPane {
            session_id: session_id.clone(),
            pane_id: pane_id.clone(),
        },
        Request::PaneImage {
            session_id: session_id.clone(),
            pane_id: pane_id.clone(),
            image_id: crate::images::ImageId(0x1234_5678_9abc_def0),
        },
        Request::DetachPane {
            session_id: session_id.clone(),
            pane_id: pane_id.clone(),
        },
        Request::GetPaneHistory {
            session_id: session_id.clone(),
            pane_id: pane_id.clone(),
            offset: 1_240,
        },
        Request::SearchPane {
            session_id: session_id.clone(),
            pane_id: pane_id.clone(),
            query: crate::search::SearchQuery::regex("error\\[E\\d+\\]"),
        },
        Request::WritePty {
            session_id: session_id.clone(),
            node_id: node_id.clone(),
            data: TerminalBytes::new(b"\x03".to_vec()),
        },
        Request::ResizePty {
            session_id: session_id.clone(),
            node_id: node_id.clone(),
            size: PtySize::new(24, 80),
        },
        Request::InterruptNode {
            session_id: session_id.clone(),
            node_id: node_id.clone(),
        },
        Request::TerminateNode {
            session_id: session_id.clone(),
            node_id: node_id.clone(),
        },
        Request::KillNode {
            session_id: session_id.clone(),
            node_id: node_id.clone(),
        },
        Request::RelaunchNode {
            session_id: session_id.clone(),
            node_id: node_id.clone(),
            resume: true,
        },
        Request::NextAttention,
        Request::ListAttention {
            session_id: Some(session_id.clone()),
        },
        Request::GotoAttention {
            attention_id: Some(attention_id.clone()),
        },
        Request::AcknowledgeAttention {
            attention_id: attention_id.clone(),
        },
        Request::SnoozeAttention {
            attention_id: attention_id.clone(),
            until_ms: 1_700_000_060_000,
        },
        Request::SetAttentionPriority {
            attention_id: attention_id.clone(),
            priority_boost: 30,
        },
        Request::DismissAttention { attention_id },
        Request::MuteSession {
            session_id: session_id.clone(),
            until_ms: Some(1_700_000_600_000),
        },
        Request::CorrectState {
            session_id: session_id.clone(),
            node_id,
            lifecycle: Some(Lifecycle::Alive),
            turn: Some(Turn::Idle),
            note: Some("still working".into()),
        },
        Request::UpdateUserActivity {
            context: UserContext {
                last_keystroke_ms: Some(1_700_000_000_000),
                app_foreground: true,
                active_session: Some(session_id.clone()),
                sensitive_operation: false,
            },
        },
        Request::GetSettings {
            session_id: Some(session_id),
        },
        Request::SetSetting {
            scope: turn_core::settings::Scope::Workspace,
            owner_id: Some("ws_req00001".into()),
            key: "appearance.font_size".into(),
            value: serde_json::json!(15),
        },
        Request::ResetSetting {
            scope: turn_core::settings::Scope::Session,
            owner_id: Some("sess_req00001".into()),
            key: "appearance.font_size".into(),
        },
    ]
}

/// Guards the fixture above against a variant being added and forgotten.
#[test]
fn every_variant_is_covered_by_the_catalogue_fixture() {
    let names: std::collections::HashSet<&'static str> =
        all_requests().iter().map(|r| r.op()).collect();
    assert_eq!(
        names.len(),
        all_requests().len(),
        "the fixture has two requests with the same op"
    );
    // 87 operations. The number is asserted so that adding one without documenting it in
    // docs/PROTOCOL.md becomes a deliberate act.
    //
    // What it does *not* do is notice a variant that was added to `Request` and never added
    // to the fixture below — the set is built from the fixture, so an absent variant is
    // absent from both sides of the comparison. The compile-time guards for that are
    // `Request::op` and `Request::expected_result`, which are exhaustive matches and cannot
    // be left alone when a variant appears. This assertion guards the *documentation*.
    assert_eq!(names.len(), 87, "the catalogue changed size: {names:?}");
}
