//! The real binary: arguments, one instance at a time, and a signal that means stop.
//!
//! The other tests start the daemon in-process. These run `turnd` itself, because the
//! things they check only exist there: exit codes a supervisor branches on, a socket
//! resolved from the environment, and `SIGTERM` arriving from outside the program.

mod common;

use common::*;
use std::process::{Command, Stdio};
use turn_core::ids::CheckoutId;
use turn_core::model::{LeaseState, PaneKind};
use turn_core::state::Lifecycle;
use turn_proto::{ErrorCode, NewPane, Request, Response};

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

/// Waits until the daemon at `pid` is the one answering on `socket`.
async fn wait_until_serving(socket: &std::path::Path, pid: u32) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        match turnd::instance::probe(socket).await {
            turnd::instance::Occupant::Live { pid: owner, .. } if owner == pid => return,
            _ if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
            occupant => panic!("daemon {pid} never took the socket: {occupant:?}"),
        }
    }
}

/// Waits for pids to leave the process table, so a test cannot pass while the thing it
/// claims is gone is still running.
async fn wait_until_gone(pids: &[u32]) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while pids.iter().copied().any(pid_is_alive) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "a process the daemon owned outlived it: {pids:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
}

/// A Workspace and a Session with one shell pane, and the pids that shell produced.
async fn seed_writer_session(
    ui: &mut Client,
    root: &std::path::Path,
) -> (
    turn_core::ids::WorkspaceId,
    turn_core::ids::SessionId,
    Vec<u32>,
) {
    let workspace = workspace_of(
        ui.ask(Request::CreateWorkspace {
            name: "crashed".to_string(),
            root: root.display().to_string(),
        })
        .await,
    );
    let session = session_of(
        ui.ask(Request::CreateSession {
            workspace_id: workspace.id.clone(),
            name: "writer".to_string(),
            cwd: None,
            panes: Some(vec![NewPane::new(PaneKind::Shell)]),
            note: None,
            tags: Vec::new(),
        })
        .await,
    );
    let details = details_of(
        ui.ask(Request::GetSession {
            session_id: session.id.clone(),
        })
        .await,
    );
    let pids: Vec<u32> = details.tree.iter().filter_map(|node| node.pid).collect();
    assert!(!pids.is_empty(), "the shell pane must have a real process");
    (workspace.id, session.id, pids)
}

async fn lease_of(
    ui: &mut Client,
    workspace_id: &turn_core::ids::WorkspaceId,
) -> Option<turn_core::model::WorkspaceWriteLease> {
    match ui
        .ask(Request::GetWorkspaceWriteLease {
            workspace_id: workspace_id.clone(),
        })
        .await
    {
        Response::WorkspaceWriteLease { lease, .. } => lease,
        other => panic!("expected a write lease answer, got {other:?}"),
    }
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

    let build_info = Command::new(TURND)
        .arg("--build-info")
        .output()
        .expect("turnd --build-info must run");
    assert!(build_info.status.success());
    let text = String::from_utf8_lossy(&build_info.stdout);
    assert!(text.contains("component=turnd"), "got {text:?}");
    assert!(text.contains(&format!("version={}", env!("CARGO_PKG_VERSION"))));
    assert!(text.contains(&format!(
        "protocol_min={}",
        turn_proto::MIN_PROTOCOL_VERSION
    )));
    assert!(text.contains(&format!("protocol_max={}", turn_proto::PROTOCOL_VERSION)));

    let help = Command::new(TURND)
        .arg("--help")
        .output()
        .expect("turnd --help must run");
    assert!(help.status.success());
    let text = String::from_utf8_lossy(&help.stdout);
    for flag in [
        "--socket",
        "--data-dir",
        "--log-level",
        "--no-persist",
        "--delete-installation-data",
    ] {
        assert!(text.contains(flag), "the help must mention {flag}: {text}");
    }
}

#[test]
fn offline_installation_deletion_removes_private_data_and_keeps_checkout_work() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let worktree = dir.path().join(turnd::paths::WORKTREES_DIR).join("ws_keep");
    std::fs::create_dir_all(&worktree).unwrap();
    std::fs::write(worktree.join("work.txt"), b"keep").unwrap();
    std::fs::create_dir_all(dir.path().join(turnd::paths::SCRATCH_DIR)).unwrap();
    std::fs::write(dir.path().join(turnd::privacy::DAEMON_LOG_FILE), b"private").unwrap();
    let store = turn_store::Store::open_in(dir.path()).unwrap();
    drop(store);

    let output = Command::new(TURND)
        .args(["--data-dir", dir.path().to_str().unwrap()])
        .arg("--delete-installation-data")
        .output()
        .expect("the offline deletion must run");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: turn_core::privacy::InstallationPurgeReport =
        serde_json::from_slice(&output.stdout).unwrap();
    assert!(report.files_deleted >= 2);
    assert_eq!(std::fs::read(worktree.join("work.txt")).unwrap(), b"keep");
    assert!(!dir.path().join("turn.db").exists());
    assert!(!dir.path().join(turnd::paths::SCRATCH_DIR).exists());
    assert!(dir.path().join(".turnd.lock").is_file());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn offline_installation_deletion_refuses_a_live_daemon_without_removing_anything() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let socket = dir.path().join("turnd.sock");
    let mut daemon = spawn_daemon(dir.path());
    wait_until_serving(&socket, daemon.id()).await;
    let sentinel = dir.path().join(turnd::paths::SCRATCH_DIR).join("keep");
    std::fs::create_dir_all(sentinel.parent().unwrap()).unwrap();
    std::fs::write(&sentinel, b"private").unwrap();

    let output = Command::new(TURND)
        .arg("--data-dir")
        .arg(dir.path())
        .arg("--socket")
        .arg(&socket)
        .arg("--delete-installation-data")
        .output()
        .expect("the contending deletion must run");
    assert_eq!(output.status.code(), Some(3));
    assert_eq!(std::fs::read(&sentinel).unwrap(), b"private");

    send_signal(daemon.id(), SIGTERM);
    let status = tokio::task::spawn_blocking(move || daemon.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(status.success());
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
    // A signal is one of the routes out, so it releases the checkout too. Otherwise the
    // next start would read this ordinary stop as a crash and ask about it.
    assert!(
        store
            .hierarchy()
            .active_lease(&workspace.id)
            .expect("the lease query")
            .is_none(),
        "SIGTERM must hand the checkout back, not leave it looking crashed"
    );
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

/// A crash is the case the recovery prompt exists for, and even then the question only
/// earns its place when something is genuinely still writing.
///
/// `SIGKILL` runs no handler, so the lease on disk is left exactly as a crash leaves it:
/// unreleased. What makes this start silent is evidence rather than optimism — the kernel
/// released the data-directory lock, which proves no other daemon has this store, and
/// every process the dead daemon recorded is gone from the process table.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_killed_daemon_whose_processes_all_died_asks_nothing_on_the_next_start() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let socket = dir.path().join("turnd.sock");
    let mut daemon = spawn_daemon(dir.path());
    wait_for_path(&socket).await;

    let mut ui = Client::connect(&socket).await;
    let (workspace_id, session_id, pids) = seed_writer_session(&mut ui, dir.path()).await;
    let before = lease_of(&mut ui, &workspace_id)
        .await
        .expect("the Session owns its checkout while it runs");
    assert_eq!(before.state, LeaseState::Active);
    drop(ui);

    send_signal(daemon.id(), SIGKILL);
    let status = tokio::task::spawn_blocking(move || daemon.wait())
        .await
        .expect("the wait task")
        .expect("the killed daemon must be reaped");
    assert!(!status.success(), "SIGKILL is not a clean exit");
    // The premise: the ptys died with the process that owned them.
    wait_until_gone(&pids).await;

    let mut replacement = spawn_daemon(dir.path());
    wait_until_serving(&socket, replacement.id()).await;
    let mut ui = Client::connect(&socket).await;

    let after = lease_of(&mut ui, &workspace_id)
        .await
        .expect("the crashed generation's authority is decided, not abandoned");
    assert_eq!(
        after.state,
        LeaseState::Active,
        "nothing from the dead daemon survived, so there is nothing to ask about"
    );
    assert_eq!(after.session_id, session_id);
    assert_eq!(
        after.id, before.id,
        "a crash leaves a lease to be adopted, and the adopted row keeps its identity"
    );
    assert!(
        after.generation > before.generation,
        "adopting it advances the fence, so a helper holding the old token is invalidated"
    );

    // And it is usable immediately: the pane whose process died can be started again
    // without a confirmation step.
    let details = details_of(
        ui.ask(Request::GetSession {
            session_id: session_id.clone(),
        })
        .await,
    );
    let dead = details
        .tree
        .iter()
        .find(|node| node.lifecycle == Lifecycle::Lost)
        .expect("the shell that died with the daemon is reported as lost");
    let restarted = node_of(
        ui.ask(Request::RelaunchNode {
            session_id: session_id.clone(),
            node_id: dead.node_id.clone(),
            resume: false,
        })
        .await,
    );
    assert!(restarted.lifecycle.is_running());
    drop(ui);

    send_signal(replacement.id(), SIGTERM);
    let status = tokio::task::spawn_blocking(move || replacement.wait())
        .await
        .expect("the wait task")
        .expect("the replacement daemon must stop");
    assert!(status.success());
}

/// The question, kept for the one case that deserves it: a process from the dead daemon
/// is still running against the checkout. Turn asks, refuses until it is really gone, and
/// the confirmation still works the moment it is.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_killed_daemon_with_a_process_still_running_asks_before_giving_the_checkout_back() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let socket = dir.path().join("turnd.sock");
    let mut daemon = spawn_daemon(dir.path());
    wait_for_path(&socket).await;

    let mut ui = Client::connect(&socket).await;
    let (workspace_id, session_id, pids) = seed_writer_session(&mut ui, dir.path()).await;
    let before = lease_of(&mut ui, &workspace_id)
        .await
        .expect("the Session owns its checkout while it runs");
    let details = details_of(
        ui.ask(Request::GetSession {
            session_id: session_id.clone(),
        })
        .await,
    );
    let node_id = details
        .tree
        .iter()
        .find(|node| node.pid.is_some())
        .expect("the shell pane's runtime node")
        .node_id
        .clone();
    drop(ui);

    send_signal(daemon.id(), SIGKILL);
    tokio::task::spawn_blocking(move || daemon.wait())
        .await
        .expect("the wait task")
        .expect("the killed daemon must be reaped");
    wait_until_gone(&pids).await;

    // A child that outlived the daemon: an agent still writing to the checkout, which is
    // what the exclusive lease exists to prevent a second writer joining.
    let mut survivor = std::process::Command::new("sleep")
        .arg("300")
        .spawn()
        .expect("the orphaned runtime");
    {
        let store = turn_store::Store::open_in(dir.path()).expect("the store must reopen");
        let mut session = store
            .sessions()
            .get(&session_id)
            .expect("the session query")
            .expect("the crashed daemon's durable Session");
        let node = session
            .tree
            .get_mut(&node_id)
            .expect("the pane's persisted runtime");
        node.pid = Some(survivor.id());
        node.command = "sleep 300".into();
        node.lifecycle = Lifecycle::Alive;
        store
            .sessions()
            .save(&session)
            .expect("the survivor metadata must save");
    }

    let mut replacement = spawn_daemon(dir.path());
    wait_until_serving(&socket, replacement.id()).await;
    let mut ui = Client::connect(&socket).await;

    let fenced = lease_of(&mut ui, &workspace_id)
        .await
        .expect("the fenced lease");
    assert_eq!(fenced.state, LeaseState::RecoveryRequired);
    assert_eq!(fenced.id, before.id);
    assert_eq!(fenced.generation, before.generation);
    assert_eq!(
        fenced.heartbeat_ms, before.heartbeat_ms,
        "starting a new daemon must not forge the previous owner's heartbeat"
    );

    let refused = ui
        .try_ask(Request::AcquireWorkspaceWriteLease {
            workspace_id: workspace_id.clone(),
            session_id: session_id.clone(),
            checkout_id: CheckoutId::primary_for(&workspace_id),
        })
        .await
        .expect_err("a living process from the previous daemon still owns the checkout");
    assert_eq!(refused.code, ErrorCode::Conflict);

    survivor
        .kill()
        .expect("the user can stop the orphan outside Turn, as the prompt asks");
    survivor.wait().expect("the orphan must be reaped");

    let confirmed = match ui
        .ask(Request::AcquireWorkspaceWriteLease {
            workspace_id: workspace_id.clone(),
            session_id: session_id.clone(),
            checkout_id: CheckoutId::primary_for(&workspace_id),
        })
        .await
    {
        Response::WorkspaceWriteLease {
            lease: Some(lease), ..
        } => lease,
        other => panic!("expected the confirmed lease, got {other:?}"),
    };
    assert_eq!(confirmed.state, LeaseState::Active);
    assert_eq!(confirmed.id, fenced.id, "the exact fenced claim is adopted");
    assert!(confirmed.generation > fenced.generation);
    drop(ui);

    send_signal(replacement.id(), SIGTERM);
    let status = tokio::task::spawn_blocking(move || replacement.wait())
        .await
        .expect("the wait task")
        .expect("the replacement daemon must stop");
    assert!(status.success());
}
