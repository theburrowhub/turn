#![cfg(unix)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[test]
fn network_forward_is_detached_while_original_stdio_and_exit_are_preserved() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (received_tx, received_rx) = mpsc::channel();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream);
        let mut length = 0_usize;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
            if let Some(value) = line.strip_prefix("Content-Length: ") {
                length = value.trim().parse().unwrap();
            }
        }
        let mut body = vec![0; length];
        reader.read_exact(&mut body).unwrap();
        received_tx.send(body).unwrap();
        // A synchronous fan-out would hold the status-line process here.
        std::thread::sleep(Duration::from_secs(1));
    });

    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("original.sh");
    std::fs::write(
        &script,
        b"#!/bin/sh\npayload=$(cat)\nprintf 'user:%s\\n' \"$payload\"\nprintf 'user-stderr\\n' >&2\nexit 23\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let payload = br#"{"model":{"display_name":"Opus"},"session_id":"same-json"}"#;

    let started = Instant::now();
    let mut child = Command::new(env!("CARGO_BIN_EXE_turn-hook"))
        .arg("--statusline")
        .arg(&script)
        .env(
            "TURN_STATUSLINE_URL",
            format!("http://{address}/hook/token/status-line"),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(payload).unwrap();
    let output = child.wait_with_output().unwrap();

    assert!(
        started.elapsed() < Duration::from_millis(500),
        "the network response delayed the user command: {:?}",
        started.elapsed()
    );
    assert_eq!(output.status.code(), Some(23));
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("user:{}\n", String::from_utf8_lossy(payload))
    );
    assert_eq!(String::from_utf8(output.stderr).unwrap(), "user-stderr\n");
    assert_eq!(
        received_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
        payload
    );
    server.join().unwrap();
}
