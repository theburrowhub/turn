//! The terminal screen, end to end: a real process, real cells, real diffs.
//!
//! Everything here drives the daemon over its socket the way the client does, with a
//! process that really runs on a pty. What is asserted is what a renderer would put on
//! screen: the characters the process printed, the colours it asked for, and the fact
//! that a keystroke costs a row rather than a screenful.
//!
//! The recovery path gets the same treatment: a client that throws an update away is a
//! client that fell behind, and it recovers here against a live pane. The daemon's own
//! repair — noticing a frame it could not deliver and making the next update a whole
//! screen — needs a client whose socket really stalls, which is a thing a unit test can
//! arrange and a socket test cannot; it lives in `core::screens`.

mod common;

use common::*;
use turn_core::ids::PaneId;
use turn_core::model::PaneKind;
use turn_proto::{
    ErrorCode, Grid, NewPane, PaneStream, PtySize, Request, ServerEvent, SessionSummary,
};

/// A session with one pane running a shell, on a daemon with only the built-in
/// adapters — so the pane is the plain terminal it says it is.
async fn shell_session(daemon: &TestDaemon, ui: &mut Client) -> (SessionSummary, PaneId) {
    let workspace = workspace_of(
        ui.ask(Request::CreateWorkspace {
            name: "cells".to_string(),
            root: daemon.data_dir().display().to_string(),
        })
        .await,
    );
    let session = session_of(
        ui.ask(Request::CreateSession {
            workspace_id: workspace.id.clone(),
            name: "a terminal".to_string(),
            cwd: None,
            // `sh` rather than the user's shell: a prompt nobody configured, and
            // `printf` behaves the same everywhere.
            panes: Some(vec![NewPane::new(PaneKind::Terminal).with_command("sh")]),
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
    let pane = details.layout.panes()[0].id.clone();
    (session, pane)
}

/// Types a line into a pane's process.
async fn type_line(ui: &mut Client, session: &SessionSummary, pane: &PaneId, line: &str) {
    let details = details_of(
        ui.ask(Request::GetSession {
            session_id: session.id.clone(),
        })
        .await,
    );
    let node = details
        .layout
        .get(pane)
        .and_then(|pane| pane.node_id.clone())
        .expect("the pane has a process");
    ui.ask(Request::WritePty {
        session_id: session.id.clone(),
        node_id: node,
        data: turn_proto::TerminalBytes::new(format!("{line}\n").into_bytes()),
    })
    .await;
}

fn history_text(attachment: &turn_proto::PaneAttachment) -> String {
    attachment
        .scrollback
        .decode_rows()
        .expect("the daemon emits valid scrollback")
        .iter()
        .map(|row| {
            row.iter()
                .map(|cell| cell.text.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restarting_reconstructs_the_visible_terminal_and_scrollback_without_claiming_it_is_alive()
{
    let daemon = TestDaemon::start_plain().await;
    let mut ui = daemon.connect().await;
    let (session, pane) = shell_session(&daemon, &mut ui).await;
    ui.attach_cells(&session.id, &pane, PtySize::new(4, 32))
        .await;

    type_line(
        &mut ui,
        &session,
        &pane,
        "i=0; while [ $i -lt 12 ]; do printf 'JOURNAL-%02d\\n' $i; i=$((i+1)); done",
    )
    .await;
    ui.wait_for_screen("JOURNAL-11").await;
    let before_screen = ui.screen(&session.id, &pane).clone();
    let before = ui
        .attach_cells(&session.id, &pane, PtySize::new(4, 32))
        .await;
    assert!(history_text(&before).contains("JOURNAL-00"));
    assert!(before.bytes_seen > 0);
    drop(ui);

    let daemon = daemon.restart().await;
    let mut ui = daemon.connect().await;
    let details = details_of(
        ui.ask(Request::GetSession {
            session_id: session.id.clone(),
        })
        .await,
    );
    assert!(details
        .tree
        .iter()
        .all(|node| node.lifecycle == turn_core::state::Lifecycle::Lost));

    let recovered = ui
        .attach_cells(&session.id, &pane, PtySize::new(4, 32))
        .await;
    assert_eq!(
        recovered.screen.as_deref(),
        Some(&before_screen),
        "the recovered grid is display history, not a blank replacement"
    );
    assert_eq!(history_text(&recovered), history_text(&before));
    assert_eq!(recovered.bytes_seen, before.bytes_seen);
    assert!(history_text(&recovered).contains("JOURNAL-00"));

    daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_sensitive_session_can_disable_terminal_history_before_launch() {
    let daemon = TestDaemon::start_plain().await;
    let mut ui = daemon.connect().await;
    let workspace = workspace_of(
        ui.ask(Request::CreateWorkspace {
            name: "private terminal".to_string(),
            root: daemon.data_dir().display().to_string(),
        })
        .await,
    );
    drop(ui);
    let dir = daemon.stop().await;

    let store = turn_store::Store::open_in(dir.path()).unwrap();
    let mut stored = store
        .workspaces()
        .list()
        .unwrap()
        .into_iter()
        .find(|candidate| candidate.id == workspace.id)
        .unwrap();
    stored
        .env
        .push(("TURN_TERMINAL_HISTORY".into(), "disabled".into()));
    store.workspaces().save(&stored).unwrap();
    drop(store);

    let daemon = TestDaemon::adopt_with(dir, turn_agents::AdapterRegistry::with_builtin).await;
    let mut ui = daemon.connect().await;
    let session = session_of(
        ui.ask(Request::CreateSession {
            workspace_id: workspace.id,
            name: "no terminal history".to_string(),
            cwd: None,
            panes: Some(vec![NewPane::new(PaneKind::Terminal).with_command("sh")]),
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
    let pane = details.layout.panes()[0].id.clone();
    ui.attach_cells(&session.id, &pane, PtySize::new(4, 32))
        .await;
    type_line(&mut ui, &session, &pane, "printf 'PRIVATE-OUTPUT\\n'").await;
    ui.wait_for_screen("PRIVATE-OUTPUT").await;

    assert!(
        !turnd::paths::session_terminal_history(daemon.data_dir(), &session.id).exists(),
        "an opted-out session must not create a raw terminal archive"
    );
    daemon.shutdown().await;
}

/// The headline: what the process printed arrives as cells, and an ANSI colour arrives
/// as a concrete value rather than as an index the client would have to interpret.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_cells_that_arrive_carry_what_the_process_printed_in_the_colours_it_asked_for() {
    let daemon = TestDaemon::start_plain().await;
    let mut ui = daemon.connect().await;
    let (session, pane) = shell_session(&daemon, &mut ui).await;

    ui.attach_cells(&session.id, &pane, PtySize::new(20, 60))
        .await;
    // Red on default, then a plain word: `printf` writes the escape sequences and the
    // daemon's parser is what turns them into attributes.
    type_line(
        &mut ui,
        &session,
        &pane,
        r"printf '\033[31mCRIMSON\033[0m ordinary\n'",
    )
    .await;
    ui.wait_for_screen("CRIMSON ordinary").await;

    let screen = ui.screen(&session.id, &pane);
    // The word appears twice — the shell echoes the command line as typed — and the
    // coloured one is the output rather than the echo.
    let coloured: Vec<&turn_proto::Cell> = screen
        .cells
        .iter()
        .filter(|cell| cell.fg == Some(turn_proto::indexed_rgb(1)))
        .collect();
    assert!(
        coloured.len() >= 7,
        "the seven letters of the printed word must carry the terminal's red, got {} cells",
        coloured.len()
    );
    let word: String = coloured
        .iter()
        .take(7)
        .map(|cell| cell.text.clone())
        .collect();
    assert_eq!(word, "CRIMSON");
    assert!(
        coloured.iter().all(|cell| cell.bg.is_none()),
        "a colour the program did not set must stay the theme's business"
    );

    // And the escape sequences themselves are nowhere on the screen: they were parsed,
    // not printed.
    let text = screen.text();
    assert!(
        !text.contains('\u{1b}'),
        "an escape character reached the cells: {text:?}"
    );

    daemon.shutdown().await;
}

/// A line of output costs the rows it changed. This is the whole reason the update is a
/// diff, so the size is asserted rather than described.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_line_of_output_costs_a_few_rows_rather_than_the_whole_screen() {
    let daemon = TestDaemon::start_plain().await;
    let mut ui = daemon.connect().await;
    let (session, pane) = shell_session(&daemon, &mut ui).await;

    ui.attach_cells(&session.id, &pane, PtySize::new(40, 120))
        .await;
    // Settle the prompt first, so what follows is one line of output rather than a
    // shell starting up.
    type_line(&mut ui, &session, &pane, "printf 'settled\\n'").await;
    ui.wait_for_screen("settled").await;

    type_line(&mut ui, &session, &pane, "printf 'one more line\\n'").await;
    let update = ui
        .wait_for("a screen update", |event| match event {
            ServerEvent::PaneScreen { update, .. } => Some(update.clone()),
            _ => None,
        })
        .await;

    assert!(
        !update.is_full(),
        "a line of output must not resend the screen: {update:?}"
    );
    let diff_bytes = serde_json::to_string(&update)
        .expect("an update serialises")
        .len();
    let whole_screen = serde_json::to_string(&Grid::blank(40, 120))
        .expect("a grid serialises")
        .len();
    assert!(
        diff_bytes < whole_screen,
        "the diff cost {diff_bytes} bytes against {whole_screen} for a blank screen"
    );

    daemon.shutdown().await;
}

/// The resync path, driven by dropping an update on the floor — which is what a client
/// that stalled has effectively done.
///
/// Two things have to hold afterwards: the recovered screen holds what was missed, and
/// the updates that follow apply to it cleanly. The second is the one that matters,
/// because it is the case where a naive implementation appears to work and leaves the
/// two screens quietly disagreeing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_client_that_dropped_an_update_recovers_the_whole_screen_and_carries_on() {
    let daemon = TestDaemon::start_plain().await;
    let mut ui = daemon.connect().await;
    let (session, pane) = shell_session(&daemon, &mut ui).await;

    ui.attach_cells(&session.id, &pane, PtySize::new(20, 60))
        .await;
    type_line(&mut ui, &session, &pane, "printf 'before-the-gap\\n'").await;

    // The update arrives and is thrown away unapplied: the client has missed it.
    let dropped = ui
        .wait_for("the update that will be dropped", |event| match event {
            ServerEvent::PaneScreen { seq, .. } => Some(*seq),
            _ => None,
        })
        .await;
    assert_eq!(dropped, 0, "the first update starts the sequence");
    assert!(
        !ui.screen(&session.id, &pane)
            .text()
            .contains("before-the-gap"),
        "the point of this test is that the client has not applied it"
    );

    // It asks for the screen instead, and gets all of it.
    let recovered = ui.resync(&session.id, &pane).await;
    assert!(
        recovered.text().contains("before-the-gap"),
        "the recovered screen must hold what the dropped update carried: {:?}",
        recovered.text()
    );

    // And the stream continues from there: `wait_for_screen` asserts the sequence, so a
    // resync that left the client's idea of `seq` wrong would fail here.
    type_line(&mut ui, &session, &pane, "printf 'after-the-gap\\n'").await;
    let text = ui.wait_for_screen("after-the-gap").await;
    assert!(
        text.contains("before-the-gap"),
        "and nothing that was already on screen was lost: {text:?}"
    );

    daemon.shutdown().await;
}

/// The byte stream is still there for whatever genuinely needs it, and a client has to
/// ask: this is the one attach in the suite that does.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_client_that_asks_for_bytes_gets_the_escape_stream_and_a_replay() {
    let daemon = TestDaemon::start_plain().await;
    let mut ui = daemon.connect().await;
    let (session, pane) = shell_session(&daemon, &mut ui).await;

    let attachment = attachment_of(
        ui.ask(Request::AttachPane {
            session_id: session.id.clone(),
            pane_id: pane.clone(),
            size: PtySize::new(20, 60),
            stream: PaneStream::Bytes,
        })
        .await,
    );
    assert_eq!(attachment.stream, PaneStream::Bytes);
    assert!(
        attachment.screen.is_none(),
        "a byte attachment must not pay for a grid it did not ask for"
    );

    type_line(&mut ui, &session, &pane, r"printf '\033[32mgreen\033[0m\n'").await;
    // The interactive shell first echoes the command, which contains the literal word
    // `green`. Wait for the rendered control sequence so that echo can never satisfy
    // the byte-stream assertion early on a slower run.
    let output = ui.wait_for_output("\u{1b}[32mgreen").await;
    assert!(
        output.contains("\u{1b}[32m"),
        "the escape sequences are the point of this stream: {output:?}"
    );

    // A byte attachment cannot resync a screen it never had; the honest answer says so
    // rather than inventing cells for it.
    let error = ui
        .try_ask(Request::ResyncPane {
            session_id: session.id.clone(),
            pane_id: pane.clone(),
        })
        .await
        .expect_err("there is no screen to resend");
    assert_eq!(error.code, ErrorCode::Conflict);
    assert!(
        error.message.contains("byte stream"),
        "the message must say what to do instead: {}",
        error.message
    );

    daemon.shutdown().await;
}

/// Two clients on the same pane. Each has its own sequence, and each ends up with the
/// same screen — which is what makes "the two screens disagree" a bug class this design
/// removes rather than one it moves.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_clients_watching_one_pane_end_up_with_the_same_screen() {
    let daemon = TestDaemon::start_plain().await;
    let mut first = daemon.connect().await;
    let (session, pane) = shell_session(&daemon, &mut first).await;
    let mut second = daemon.connect().await;

    first
        .attach_cells(&session.id, &pane, PtySize::new(20, 60))
        .await;
    second
        .attach_cells(&session.id, &pane, PtySize::new(20, 60))
        .await;

    type_line(&mut first, &session, &pane, "printf 'shared-line\\n'").await;
    first.wait_for_screen("shared-line").await;
    second.wait_for_screen("shared-line").await;

    assert_eq!(
        first.screen(&session.id, &pane).text(),
        second.screen(&session.id, &pane).text(),
        "two clients rendering one pane must not see different things"
    );

    daemon.shutdown().await;
}

/// A resize is the case a row diff cannot describe, so every watcher is handed a whole
/// screen at the new geometry — including the one that asked for the resize.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resizing_hands_the_client_a_whole_screen_at_its_new_geometry() {
    let daemon = TestDaemon::start_plain().await;
    let mut ui = daemon.connect().await;
    let (session, pane) = shell_session(&daemon, &mut ui).await;

    ui.attach_cells(&session.id, &pane, PtySize::new(20, 60))
        .await;
    type_line(&mut ui, &session, &pane, "printf 'before-resize\\n'").await;
    ui.wait_for_screen("before-resize").await;

    let details = details_of(
        ui.ask(Request::GetSession {
            session_id: session.id.clone(),
        })
        .await,
    );
    let node = details
        .layout
        .get(&pane)
        .and_then(|pane| pane.node_id.clone())
        .expect("the pane has a process");
    ui.ask(Request::ResizePty {
        session_id: session.id.clone(),
        node_id: node,
        size: PtySize::new(30, 100),
    })
    .await;

    // Applied through the same path as any other update, so the sequence is checked.
    ui.poll_screens().await;
    let screen = ui.screen(&session.id, &pane);
    assert_eq!(
        (screen.rows, screen.cols),
        (30, 100),
        "the client must be given the screen at the size it is now rendering"
    );

    // The screen model changing is not enough: the program on the slave side of
    // the pty must receive the same geometry (and the corresponding SIGWINCH), or
    // a full-screen application will keep painting into its old 80-column box.
    // This is the user-visible regression that left most of a large Pane empty.
    type_line(&mut ui, &session, &pane, "stty size").await;
    ui.wait_for_screen("30 100").await;

    daemon.shutdown().await;
}

/// A geometry no terminal has is refused, so the cap is something a client can rely on
/// rather than something it discovers as a frame that never arrives.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn attaching_at_a_geometry_over_the_announced_limit_is_refused() {
    let daemon = TestDaemon::start_plain().await;
    let mut ui = daemon.connect().await;
    let (session, pane) = shell_session(&daemon, &mut ui).await;

    let limit = ui.welcome.limits.max_screen_cells;
    assert_eq!(
        limit,
        turn_proto::MAX_SCREEN_CELLS,
        "announced in `welcome`"
    );

    let error = ui
        .try_ask(Request::AttachPane {
            session_id: session.id.clone(),
            pane_id: pane.clone(),
            size: PtySize::new(1_000, 1_000),
            stream: PaneStream::Cells,
        })
        .await
        .expect_err("a million cells is not a screen");
    assert_eq!(error.code, ErrorCode::InvalidArgument);

    // And the pane is still attachable at a real size afterwards.
    let attachment = ui
        .attach_cells(&session.id, &pane, PtySize::new(24, 80))
        .await;
    assert_eq!(attachment.size, PtySize::new(24, 80));

    daemon.shutdown().await;
}

/// A pane nobody attached produces no terminal traffic at all. With thirty sessions
/// open and one on screen, this is the difference between a daemon that idles and one
/// that does not.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unattached_pane_sends_nothing_however_much_it_prints() {
    let daemon = TestDaemon::start_plain().await;
    let mut ui = daemon.connect().await;
    let (session, pane) = shell_session(&daemon, &mut ui).await;

    // Nothing is attached. The process prints a great deal.
    type_line(
        &mut ui,
        &session,
        &pane,
        "i=0; while [ $i -lt 200 ]; do printf 'noise %s\\n' $i; i=$((i+1)); done",
    )
    .await;
    // Long enough for several coalescing windows to have passed.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    ui.poll_events().await;

    let terminal_traffic = ui.buffered().filter(|event| event.is_output()).count();
    assert_eq!(
        terminal_traffic, 0,
        "an unwatched pane must not cost the socket anything"
    );

    // And attaching afterwards reads the screen out of the buffer that was being kept
    // all along, so nothing was lost by not sending it.
    let attachment = ui
        .attach_cells(&session.id, &pane, PtySize::new(24, 80))
        .await;
    let screen = attachment.screen.expect("the screen as cells");
    assert!(
        screen.text().contains("noise 199"),
        "the last of what it printed must be there: {:?}",
        screen.text()
    );

    daemon.shutdown().await;
}

/// A flood produces updates in step with the coalescing window, not one per write.
/// Without the batching a program printing two hundred lines would produce two hundred
/// frames, most of them describing rows that had already scrolled away.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_flood_of_output_is_coalesced_into_far_fewer_updates_than_lines() {
    let daemon = TestDaemon::start_plain().await;
    let mut ui = daemon.connect().await;
    let (session, pane) = shell_session(&daemon, &mut ui).await;

    ui.attach_cells(&session.id, &pane, PtySize::new(24, 80))
        .await;
    type_line(
        &mut ui,
        &session,
        &pane,
        "i=0; while [ $i -lt 200 ]; do printf 'flood %s\\n' $i; i=$((i+1)); done",
    )
    .await;
    ui.wait_for_screen("flood 199").await;
    // Whatever else the flood produced.
    ui.poll_screens().await;

    let updates = ui.updates_applied(&session.id, &pane);
    assert!(
        updates < 100,
        "two hundred lines produced {updates} updates; the coalescing window is not working"
    );
    // And the screen is right, not merely cheap.
    assert!(ui.screen(&session.id, &pane).text().contains("flood 199"));

    daemon.shutdown().await;
}
