//! The protocol exercised the way a second frontend would use it: public API
//! only, whole conversations, bytes on both sides.
//!
//! The unit tests inside the crate check each type. These check that the pieces
//! compose into the things the product promises — a UI that can be killed and
//! rebuilt without disturbing the processes, a stream that survives a hostile
//! peer, and a version mismatch that fails loudly instead of subtly.

use turn_core::attention::{AttentionPolicy, Effect, UserContext};
use turn_core::event::{Confidence, EventKind, EventSource, Risk, TurnEvent};
use turn_core::ids::{NodeId, PaneId, SessionId, WorkspaceId};
use turn_core::model::{
    Layout, Pane, PaneKind, ProcessNode, Relation, RestoreState, Session, SessionTree,
};
use turn_core::state::{AwaitingReason, Lifecycle, Turn};

use turn_proto::{
    encode, negotiate_within, ClientFrame, ClientMessage, ErrorCode, Grid, Hello, LineDecoder,
    NewPane, PaneAttachment, PaneRestoreOutcome, PaneStream, ProtoError, PtySize, Request,
    RequestId, Response, ScreenUpdate, ServerEvent, ServerFrame, ServerMessage, SessionSummary,
    TerminalBytes, TreeNodeView, Welcome, MAX_OUTPUT_CHUNK_BYTES, PROTOCOL_VERSION,
};

const T0: i64 = 1_700_000_000_000;

/// A minimal in-memory transport. Both sides write bytes; both sides decode with
/// the real [`LineDecoder`], so nothing here is faked except the socket.
struct Wire {
    to_daemon: Vec<u8>,
    to_client: Vec<u8>,
}

impl Wire {
    fn new() -> Self {
        Self {
            to_daemon: Vec::new(),
            to_client: Vec::new(),
        }
    }

    fn client_sends(&mut self, frame: &ClientFrame) {
        self.to_daemon
            .extend(encode(frame).expect("client frame encodes"));
    }

    fn daemon_sends(&mut self, frame: &ServerFrame) {
        self.to_client
            .extend(encode(frame).expect("server frame encodes"));
    }

    /// Everything the daemon has queued, decoded in order. Panics on a bad frame,
    /// which is what a client should never see from a healthy daemon.
    fn client_reads(&mut self) -> Vec<ServerFrame> {
        let mut decoder = LineDecoder::new();
        decoder.feed(&std::mem::take(&mut self.to_client));
        let mut out = Vec::new();
        while let Some(result) = decoder.next_message::<ServerFrame>() {
            out.push(result.expect("a healthy daemon sends only valid frames"));
        }
        assert_eq!(decoder.buffered(), 0, "a partial frame was left behind");
        out
    }

    fn daemon_reads(&mut self) -> Vec<ClientFrame> {
        let mut decoder = LineDecoder::new();
        decoder.feed(&std::mem::take(&mut self.to_daemon));
        let mut out = Vec::new();
        while let Some(result) = decoder.next_message::<ClientFrame>() {
            out.push(result.expect("a healthy client sends only valid frames"));
        }
        out
    }
}

fn session_with_blocked_agent() -> (Session, NodeId) {
    let mut session = Session::new(
        WorkspaceId::from_stored("ws_conv00001"),
        "Fix the flaky test",
        "/repo",
        Layout::single(Pane::new(PaneKind::Agent).with_command("claude")),
        T0,
    );
    let mut agent = ProcessNode::agent(session.id.clone(), "claude", "/repo", T0);
    agent.lifecycle = Lifecycle::Alive;
    agent.turn = Some(Turn::AwaitingUser {
        reason: AwaitingReason::Permission,
    });
    agent.pid = Some(4242);
    session
        .layout
        .get_mut(&session.layout.panes()[0].id.clone())
        .unwrap()
        .node_id = Some(agent.id.clone());
    let node_id = session.tree.insert(agent);
    (session, node_id)
}

/// The everyday path, end to end: handshake, list, create, attach, type, get told
/// something happened.
#[test]
fn a_full_working_conversation_completes_over_the_real_framing() {
    let mut wire = Wire::new();
    let (session, node_id) = session_with_blocked_agent();
    let pane_id = session.layout.panes()[0].id.clone();

    // 1. Handshake.
    wire.client_sends(&ClientFrame::hello(Hello::new("turn-ui", "0.1.0")));
    let hello = wire.daemon_reads();
    assert_eq!(hello.len(), 1);
    let agreed = hello[0]
        .negotiate()
        .expect("the current client is accepted");
    wire.daemon_sends(&ServerFrame::welcome(Welcome::new(
        agreed, "0.1.0", 4242, T0,
    )));
    match &wire.client_reads()[0].message {
        ServerMessage::Welcome(w) => assert_eq!(w.agreed_version, PROTOCOL_VERSION),
        other => panic!("expected a welcome, got {other:?}"),
    }

    // 2. The client asks for the world, then attaches the pane it will render.
    wire.client_sends(&ClientFrame::request(
        RequestId::new("r-1"),
        Request::ListSessions {
            workspace_id: Some(session.workspace_id.clone()),
            include_archived: false,
        },
    ));
    wire.client_sends(&ClientFrame::request(
        RequestId::new("r-2"),
        Request::AttachPane {
            session_id: session.id.clone(),
            pane_id: pane_id.clone(),
            size: PtySize::new(40, 120),
            // What a renderer without its own terminal emulator asks for, and what
            // it would get by leaving the field out.
            stream: PaneStream::Cells,
        },
    ));
    // Pipelined: two requests before either answer. The ids are what correlate.
    let asked = wire.daemon_reads();
    assert_eq!(asked.len(), 2);
    assert_eq!(asked[0].request_id().unwrap().as_str(), "r-1");
    assert_eq!(asked[1].request_id().unwrap().as_str(), "r-2");

    let screen = Grid::from_lines(&["Allow rm -rf build? (y/n) "], 120);
    wire.daemon_sends(&ServerFrame::response(
        RequestId::new("r-1"),
        Response::Sessions {
            sessions: vec![SessionSummary::from_session(&session, 1, false, T0 + 5_000)],
        },
    ));
    wire.daemon_sends(&ServerFrame::response(
        RequestId::new("r-2"),
        Response::Attached {
            attachment: Box::new(PaneAttachment {
                session_id: session.id.clone(),
                pane_id: pane_id.clone(),
                node_id: Some(node_id.clone()),
                stream: PaneStream::Cells,
                screen: Some(Box::new(screen.clone())),
                replay: TerminalBytes::default(),
                size: PtySize::new(40, 120),
                scrollback_truncated: false,
                bytes_seen: 26,
                next_seq: 0,
            }),
        },
    ));

    let answers = wire.client_reads();
    assert_eq!(answers.len(), 2);

    match &answers[0].message {
        ServerMessage::Response { id, response } => {
            assert_eq!(id.as_str(), "r-1");
            let Response::Sessions { sessions } = response else {
                panic!("expected sessions, got {response:?}");
            };
            // The UI is told the state, the label and the badge. It derives nothing.
            assert_eq!(sessions[0].state_label, "YOUR TURN");
            assert!(sessions[0].needs_user);
            assert_eq!(sessions[0].badge_count, 1);
        }
        other => panic!("expected a response, got {other:?}"),
    }

    let for_the_renderer = match &answers[1].message {
        ServerMessage::Response {
            response: Response::Attached { attachment },
            ..
        } => attachment
            .screen
            .clone()
            .expect("a cells attachment carries the screen"),
        other => panic!("expected an attachment, got {other:?}"),
    };
    assert_eq!(
        *for_the_renderer, screen,
        "the screen must arrive cell for cell, or the pane is not what the daemon sees"
    );
    assert_eq!(
        for_the_renderer.row_text(0),
        "Allow rm -rf build? (y/n)",
        "and it reads as what the agent asked, for the accessibility tree"
    );

    // 3. The user answers the agent by typing. There is no "approve" request; this
    //    is the only path, and it is a human pressing a key.
    wire.client_sends(&ClientFrame::request(
        RequestId::new("r-3"),
        Request::WritePty {
            session_id: session.id.clone(),
            node_id: node_id.clone(),
            data: TerminalBytes::new(b"y\r".to_vec()),
        },
    ));
    match &wire.daemon_reads()[0].message {
        ClientMessage::Request {
            request: Request::WritePty { data, .. },
            ..
        } => assert_eq!(data.as_slice(), b"y\r"),
        other => panic!("expected a write, got {other:?}"),
    }
    wire.daemon_sends(&ServerFrame::response(RequestId::new("r-3"), Response::Ack));

    // 4. The daemon pushes what followed: the demand cleared, the turn resumed.
    wire.daemon_sends(&ServerFrame::event(ServerEvent::AttentionEffect {
        effect: Effect::Cleared {
            session_id: session.id.clone(),
        },
    }));
    wire.daemon_sends(&ServerFrame::event(ServerEvent::NodeStateChanged {
        session_id: session.id.clone(),
        node_id,
        lifecycle: Lifecycle::Alive,
        turn: Some(Turn::Active),
        display_state: turn_core::state::DisplayState::Running,
        caused_by: None,
    }));

    let pushes = wire.client_reads();
    assert_eq!(pushes.len(), 3, "the ack plus two pushes");
    assert!(
        pushes.iter().all(|f| !f.is_terminal()),
        "nothing here ends the connection"
    );
    let events: Vec<&ServerEvent> = pushes
        .iter()
        .filter_map(|f| match &f.message {
            ServerMessage::Event { event } => Some(event),
            _ => None,
        })
        .collect();
    assert_eq!(events.len(), 2);
    assert!(events.iter().all(|e| e.session_id() == Some(&session.id)));
}

/// The feature the whole architecture exists for: the UI dies, comes back, and
/// finds its processes still running with a screen it can rebuild exactly.
#[test]
fn a_ui_restart_rebuilds_its_terminals_without_touching_the_processes() {
    let mut wire = Wire::new();
    let (session, node_id) = session_with_blocked_agent();
    let pane_id = session.layout.panes()[0].id.clone();

    // The UI is gone. It comes back and re-handshakes from scratch.
    wire.client_sends(&ClientFrame::hello(Hello::new("turn-ui", "0.1.0")));
    let agreed = wire.daemon_reads()[0].negotiate().unwrap();
    // A different pid would mean the daemon restarted too and nothing survived;
    // the same pid is how the UI knows its processes are still there.
    wire.daemon_sends(&ServerFrame::welcome(Welcome::new(
        agreed, "0.1.0", 4242, T0,
    )));
    let welcome = match &wire.client_reads()[0].message {
        ServerMessage::Welcome(w) => w.clone(),
        other => panic!("expected a welcome, got {other:?}"),
    };
    assert_eq!(welcome.daemon_pid, 4242);
    assert_eq!(welcome.daemon_started_ms, T0);

    // Re-attaching returns the current screen, at the new window's geometry.
    wire.client_sends(&ClientFrame::request(
        RequestId::new("r-1"),
        Request::AttachPane {
            session_id: session.id.clone(),
            pane_id: pane_id.clone(),
            size: PtySize::new(24, 80),
            stream: PaneStream::Cells,
        },
    ));
    let _ = wire.daemon_reads();
    wire.daemon_sends(&ServerFrame::response(
        RequestId::new("r-1"),
        Response::Attached {
            attachment: Box::new(PaneAttachment {
                session_id: session.id.clone(),
                pane_id,
                node_id: Some(node_id),
                stream: PaneStream::Cells,
                screen: Some(Box::new(Grid::from_lines(&["still waiting"], 80))),
                replay: TerminalBytes::default(),
                size: PtySize::new(24, 80),
                // A long-running build overflowed the daemon's ring, and the
                // protocol says so rather than letting the user scroll into a lie.
                scrollback_truncated: true,
                bytes_seen: 12_000_000,
                next_seq: 41_000,
            }),
        },
    ));
    match &wire.client_reads()[0].message {
        ServerMessage::Response {
            response: Response::Attached { attachment },
            ..
        } => {
            assert!(attachment.scrollback_truncated);
            assert_eq!(attachment.bytes_seen, 12_000_000);
            assert_eq!(attachment.size, PtySize::new(24, 80));
            assert_eq!(
                attachment
                    .screen
                    .as_ref()
                    .map(|screen| screen.row_text(0))
                    .as_deref(),
                Some("still waiting"),
                "the pane comes back showing what it showed before the UI died"
            );
        }
        other => panic!("expected an attachment, got {other:?}"),
    }
}

/// A whole cells conversation, the way a renderer would drive it: attach, apply the
/// diffs, miss one, ask for the screen again, carry on. Only the public API, and
/// everything through the real framing.
///
/// The assertion that matters is the last one: after the resync the client's screen is
/// exactly the daemon's, which is the property the sequence number and the whole-screen
/// answer exist to guarantee.
#[test]
fn a_client_rendering_cells_stays_in_step_with_the_daemon_across_a_missed_update() {
    let mut wire = Wire::new();
    let (session, node_id) = session_with_blocked_agent();
    let pane_id = session.layout.panes()[0].id.clone();

    // What the daemon holds. A client never sees this; it sees what is sent about it.
    let mut daemon_screen = Grid::from_lines(&["$ claude", "Allow rm -rf build? (y/n)"], 40);

    wire.client_sends(&ClientFrame::request(
        RequestId::new("r-1"),
        Request::AttachPane {
            session_id: session.id.clone(),
            pane_id: pane_id.clone(),
            size: PtySize::new(2, 40),
            // Absent on the wire would mean the same thing; named here for clarity.
            stream: PaneStream::Cells,
        },
    ));
    assert_eq!(wire.daemon_reads().len(), 1);
    wire.daemon_sends(&ServerFrame::response(
        RequestId::new("r-1"),
        Response::Attached {
            attachment: Box::new(PaneAttachment {
                session_id: session.id.clone(),
                pane_id: pane_id.clone(),
                node_id: Some(node_id.clone()),
                stream: PaneStream::Cells,
                screen: Some(Box::new(daemon_screen.clone())),
                replay: TerminalBytes::default(),
                size: PtySize::new(2, 40),
                scrollback_truncated: false,
                bytes_seen: 34,
                next_seq: 0,
            }),
        },
    ));

    let mut client_screen = match &wire.client_reads()[0].message {
        ServerMessage::Response {
            response: Response::Attached { attachment },
            ..
        } => *attachment
            .screen
            .clone()
            .expect("a cells attachment carries the screen"),
        other => panic!("expected an attachment, got {other:?}"),
    };
    assert_eq!(client_screen, daemon_screen);

    // The user types `y`. The daemon's screen changes on one row, and that is what is
    // sent: a diff, with the sequence number the client checks.
    let mut next = daemon_screen.clone();
    if let Some(cell) = next.cell_mut(1, 25) {
        cell.text = "y".into();
    }
    next.cursor = Some((1, 26));
    let update = ScreenUpdate::between(&daemon_screen, &next);
    assert!(!update.is_full(), "one row changed: {update:?}");
    daemon_screen = next;
    wire.daemon_sends(&ServerFrame::event(ServerEvent::PaneScreen {
        session_id: session.id.clone(),
        pane_id: pane_id.clone(),
        node_id: Some(node_id.clone()),
        seq: 0,
        update,
    }));

    for frame in wire.client_reads() {
        if let ServerMessage::Event {
            event: ServerEvent::PaneScreen { seq, update, .. },
        } = frame.message
        {
            assert_eq!(seq, 0, "the first update starts the sequence");
            update
                .apply(&mut client_screen)
                .expect("an update from the daemon must apply");
        }
    }
    assert_eq!(client_screen, daemon_screen);

    // The next update is lost — the daemon's channel is bounded on purpose — and the
    // one after it arrives with a sequence number that has jumped.
    let mut after = daemon_screen.clone();
    write_line(&mut after, 0, "running the command");
    write_line(&mut after, 1, "$ ");
    daemon_screen = after;
    wire.daemon_sends(&ServerFrame::event(ServerEvent::PaneScreen {
        session_id: session.id.clone(),
        pane_id: pane_id.clone(),
        node_id: Some(node_id.clone()),
        seq: 2,
        update: ScreenUpdate::between(&client_screen, &daemon_screen),
    }));

    let jumped = match &wire.client_reads()[0].message {
        ServerMessage::Event {
            event: ServerEvent::PaneScreen { seq, .. },
        } => *seq,
        other => panic!("expected a screen update, got {other:?}"),
    };
    assert_eq!(jumped, 2, "one update went missing");
    // The client must not apply it: rows on top of a stale screen would leave the two
    // disagreeing with nothing to detect it. It asks for the whole screen instead.
    wire.client_sends(&ClientFrame::request(
        RequestId::new("r-2"),
        Request::ResyncPane {
            session_id: session.id.clone(),
            pane_id: pane_id.clone(),
        },
    ));
    assert_eq!(wire.daemon_reads().len(), 1);
    wire.daemon_sends(&ServerFrame::response(
        RequestId::new("r-2"),
        Response::Screen {
            session_id: session.id.clone(),
            pane_id,
            node_id: Some(node_id),
            next_seq: 3,
            grid: Box::new(daemon_screen.clone()),
        },
    ));

    let (recovered, next_seq) = match &wire.client_reads()[0].message {
        ServerMessage::Response {
            response: Response::Screen { grid, next_seq, .. },
            ..
        } => ((**grid).clone(), *next_seq),
        other => panic!("expected a screen, got {other:?}"),
    };
    assert_eq!(next_seq, 3, "the client knows where the stream resumes");
    assert_eq!(
        recovered, daemon_screen,
        "after a resync the two screens must be identical"
    );
    assert!(recovered.text().contains("running the command"));
}

/// Overwrites one row of a grid with text, blanking the rest of it.
fn write_line(grid: &mut Grid, row: u16, text: &str) {
    for col in 0..grid.cols {
        if let Some(cell) = grid.cell_mut(row, col) {
            *cell = turn_proto::Cell::blank();
        }
    }
    for (col, ch) in text.chars().enumerate().take(grid.cols as usize) {
        if let Some(cell) = grid.cell_mut(row, col as u16) {
            cell.text = ch.to_string();
        }
    }
}

/// A partial restore reports and offers. Nothing has been started.
#[test]
fn a_partial_restore_offers_a_relaunch_that_only_the_user_can_accept() {
    let mut wire = Wire::new();
    let session_id = SessionId::from_stored("sess_restore1");
    let lost_pane = PaneId::from_stored("pane_watch001");

    wire.daemon_sends(&ServerFrame::event(ServerEvent::RestoreResult {
        session_id: session_id.clone(),
        state: RestoreState::PartiallyRestored,
        needs_explanation: true,
        panes: vec![
            PaneRestoreOutcome {
                pane_id: PaneId::from_stored("pane_agent001"),
                node_id: NodeId::from_stored("proc_agent001"),
                lifecycle: Lifecycle::Orphaned,
                can_relaunch: false,
                command: None,
            },
            PaneRestoreOutcome {
                pane_id: lost_pane.clone(),
                node_id: NodeId::from_stored("proc_watch001"),
                lifecycle: Lifecycle::Lost,
                can_relaunch: true,
                command: Some("cargo watch -x test".into()),
            },
        ],
    }));

    let offer = match &wire.client_reads()[0].message {
        ServerMessage::Event {
            event:
                ServerEvent::RestoreResult {
                    state,
                    needs_explanation,
                    panes,
                    ..
                },
        } => {
            assert_eq!(*state, RestoreState::PartiallyRestored);
            assert!(*needs_explanation, "the user must be told");
            panes
                .iter()
                .find(|p| p.pane_id == lost_pane)
                .expect("the lost pane is reported")
                .clone()
        }
        other => panic!("expected a restore result, got {other:?}"),
    };
    assert!(offer.can_relaunch);
    assert_eq!(offer.node_id, NodeId::from_stored("proc_watch001"));
    assert_eq!(offer.command.as_deref(), Some("cargo watch -x test"));

    // Only if the user accepts does anything start, and it is an explicit request.
    wire.client_sends(&ClientFrame::request(
        RequestId::new("r-1"),
        Request::RelaunchNode {
            session_id,
            node_id: offer.node_id,
            resume: false,
        },
    ));
    let frames = wire.daemon_reads();
    assert!(matches!(
        &frames[0].message,
        ClientMessage::Request {
            request: Request::RelaunchNode { .. },
            ..
        }
    ));
}

/// A heuristic's opinion travels as an opinion, all the way to the renderer, and
/// the effect it produces is a badge rather than a jump.
#[test]
fn a_guessed_state_reaches_the_client_as_a_guess_and_never_as_a_focus_jump() {
    let mut wire = Wire::new();
    let session_id = SessionId::from_stored("sess_guess001");

    let guessed = TurnEvent::new(
        session_id.clone(),
        EventKind::AgentPermissionRequired {
            summary: "looks like a y/n prompt".into(),
            command: None,
            tool_name: None,
            risk: Risk::Medium,
        },
        EventSource::PtyHeuristic {
            rule: "permission_box".into(),
        },
        // The adapter asks for the world; the source caps what it may claim.
        Confidence::Explicit,
        T0,
    );
    assert!(guessed.confidence.is_provisional(), "capped at the source");

    // The manager's own policy degrades the focus action for a provisional event.
    let actions = AttentionPolicy::default().resolve(
        turn_core::attention::Trigger::PermissionRequired,
        guessed.confidence,
    );
    assert!(!actions.iter().any(|a| a.is_focus()));

    wire.daemon_sends(&ServerFrame::event(ServerEvent::TurnEventEmitted {
        turn_event: guessed,
    }));
    wire.daemon_sends(&ServerFrame::event(ServerEvent::AttentionEffect {
        effect: Effect::Badge {
            session_id,
            count: 1,
        },
    }));

    let pushes = wire.client_reads();
    match &pushes[0].message {
        ServerMessage::Event {
            event: ServerEvent::TurnEventEmitted { turn_event },
        } => {
            assert_eq!(turn_event.confidence, Confidence::InferredHigh);
            assert!(!turn_event.confidence.may_steal_focus());
        }
        other => panic!("expected the event, got {other:?}"),
    }
    for frame in &pushes {
        if let ServerMessage::Event {
            event: ServerEvent::AttentionEffect { effect },
        } = &frame.message
        {
            assert!(
                !matches!(effect, Effect::Focus { .. }),
                "a heuristic must never move the user: {effect:?}"
            );
        }
    }
}

/// A subagent appearing arrives as a confirmed edge, because the tool said so.
#[test]
fn a_subagent_appearing_pushes_a_tree_the_client_can_draw_without_guessing() {
    let mut wire = Wire::new();
    let session_id = SessionId::from_stored("sess_subs0001");

    let mut tree = SessionTree::new();
    let root = tree.insert(ProcessNode::agent(
        session_id.clone(),
        "claude",
        "/repo",
        T0,
    ));
    let mut sub = ProcessNode::agent(session_id.clone(), "explore", "/repo", T0);
    sub.kind = turn_core::model::NodeKind::Subagent;
    sub.lifecycle = Lifecycle::Alive;
    sub.link_to(root.clone(), Relation::Confirmed);
    tree.insert(sub);
    // And something we only spotted in the process table.
    let mut spotted = ProcessNode::process(
        session_id.clone(),
        turn_core::model::NodeKind::Build,
        "cc",
        "/",
        T0,
    );
    spotted.lifecycle = Lifecycle::Alive;
    spotted.link_to(root, Relation::Inferred);
    tree.insert(spotted);

    wire.daemon_sends(&ServerFrame::event(ServerEvent::TreeChanged {
        session_id,
        nodes: TreeNodeView::flatten(&tree, T0 + 1_000),
    }));

    let nodes = match &wire.client_reads()[0].message {
        ServerMessage::Event {
            event: ServerEvent::TreeChanged { nodes, .. },
        } => nodes.clone(),
        other => panic!("expected a tree change, got {other:?}"),
    };
    assert_eq!(nodes.len(), 3);
    assert_eq!(nodes[0].depth, 0);
    let confirmed = nodes.iter().find(|n| n.command == "explore").unwrap();
    let inferred = nodes.iter().find(|n| n.command == "cc").unwrap();
    assert!(
        !confirmed.relationship_is_provisional,
        "the tool reported it"
    );
    assert!(
        inferred.relationship_is_provisional,
        "a process-table match is a guess and must be drawn as one"
    );
}

/// A build's worth of output, chunked, reassembled, with a gap admitted in the
/// middle.
#[test]
fn a_firehose_of_output_reassembles_in_order_and_admits_what_was_dropped() {
    let mut wire = Wire::new();
    let session_id = SessionId::from_stored("sess_noisy001");
    let pane_id = PaneId::from_stored("pane_build001");

    // 700 KB of build noise, which is more than one frame may carry.
    let noisy: Vec<u8> = (0..700_000u32)
        .map(|i| b"compiling turn-proto\r\n"[(i % 22) as usize])
        .collect();
    let chunks = TerminalBytes::new(noisy.clone()).chunks(MAX_OUTPUT_CHUNK_BYTES);
    assert!(
        chunks.len() > 1,
        "the point is that it did not fit in one frame"
    );

    let mut seq = 0u64;
    for chunk in &chunks {
        wire.daemon_sends(&ServerFrame::event(ServerEvent::PaneOutput {
            session_id: session_id.clone(),
            pane_id: pane_id.clone(),
            node_id: None,
            seq,
            data: chunk.clone(),
        }));
        seq += 1;
    }
    // The client fell behind and the daemon's bounded channel dropped frames. It
    // says so rather than leaving a hole the UI would never notice.
    wire.daemon_sends(&ServerFrame::event(ServerEvent::PaneOutputGap {
        session_id,
        pane_id,
        dropped: 12,
        resume_seq: seq + 12,
    }));

    let mut reassembled: Vec<u8> = Vec::new();
    let mut expected_seq = 0u64;
    let mut gap_reported = false;
    for frame in wire.client_reads() {
        match frame.message {
            ServerMessage::Event {
                event: ServerEvent::PaneOutput { seq, data, .. },
            } => {
                assert_eq!(seq, expected_seq, "output must arrive in order");
                expected_seq += 1;
                reassembled.extend(data.as_slice());
            }
            ServerMessage::Event {
                event:
                    ServerEvent::PaneOutputGap {
                        dropped,
                        resume_seq,
                        ..
                    },
            } => {
                assert_eq!(dropped, 12);
                assert_eq!(resume_seq, expected_seq + 12);
                gap_reported = true;
            }
            other => panic!("unexpected frame: {other:?}"),
        }
    }
    assert_eq!(reassembled, noisy, "not a byte may be reordered or lost");
    assert!(gap_reported);
}

/// A peer writing rubbish must cost one line, not the connection. This is the
/// difference between a multiplexer that survives a buggy client and one that
/// takes thirty running agents down with it.
#[test]
fn a_hostile_stream_of_rubbish_never_costs_more_than_the_bad_lines() {
    let good_first = encode(&ClientFrame::request(
        RequestId::new("r-1"),
        Request::NextAttention,
    ))
    .unwrap();
    let good_last = encode(&ClientFrame::request(
        RequestId::new("r-2"),
        Request::ListTemplates,
    ))
    .unwrap();

    let mut stream = Vec::new();
    stream.extend(&good_first);
    stream.extend(b"{\"v\":1,\"type\":\"request\"\n"); // truncated JSON
    stream.extend(b"[]\n"); // valid JSON, wrong shape
    stream.extend(b"\n\n"); // blank padding
    stream.extend(&[0xff, 0xfe, b'\n']); // not UTF-8
    stream.extend(b"{\"v\":1,\"type\":\"unheard_of\"}\n"); // unknown message type
    stream.extend(vec![b'A'; 200]); // an over-length line, with no newline yet
    stream.extend(b"\n");
    stream.extend(&good_last);

    let mut decoder = LineDecoder::with_limit(128);
    decoder.feed(&stream);

    let mut accepted = Vec::new();
    let mut refusals = Vec::new();
    while let Some(result) = decoder.next_message::<ClientFrame>() {
        match result {
            Ok(frame) => accepted.push(frame),
            Err(error) => refusals.push(error.to_proto_error()),
        }
    }

    assert_eq!(
        accepted.len(),
        2,
        "both good frames must get through: {refusals:?}"
    );
    assert_eq!(accepted[0].request_id().unwrap().as_str(), "r-1");
    assert_eq!(accepted[1].request_id().unwrap().as_str(), "r-2");
    assert_eq!(refusals.len(), 5, "one refusal per bad line: {refusals:?}");
    assert!(refusals
        .iter()
        .any(|e| e.code == ErrorCode::MalformedMessage));
    assert!(refusals.iter().any(|e| e.code == ErrorCode::LineTooLong));
    // And every refusal is something the daemon can send back verbatim.
    for error in &refusals {
        assert!(!error.message.is_empty());
        assert!(!error.code.is_fatal_to_connection());
    }
    assert_eq!(decoder.buffered(), 0);
}

/// The mismatch path, from the client's side: it sees a `rejected` frame with a
/// message it can put on screen, and nothing else.
#[test]
fn a_stale_client_is_told_which_side_is_old_and_the_connection_ends() {
    let mut wire = Wire::new();

    // A daemon from the future: it has moved on to 3..=4.
    let mut stale = ClientFrame::hello(Hello::new("turn-ui", "0.0.9"));
    stale.v = 2;
    wire.client_sends(&stale);

    let arrived = wire.daemon_reads();
    let refusal: ProtoError =
        negotiate_within(arrived[0].v, 3, 4).expect_err("2 is below the daemon's window");
    wire.daemon_sends(&ServerFrame::rejected(refusal));

    let frames = wire.client_reads();
    assert_eq!(frames.len(), 1, "nothing follows a rejection");
    assert!(frames[0].is_terminal());
    match &frames[0].message {
        ServerMessage::Rejected { error } => {
            assert_eq!(error.code, ErrorCode::UnsupportedVersion);
            assert!(error.code.is_fatal_to_connection());
            assert!(!error.code.is_retryable(), "retrying cannot help");
            // The message names both versions and what to do about it.
            assert!(error.message.contains('2') && error.message.contains('3'));
            assert!(error.message.to_lowercase().contains("quit"));
        }
        other => panic!("expected a rejection, got {other:?}"),
    }
}

/// The focus governor's decisions cross the boundary intact. A UI that treated a
/// deferral as a jump would undo the one guard the product cannot lose.
#[test]
fn the_governors_verdicts_stay_distinguishable_across_the_boundary() {
    let mut wire = Wire::new();
    let session_id = SessionId::from_stored("sess_focus001");

    wire.client_sends(&ClientFrame::request(
        RequestId::new("r-1"),
        Request::UpdateUserActivity {
            context: UserContext {
                last_keystroke_ms: Some(T0),
                app_foreground: true,
                active_session: Some(SessionId::from_stored("sess_other001")),
                sensitive_operation: false,
            },
        },
    ));
    let _ = wire.daemon_reads();

    wire.daemon_sends(&ServerFrame::response(
        RequestId::new("r-1"),
        Response::Effects {
            effects: vec![
                Effect::Badge {
                    session_id: session_id.clone(),
                    count: 1,
                },
                Effect::FocusDeferred {
                    session_id: session_id.clone(),
                    until_ms: T0 + 1_500,
                    reason: turn_core::attention::DeferReason::UserTyping,
                },
                Effect::FocusDenied {
                    session_id,
                    reason: turn_core::attention::FocusDenial::RateLimited,
                },
            ],
        },
    ));

    match &wire.client_reads()[0].message {
        ServerMessage::Response {
            response: Response::Effects { effects },
            ..
        } => {
            assert_eq!(effects.len(), 3);
            assert!(
                !effects.iter().any(|e| matches!(e, Effect::Focus { .. })),
                "a deferral and a denial must not read as a jump: {effects:?}"
            );
            assert!(effects
                .iter()
                .any(|e| matches!(e, Effect::FocusDeferred { reason, .. }
                    if *reason == turn_core::attention::DeferReason::UserTyping)));
            assert!(effects
                .iter()
                .any(|e| matches!(e, Effect::FocusDenied { reason, .. }
                    if *reason == turn_core::attention::FocusDenial::RateLimited)));
        }
        other => panic!("expected effects, got {other:?}"),
    }
}

/// Every request the client can make is answered by the result it promised. The
/// client-side half of the contract test inside the crate: here it is checked
/// against a hand-built pairing, using only the public API.
#[test]
fn a_client_can_predict_the_response_shape_before_it_arrives() {
    let session_id = SessionId::from_stored("sess_pred0001");
    let pairs: Vec<(Request, &str)> = vec![
        (
            Request::ListWorkspaces {
                include_archived: false,
            },
            "workspaces",
        ),
        (
            Request::GetSession {
                session_id: session_id.clone(),
            },
            "session_details",
        ),
        (
            Request::GetProcessTree {
                session_id: session_id.clone(),
            },
            "tree",
        ),
        (
            Request::SplitPane {
                session_id: session_id.clone(),
                pane_id: PaneId::from_stored("pane_a"),
                direction: turn_core::model::Direction::Vertical,
                pane: NewPane::new(PaneKind::Shell).with_command("zsh"),
            },
            "layout",
        ),
        (
            Request::AttachPane {
                session_id: session_id.clone(),
                pane_id: PaneId::from_stored("pane_a"),
                size: PtySize::default(),
                stream: PaneStream::Cells,
            },
            "attached",
        ),
        (
            Request::ResyncPane {
                session_id: session_id.clone(),
                pane_id: PaneId::from_stored("pane_a"),
            },
            "screen",
        ),
        (
            Request::WritePty {
                session_id: session_id.clone(),
                node_id: NodeId::from_stored("proc_a"),
                data: TerminalBytes::new(b"x".to_vec()),
            },
            "ack",
        ),
        (Request::NextAttention, "attention"),
        (Request::GotoAttention { attention_id: None }, "effects"),
        (
            Request::RelaunchNode {
                session_id,
                node_id: NodeId::from_stored("proc_a"),
                resume: true,
            },
            "node",
        ),
    ];

    for (request, expected) in pairs {
        assert_eq!(
            request.expected_result(),
            expected,
            "{} must answer with `{expected}`",
            request.op()
        );
        assert!(
            Response::RESULT_NAMES.contains(&expected),
            "`{expected}` is not a response this protocol defines"
        );
    }
}
