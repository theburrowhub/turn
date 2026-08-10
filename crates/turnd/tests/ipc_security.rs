//! Reproducible acceptance tests for the daemon control-socket boundary.

#![cfg(unix)]

mod common;

use common::TestDaemon;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use turn_proto::envelope::{Hello, ServerMessage};
use turn_proto::{AuthToken, ClientFrame, ErrorCode, Request, RequestId, ServerFrame};

const WAIT: Duration = Duration::from_secs(5);

async fn handshake(socket: &std::path::Path, hello: Hello) -> (UnixStream, ServerFrame) {
    let mut stream = UnixStream::connect(socket).await.unwrap();
    let bytes = turn_proto::encode(&ClientFrame::hello(hello)).unwrap();
    stream.write_all(&bytes).await.unwrap();
    stream.flush().await.unwrap();

    let mut line = String::new();
    let read = tokio::time::timeout(WAIT, BufReader::new(&mut stream).read_line(&mut line))
        .await
        .expect("the daemon must answer the handshake")
        .unwrap();
    assert!(read > 0, "the daemon closed without a handshake response");
    (stream, serde_json::from_str(&line).unwrap())
}

fn current_token(daemon: &TestDaemon) -> AuthToken {
    AuthToken::new(std::fs::read_to_string(daemon.handle().ipc_auth_token_path()).unwrap())
}

#[tokio::test]
async fn missing_and_invalid_capabilities_cannot_complete_the_handshake() {
    let daemon = TestDaemon::start_plain().await;

    for hello in [
        Hello::unauthenticated("missing-token", "0.1.0"),
        Hello::new("wrong-token", "0.1.0", AuthToken::new("0".repeat(64))),
    ] {
        let (_stream, answer) = handshake(daemon.socket(), hello).await;
        match answer.message {
            ServerMessage::Rejected { error } => assert_eq!(error.code, ErrorCode::Unauthorized),
            other => panic!("unauthenticated client was not rejected: {other:?}"),
        }
    }

    assert_eq!(daemon.handle().ipc_stats().rejected_auth, 2);
    daemon.shutdown().await;
}

#[tokio::test]
async fn two_valid_uis_can_use_the_daemon_at_the_same_time() {
    let daemon = TestDaemon::start_plain().await;
    let mut first = daemon.connect().await;
    let mut second = daemon.connect().await;

    let first_answer = first.ask(Request::ListTemplates).await;
    let second_answer = second.ask(Request::ListTemplates).await;
    assert_eq!(first_answer.result_name(), "templates");
    assert_eq!(second_answer.result_name(), "templates");
    assert_eq!(daemon.handle().ipc_stats().active_connections, 2);

    drop((first, second));
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_restart_rotates_and_revokes_the_previous_capability() {
    let daemon = TestDaemon::start_plain().await;
    let old = current_token(&daemon);
    let daemon = daemon.restart().await;
    let current = current_token(&daemon);
    assert_ne!(
        old, current,
        "every daemon generation needs a fresh capability"
    );

    let (_stream, replay) = handshake(daemon.socket(), Hello::new("replay", "0.1.0", old)).await;
    assert!(matches!(
        replay.message,
        ServerMessage::Rejected { error } if error.code == ErrorCode::Unauthorized
    ));

    let mut valid = daemon.connect().await;
    assert_eq!(
        valid.ask(Request::ListTemplates).await.result_name(),
        "templates"
    );
    drop(valid);
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_connection_storm_keeps_tasks_and_descriptors_bounded() {
    let daemon = TestDaemon::start_plain().await;
    let mut sockets = Vec::new();
    for _ in 0..(turnd::MAX_IPC_CONNECTIONS + 12) {
        sockets.push(UnixStream::connect(daemon.socket()).await.unwrap());
    }

    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let stats = daemon.handle().ipc_stats();
        if stats.rejected_capacity >= 12 {
            assert_eq!(stats.peak_connections, turnd::MAX_IPC_CONNECTIONS);
            assert!(stats.active_connections <= turnd::MAX_IPC_CONNECTIONS);
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "stats: {stats:?}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    drop(sockets);
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_request_flood_is_limited_before_it_can_reach_core_unbounded() {
    let daemon = TestDaemon::start_plain().await;
    let token = current_token(&daemon);
    let (mut stream, answer) =
        handshake(daemon.socket(), Hello::new("request-flood", "0.1.0", token)).await;
    assert!(matches!(answer.message, ServerMessage::Welcome(_)));

    let mut burst = Vec::new();
    for index in 0..(turnd::REQUEST_BURST * 8) {
        let frame = ClientFrame::request(
            RequestId::new(format!("flood-{index}")),
            Request::NextAttention,
        );
        turn_proto::encode_into(&frame, &mut burst).unwrap();
    }
    // The kernel may report a reset once the daemon closes a persistently abusive
    // connection; either outcome is fine. The observable contract is the counter.
    let _ = stream.write_all(&burst).await;

    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let stats = daemon.handle().ipc_stats();
        if stats.rate_limited_frames > 0 {
            assert!(stats.active_connections <= turnd::MAX_IPC_CONNECTIONS);
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "stats: {stats:?}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    drop(stream);
    daemon.shutdown().await;
}
