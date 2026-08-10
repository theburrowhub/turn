//! Handshake and envelope tests, including the version-mismatch paths that are
//! the whole reason the version rides on every frame.

use super::*;
use crate::framing::{encode, LineDecoder};
use crate::response::Response;

fn hello_frame() -> ClientFrame {
    ClientFrame::hello(Hello::new("turn-ui", "0.1.0", AuthToken::new("test-token")))
}

#[test]
fn auth_tokens_cross_the_wire_but_debug_output_is_redacted() {
    let token = AuthToken::new("secret-capability");
    let hello = Hello::new("turn-ui", "0.1.0", token.clone());
    let json = serde_json::to_string(&hello).unwrap();
    assert!(
        json.contains("secret-capability"),
        "the peer needs the secret"
    );
    assert!(!format!("{hello:?}").contains("secret-capability"));
    assert_eq!(
        serde_json::from_str::<Hello>(&json)
            .unwrap()
            .auth_token
            .unwrap()
            .expose_secret(),
        token.expose_secret()
    );
}

#[test]
fn the_token_path_is_derived_from_the_exact_socket_path() {
    assert_eq!(
        ipc_auth_token_path(Path::new("/run/turn/alternate.sock")),
        PathBuf::from("/run/turn/alternate.sock.token")
    );
}

#[test]
fn the_shared_token_reader_enforces_the_published_file_contract() {
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("turnd.sock");
    let path = ipc_auth_token_path(&socket);
    std::fs::write(&path, "a".repeat(IPC_AUTH_TOKEN_HEX_BYTES)).unwrap();
    assert_eq!(
        read_ipc_auth_token(&socket).unwrap().expose_secret(),
        "a".repeat(IPC_AUTH_TOKEN_HEX_BYTES)
    );

    std::fs::write(&path, "a".repeat(IPC_AUTH_TOKEN_HEX_BYTES + 1)).unwrap();
    assert_eq!(
        read_ipc_auth_token(&socket).unwrap_err().kind(),
        std::io::ErrorKind::InvalidData
    );
}

#[cfg(unix)]
#[test]
fn the_shared_token_reader_never_follows_a_sidecar_symlink() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("turnd.sock");
    let target = temp.path().join("elsewhere");
    std::fs::write(&target, "a".repeat(IPC_AUTH_TOKEN_HEX_BYTES)).unwrap();
    symlink(&target, ipc_auth_token_path(&socket)).unwrap();
    assert!(read_ipc_auth_token(&socket).is_err());
}

/// The limits a client has to respect are announced rather than assumed, including
/// the largest screen `attach_pane` will accept.
#[test]
fn the_welcome_announces_the_limits_a_client_can_actually_hit() {
    let welcome = Welcome::new(PROTOCOL_VERSION, "0.1.0", 1, 0);
    assert_eq!(welcome.limits.max_screen_cells, crate::MAX_SCREEN_CELLS);

    // A daemon that predates the field is read as meaning the same number rather
    // than as meaning "no screens allowed".
    let older: Limits =
        serde_json::from_str("{\"max_line_bytes\":8388608,\"max_output_chunk_bytes\":262144}")
            .expect("an absent limit is defaulted");
    assert_eq!(older.max_screen_cells, crate::MAX_SCREEN_CELLS);
    assert_eq!(
        older.max_image_pixels,
        crate::MAX_IMAGE_PIXELS,
        "a daemon that predates the image limits must read as meaning them, not as zero"
    );
    assert_eq!(older.max_placed_images, crate::MAX_PLACED_IMAGES);
}

#[test]
fn a_handshake_completes_and_agrees_on_this_builds_version() {
    let frame = hello_frame();
    let agreed = frame.negotiate().expect("the current client is accepted");
    assert_eq!(agreed, PROTOCOL_VERSION);

    let welcome = ServerFrame::welcome(Welcome::new(agreed, "0.1.0", 4242, 1_700_000_000_000));
    assert_eq!(welcome.v, PROTOCOL_VERSION);
    assert!(!welcome.is_terminal());
    match welcome.message {
        ServerMessage::Welcome(w) => {
            assert_eq!(w.agreed_version, PROTOCOL_VERSION);
            assert_eq!(w.limits.max_line_bytes, MAX_LINE_BYTES);
            assert_eq!(w.output_encoding, OutputEncoding::Base64);
        }
        other => panic!("expected a welcome, got {other:?}"),
    }
}

/// The requirement this whole module exists for: a stale UI is refused, with a
/// message that tells the user which side is old.
#[test]
fn a_stale_client_is_refused_with_a_message_naming_both_sides() {
    // A daemon that has moved on: it accepts 3..=4, the client speaks 2.
    let error = negotiate_within(2, 3, 4).expect_err("2 is below the window");
    assert_eq!(error.code, ErrorCode::UnsupportedVersion);
    assert!(error.message.contains("too old"), "got {}", error.message);
    assert!(error.message.contains('2') && error.message.contains('3'));
    assert!(
        error.message.to_lowercase().contains("quit"),
        "the user must be told what to do: {}",
        error.message
    );
    assert_eq!(
        error.detail.as_deref(),
        Some("client=2 supported=3..=4"),
        "the exact versions belong in the log"
    );
    assert!(error.code.is_fatal_to_connection());
    assert!(!error.code.is_retryable());
}

#[test]
fn a_client_newer_than_the_daemon_is_refused_the_other_way_round() {
    let error = negotiate_within(9, 1, 4).expect_err("9 is above the window");
    assert_eq!(error.code, ErrorCode::UnsupportedVersion);
    assert!(
        error.message.contains("daemon is older"),
        "the blame must point at the daemon: {}",
        error.message
    );
}

#[test]
fn any_version_inside_the_window_is_accepted_and_the_clients_dialect_is_used() {
    for client in 3..=5 {
        assert_eq!(
            negotiate_within(client, 3, 5).unwrap(),
            client,
            "a rollout window means speaking the older dialect on purpose"
        );
    }
    // The edges are inclusive.
    assert!(negotiate_within(2, 3, 5).is_err());
    assert!(negotiate_within(6, 3, 5).is_err());
}

#[test]
fn version_zero_is_refused_rather_than_treated_as_unset() {
    let error = negotiate(0).expect_err("0 is not a version");
    assert_eq!(error.code, ErrorCode::UnsupportedVersion);
}

#[test]
fn a_refused_handshake_is_a_terminal_frame_carrying_the_reason() {
    let error = negotiate_within(0, 1, 1).unwrap_err();
    let frame = ServerFrame::rejected(error.clone());
    assert!(frame.is_terminal());
    assert!(frame.request_id().is_none());

    let json = serde_json::to_string(&frame).unwrap();
    assert!(json.contains("\"type\":\"rejected\""), "got {json}");
    assert!(json.contains("\"unsupported_version\""));
    assert_eq!(serde_json::from_str::<ServerFrame>(&json).unwrap(), frame);
}

/// A peer that reconnects with different code without re-handshaking is caught
/// rather than tolerated.
#[test]
fn a_frame_marked_with_the_wrong_version_after_the_handshake_is_rejected() {
    let mut frame = ClientFrame::request(RequestId::new("r-1"), Request::NextAttention);
    assert!(frame.expect_version(PROTOCOL_VERSION).is_ok());

    frame.v = PROTOCOL_VERSION + 7;
    let error = frame.expect_version(PROTOCOL_VERSION).unwrap_err();
    assert_eq!(error.code, ErrorCode::UnsupportedVersion);
    assert!(error.message.contains("Reconnect"), "got {}", error.message);
}

#[test]
fn every_frame_carries_its_version_alongside_the_message_body() {
    let frame = hello_frame();
    let json = serde_json::to_string(&frame).unwrap();
    assert!(
        json.starts_with(&format!("{{\"v\":{PROTOCOL_VERSION},")),
        "got {json}"
    );
    assert!(json.contains("\"type\":\"hello\""));
    assert!(json.contains("\"client\":\"turn-ui\""));
    assert_eq!(serde_json::from_str::<ClientFrame>(&json).unwrap(), frame);
}

#[test]
fn a_request_is_correlated_by_the_id_the_client_chose() {
    let id = RequestId::new("req-42");
    let frame = ClientFrame::request(id.clone(), Request::ListTemplates);
    assert_eq!(frame.request_id(), Some(&id));

    let answer = ServerFrame::response(
        id.clone(),
        Response::Templates {
            templates: Vec::new(),
        },
    );
    assert_eq!(answer.request_id(), Some(&id));

    let json = serde_json::to_string(&answer).unwrap();
    assert!(json.contains("\"id\":\"req-42\""), "got {json}");
    assert_eq!(serde_json::from_str::<ServerFrame>(&json).unwrap(), answer);
}

#[test]
fn a_failure_with_no_request_behind_it_omits_the_id() {
    let frame = ServerFrame::error(
        None,
        ProtoError::new(
            ErrorCode::MalformedMessage,
            "A message could not be understood",
        ),
    );
    assert!(frame.request_id().is_none());
    let json = serde_json::to_string(&frame).unwrap();
    assert!(!json.contains("\"id\""), "got {json}");
    assert_eq!(serde_json::from_str::<ServerFrame>(&json).unwrap(), frame);
}

#[test]
fn a_push_carries_no_request_id_because_nothing_asked_for_it() {
    let frame = ServerFrame::event(ServerEvent::AttentionQueueChanged {
        entries: Vec::new(),
    });
    assert!(frame.request_id().is_none());
    let json = serde_json::to_string(&frame).unwrap();
    assert!(json.contains("\"type\":\"event\""), "got {json}");
    assert_eq!(serde_json::from_str::<ServerFrame>(&json).unwrap(), frame);
}

/// End to end over the real framing: handshake, request, response, push.
#[test]
fn a_whole_conversation_survives_the_wire() {
    let mut stream = Vec::new();
    stream.extend(encode(&hello_frame()).unwrap());
    stream.extend(
        encode(&ClientFrame::request(
            RequestId::new("r-1"),
            Request::NextAttention,
        ))
        .unwrap(),
    );

    let mut decoder = LineDecoder::new();
    decoder.feed(&stream);

    let first: ClientFrame = decoder.next_message().unwrap().unwrap();
    assert_eq!(first.negotiate().unwrap(), PROTOCOL_VERSION);
    assert!(matches!(first.message, ClientMessage::Hello(_)));

    let second: ClientFrame = decoder.next_message().unwrap().unwrap();
    assert_eq!(second.request_id().unwrap().as_str(), "r-1");
    assert!(decoder.next_message::<ClientFrame>().is_none());

    let mut back = Vec::new();
    back.extend(
        encode(&ServerFrame::welcome(Welcome::new(
            PROTOCOL_VERSION,
            "0.1.0",
            1,
            0,
        )))
        .unwrap(),
    );
    back.extend(
        encode(&ServerFrame::response(
            RequestId::new("r-1"),
            Response::Attention { entry: None },
        ))
        .unwrap(),
    );
    back.extend(
        encode(&ServerFrame::event(ServerEvent::SessionRemoved {
            session_id: turn_core::ids::SessionId::from_stored("sess_a"),
            workspace_id: turn_core::ids::WorkspaceId::from_stored("ws_a"),
        }))
        .unwrap(),
    );

    let mut decoder = LineDecoder::new();
    decoder.feed(&back);
    let frames: Vec<ServerFrame> = std::iter::from_fn(|| decoder.next_message())
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(frames.len(), 3);
    assert!(matches!(frames[0].message, ServerMessage::Welcome(_)));
    assert_eq!(frames[1].request_id().unwrap().as_str(), "r-1");
    assert!(matches!(frames[2].message, ServerMessage::Event { .. }));
}

/// A daemon must be able to read the version out of a frame it cannot
/// otherwise understand — that is what makes a version mismatch reportable
/// instead of merely malformed.
#[test]
fn the_version_is_readable_even_when_the_body_is_from_the_future() {
    let future = b"{\"v\":99,\"type\":\"request\",\"id\":\"r-1\",\
                   \"request\":{\"op\":\"invent_a_new_thing\"}}";

    // The typed parse fails, as it should.
    assert!(serde_json::from_slice::<ClientFrame>(future).is_err());

    // But the envelope's version is still recoverable, so the refusal can name it
    // instead of saying "malformed".
    assert_eq!(peek_version(future), Some(99));
    let error = version_refusal(future).expect("99 is outside this build's window");
    assert_eq!(error.code, ErrorCode::UnsupportedVersion);
    assert!(error.message.contains("99"), "got {}", error.message);
    assert!(error.code.is_fatal_to_connection());
}

/// The path the daemon actually takes: bytes off the socket, a parse that fails,
/// and a decision about which error the peer is sent.
#[test]
fn an_unparsable_frame_off_the_wire_is_refused_by_version_when_that_is_the_reason() {
    let mut stream = Vec::new();
    stream.extend_from_slice(
        b"{\"v\":99,\"type\":\"request\",\"id\":\"r-1\",\"request\":{\"op\":\"from_the_future\"}}\n",
    );
    stream.extend_from_slice(
        format!("{{\"v\":{PROTOCOL_VERSION},\"type\":\"request\",\"id\":\"r-2\",\"request\":{{\"op\":\"no_such_op\"}}}}\n")
            .as_bytes(),
    );

    let mut decoder = LineDecoder::new();
    decoder.feed(&stream);

    // A frame from a build this one does not know: the version is the answer.
    let line = decoder.next_line().unwrap().unwrap();
    let failure = serde_json::from_slice::<ClientFrame>(&line)
        .expect_err("this build cannot parse a future request");
    let refusal = version_refusal(&line).unwrap_or_else(|| {
        panic!("the version must be recoverable from {line:?}, not lost to {failure}")
    });
    assert_eq!(refusal.code, ErrorCode::UnsupportedVersion);

    // A frame at the agreed version whose body is nonsense: no version excuse.
    let line = decoder.next_line().unwrap().unwrap();
    assert!(serde_json::from_slice::<ClientFrame>(&line).is_err());
    assert_eq!(peek_version(&line), Some(PROTOCOL_VERSION));
    assert!(
        version_refusal(&line).is_none(),
        "an unknown op at the agreed version is malformed, not a version problem"
    );
}

/// Anything the version cannot be read out of is malformed and must be reported
/// as malformed: guessing a version would blame the wrong side.
#[test]
fn a_frame_with_no_readable_version_yields_none_rather_than_a_guess() {
    for hopeless in [
        &b"{this is not json"[..],
        &b"{\"type\":\"hello\",\"client\":\"turn-ui\"}"[..],
        &b"{\"v\":\"1\",\"type\":\"hello\"}"[..],
        &b"{\"v\":null,\"type\":\"hello\"}"[..],
        &b"{\"v\":1.5,\"type\":\"hello\"}"[..],
        &b"{\"v\":-1,\"type\":\"hello\"}"[..],
        &b"[1,2,3]"[..],
        &b""[..],
    ] {
        assert_eq!(
            peek_version(hopeless),
            None,
            "got a version out of {}",
            String::from_utf8_lossy(hopeless)
        );
        assert!(version_refusal(hopeless).is_none());
    }
}

/// The version is readable from a frame whose `v` is not the first field, because
/// nothing guarantees a peer's serialiser orders keys the way this one does.
#[test]
fn the_version_is_found_wherever_the_peer_put_it_in_the_object() {
    let trailing = b"{\"type\":\"request\",\"id\":\"r-1\",\"request\":{\"op\":\"future\"},\"v\":7}";
    assert_eq!(peek_version(trailing), Some(7));
}

/// Both directions: a daemon reads a client's frame, and a client reads a
/// daemon's.
#[test]
fn either_side_can_read_the_version_off_a_frame_the_other_side_sent() {
    let from_client = encode(&hello_frame()).unwrap();
    assert_eq!(peek_version(&from_client), Some(PROTOCOL_VERSION));
    let from_daemon = encode(&ServerFrame::welcome(Welcome::new(
        PROTOCOL_VERSION,
        "0.1.0",
        1,
        0,
    )))
    .unwrap();
    assert_eq!(peek_version(&from_daemon), Some(PROTOCOL_VERSION));
}

#[test]
fn an_unknown_operation_at_the_agreed_version_is_malformed_not_a_version_problem() {
    let line = format!(
        "{{\"v\":{PROTOCOL_VERSION},\"type\":\"request\",\"id\":\"r-1\",\
         \"request\":{{\"op\":\"no_such_op\"}}}}"
    );
    let mut decoder = LineDecoder::new();
    decoder.feed(line.as_bytes());
    decoder.feed(b"\n");
    let error = decoder.next_message::<ClientFrame>().unwrap().unwrap_err();
    assert_eq!(error.code(), ErrorCode::MalformedMessage);
}

#[test]
fn a_hello_omitting_its_optional_encoding_list_is_accepted() {
    let frame: ClientFrame = serde_json::from_str(
        "{\"v\":2,\"type\":\"hello\",\"client\":\"turn-cli\",\"client_version\":\"0.0.1\"}",
    )
    .unwrap();
    match frame.message {
        ClientMessage::Hello(hello) => {
            assert!(hello.accepts_encoding.is_empty(), "empty means base64");
        }
        other => panic!("expected a hello, got {other:?}"),
    }
}
