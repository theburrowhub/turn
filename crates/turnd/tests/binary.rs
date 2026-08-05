//! The real binary: arguments, one instance at a time, and a signal that means stop.
//!
//! The other tests start the daemon in-process. These run `turnd` itself, because the
//! things they check only exist there: exit codes a supervisor branches on, a socket
//! resolved from the environment, and `SIGTERM` arriving from outside the program.

mod common;

use common::*;
use std::process::{Command, Stdio};
use turn_proto::Request;

/// The binary cargo just built.
const TURND: &str = env!("CARGO_BIN_EXE_turnd");

/// Starts the daemon binary over a directory, quietly.
fn spawn_daemon(dir: &std::path::Path) -> std::process::Child {
    Command::new(TURND)
        .arg("--data-dir")
        .arg(dir)
        .arg("--socket")
        .arg(dir.join("turnd.sock"))
        .arg("--log-level")
        .arg("error")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("turnd must start")
}

#[test]
fn the_binary_reports_its_version_and_its_usage() {
    let version = Command::new(TURND)
        .arg("--version")
        .output()
        .expect("turnd --version must run");
    assert!(version.status.success());
    let text = String::from_utf8_lossy(&version.stdout);
    assert!(text.starts_with("turnd "), "got {text:?}");
    assert!(text.contains(env!("CARGO_PKG_VERSION")));

    let help = Command::new(TURND)
        .arg("--help")
        .output()
        .expect("turnd --help must run");
    assert!(help.status.success());
    let text = String::from_utf8_lossy(&help.stdout);
    for flag in ["--socket", "--data-dir", "--log-level", "--no-persist"] {
        assert!(text.contains(flag), "the help must mention {flag}: {text}");
    }
}

#[test]
fn an_unusable_command_line_fails_before_anything_is_started() {
    let output = Command::new(TURND)
        .arg("--dance")
        .output()
        .expect("turnd must run");
    assert_eq!(
        output.status.code(),
        Some(2),
        "a usage error has its own exit code"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--dance"), "{stderr}");
    assert!(stderr.contains("Usage: turnd"), "{stderr}");

    // A flag that eats the next flag would be a daemon listening on a socket called
    // "--log-level".
    let output = Command::new(TURND)
        .args(["--socket", "--log-level", "debug"])
        .output()
        .expect("turnd must run");
    assert_eq!(output.status.code(), Some(2));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_daemon_serves_until_a_signal_and_then_flushes_and_goes() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let socket = dir.path().join("turnd.sock");
    let mut daemon = spawn_daemon(dir.path());
    wait_for_path(&socket).await;

    let mut ui = Client::connect(&socket).await;
    assert_eq!(
        ui.welcome.daemon_pid,
        daemon.id(),
        "the handshake names the process a UI is actually talking to"
    );
    assert!(ui.welcome.daemon_started_ms > 0);

    // Do enough work that there is something to flush.
    let workspace = workspace_of(
        ui.ask(Request::CreateWorkspace {
            name: "signalled".to_string(),
            root: dir.path().display().to_string(),
        })
        .await,
    );
    let session = session_of(
        ui.ask(Request::CreateSession {
            workspace_id: workspace.id.clone(),
            name: "before the signal".to_string(),
            cwd: None,
            panes: None,
            note: None,
            tags: Vec::new(),
        })
        .await,
    );
    drop(ui);

    send_signal(daemon.id(), SIGTERM);
    let status = tokio::task::spawn_blocking(move || daemon.wait())
        .await
        .expect("the wait must not panic")
        .expect("turnd must exit");
    assert!(
        status.success(),
        "a signalled daemon exits cleanly: {status}"
    );
    assert!(
        !socket.exists(),
        "the socket goes with the daemon, so the next start has nothing to diagnose"
    );

    // What was on the desk is on disk. A daemon that exited without flushing would leave
    // the user's session behind.
    let store = turn_store::Store::open_in(dir.path()).expect("the store must open");
    let sessions = store.sessions().list_all().expect("sessions must load");
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id, session.id);
    assert_eq!(sessions[0].name, "before the signal");
    assert_eq!(sessions[0].layout.pane_count(), 1);
    // The node is stored as it was — running — so the next start reads it as "was
    // running", checks the process table and reports honestly rather than assuming.
    assert_eq!(sessions[0].tree.len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_data_directory_rejects_another_socket_and_recovers_after_sigkill() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let socket = dir.path().join("turnd.sock");
    let mut first = spawn_daemon(dir.path());
    wait_for_path(&socket).await;

    let alternate_socket = dir.path().join("other.sock");
    let second = Command::new(TURND)
        .arg("--data-dir")
        .arg(dir.path())
        .arg("--socket")
        .arg(&alternate_socket)
        .arg("--log-level")
        .arg("error")
        .output()
        .expect("the second turnd must run");
    assert_eq!(
        second.status.code(),
        Some(3),
        "contention gets its own code so a launcher can connect to the running one \
         instead of reporting a failure"
    );
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(stderr.contains("already running"), "{stderr}");
    assert!(
        stderr.contains(&first.id().to_string()),
        "the message names the daemon to look at: {stderr}"
    );

    // The first one was not disturbed by any of that.
    let mut ui = Client::connect(&socket).await;
    ui.ask(Request::ListTemplates).await;
    drop(ui);

    // No shutdown handler runs. The socket file is stale, but the kernel releases
    // the process lock when SIGKILL closes the owning file table.
    send_signal(first.id(), SIGKILL);
    let status = tokio::task::spawn_blocking(move || first.wait())
        .await
        .expect("the wait task")
        .expect("the killed daemon must be reaped");
    assert!(!status.success());
    assert!(socket.exists(), "SIGKILL leaves the old socket pathname");

    let mut replacement = spawn_daemon(dir.path());
    let replacement_pid = replacement.id();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        match turnd::instance::probe(&socket).await {
            turnd::instance::Occupant::Live { pid, .. } if pid == replacement_pid => break,
            _ if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
            occupant => panic!("replacement daemon never acquired stale socket: {occupant:?}"),
        }
    }
    let mut ui = Client::connect(&socket).await;
    ui.ask(Request::ListTemplates).await;
    drop(ui);

    send_signal(replacement.id(), SIGTERM);
    let status = tokio::task::spawn_blocking(move || replacement.wait())
        .await
        .expect("the wait task")
        .expect("the replacement daemon must stop");
    assert!(status.success());
}
