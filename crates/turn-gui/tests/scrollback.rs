//! Searching a pane's scrollback across the real daemon boundary.
//!
//! The claim this test exists to check is the one the feature stands on: **a search covers
//! output that scrolled off the screen long ago, and the window can scroll back to it**.
//! Everything below the protocol is exercised for real — a pty, a shell, the daemon's own
//! `vt100` parser and its five thousand rows of history — because that is the only place the
//! scrollback exists. A client-side test could not tell the difference between a search that
//! read the whole record and one that read the last forty rows of it.
//!
//! It talks to the socket directly rather than through `Desk`, so what it proves is the
//! protocol contract: request in, matches out, and a history window that holds the row the
//! match named. The window's own half — the offset arithmetic, the highlights, the bar — is
//! covered without a daemon in `terminal::search` and `terminal::feed`.
//!
//! The daemon's data directory and the workspace root are both inside one `TempDir`, so this
//! cannot touch a developer's Turn state.

#![cfg(unix)]

use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use turn_core::ids::{PaneId, SessionId};
use turn_core::model::PaneKind;
use turn_proto::search::{SearchQuery, MAX_MATCHES};
use turn_proto::{
    AuthToken, ClientFrame, ClientMessage, Grid, Hello, LineDecoder, NewPane, PaneStream, PtySize,
    Request, RequestId, Response, ServerFrame, ServerMessage,
};

const WAIT: Duration = Duration::from_secs(20);
/// The pane's geometry. Small on purpose: forty lines of output then fills the history
/// several times over, which is the case a search has to cover.
const SIZE: PtySize = PtySize { rows: 8, cols: 40 };

/// One client connection, spoken to in the protocol's own terms.
struct Client {
    stream: UnixStream,
    decoder: LineDecoder,
    next_id: u64,
    /// On the heap, and reused. A read buffer inside an `async fn` lives in the future, and a
    /// future carrying sixty-four kilobytes across an await is how an async test overflows a
    /// worker thread's stack.
    chunk: Vec<u8>,
}

impl Client {
    async fn connect(socket: &std::path::Path) -> Self {
        let token = std::fs::read_to_string(turn_proto::ipc_auth_token_path(socket))
            .expect("the daemon publishes its owner-only authentication token");
        let stream = UnixStream::connect(socket)
            .await
            .expect("the daemon's socket accepts a connection");
        let mut client = Self {
            stream,
            decoder: LineDecoder::new(),
            next_id: 0,
            chunk: vec![0u8; 16 * 1024],
        };
        client
            .write(&ClientMessage::Hello(Hello::new(
                "scrollback-test",
                "0.1.0",
                AuthToken::new(token),
            )))
            .await;
        let welcome = client.read_frame().await;
        assert!(
            matches!(welcome.message, ServerMessage::Welcome(_)),
            "the handshake must be answered first: {welcome:?}"
        );
        client
    }

    async fn write(&mut self, message: &ClientMessage) {
        let frame =
            turn_proto::encode(&ClientFrame::new(message.clone())).expect("a request serialises");
        self.stream
            .write_all(&frame)
            .await
            .expect("the socket accepts the frame");
    }

    async fn read_frame(&mut self) -> ServerFrame {
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(line) = self.decoder.next_line() {
                let line = line.expect("a frame within the line limit");
                return serde_json::from_slice(&line).expect("a frame the client understands");
            }
            assert!(Instant::now() < deadline, "the daemon went quiet");
            let read = tokio::time::timeout(WAIT, self.stream.read(&mut self.chunk))
                .await
                .expect("the daemon answers within the deadline")
                .expect("the socket stays readable");
            assert!(read > 0, "the daemon closed the connection");
            let chunk = std::mem::take(&mut self.chunk);
            self.decoder.feed(&chunk[..read]);
            self.chunk = chunk;
        }
    }

    /// Sends a request and returns its answer, letting pushes go past.
    async fn ask(&mut self, request: Request) -> Response {
        self.next_id += 1;
        let id = RequestId::new(format!("r-{}", self.next_id));
        let expected = request.expected_result();
        self.write(&ClientMessage::Request {
            id: id.clone(),
            request,
        })
        .await;
        loop {
            let frame = self.read_frame().await;
            match frame.message {
                ServerMessage::Response {
                    id: answered,
                    response,
                } if answered == id => {
                    assert_eq!(
                        response.result_name(),
                        expected,
                        "the daemon answered with the wrong result shape"
                    );
                    return response;
                }
                ServerMessage::Error {
                    id: Some(answered),
                    error,
                } if answered == id => panic!("the request failed: {error:?}"),
                // Events and other answers are not what this call asked for.
                _ => continue,
            }
        }
    }
}

/// The whole path: a shell prints two hundred lines, all but eight of them scroll off, and
/// the daemon finds one of the ones that did.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_search_finds_output_that_scrolled_off_the_screen_and_the_window_can_reach_it() {
    let state = tempfile::tempdir().expect("an isolated daemon data directory");
    let project = state.path().join("project");
    std::fs::create_dir(&project).expect("an isolated workspace root");

    let daemon = turnd::start(turnd::Config::in_dir(state.path()))
        .await
        .expect("the isolated daemon starts");
    let mut client = Client::connect(daemon.socket_path()).await;

    let workspace = match client
        .ask(Request::CreateWorkspace {
            name: "scrollback".into(),
            root: project.display().to_string(),
        })
        .await
    {
        Response::Workspace { workspace } => workspace.id,
        other => panic!("expected a workspace, got {other:?}"),
    };

    // A plain shell, so the output is a real program's rather than something the test wrote
    // into a buffer.
    let session = match client
        .ask(Request::CreateSession {
            workspace_id: workspace,
            name: "print a lot".into(),
            cwd: Some(project.display().to_string()),
            panes: Some(vec![NewPane::new(PaneKind::Shell).with_command("/bin/sh")]),
            note: None,
            tags: Vec::new(),
        })
        .await
    {
        Response::Session { session } => session,
        other => panic!("expected a session, got {other:?}"),
    };
    let session_id: SessionId = session.id.clone();
    let details = match client
        .ask(Request::GetSession {
            session_id: session_id.clone(),
        })
        .await
    {
        Response::SessionDetails { details } => details,
        other => panic!("expected session details, got {other:?}"),
    };
    let pane: PaneId = details
        .layout
        .panes()
        .first()
        .map(|pane| pane.id.clone())
        .expect("the session has a pane");
    let node = details
        .layout
        .panes()
        .first()
        .and_then(|pane| pane.node_id.clone())
        .expect("the pane has a process behind it");

    // Attaching is what starts the pump and fixes the geometry the screen is taken at.
    match client
        .ask(Request::AttachPane {
            session_id: session_id.clone(),
            pane_id: pane.clone(),
            size: SIZE,
            stream: PaneStream::Cells,
        })
        .await
    {
        Response::Attached { attachment } => {
            assert!(attachment.screen.is_some(), "cells were asked for");
        }
        other => panic!("expected an attachment, got {other:?}"),
    }

    // Two hundred numbered lines from a real shell: twenty-five screens' worth, so all but
    // the last eight rows are in the daemon's scrollback and nowhere else.
    client
        .ask(Request::WritePty {
            session_id: session_id.clone(),
            node_id: node.clone(),
            data: turn_proto::TerminalBytes::new(
                b"i=1; while [ $i -le 200 ]; do echo \"row $i needle$i\"; i=$((i+1)); done\n"
                    .to_vec(),
            ),
        })
        .await;

    // The shell has to actually run before there is anything to find, so the last line it
    // prints is what this waits for.
    let deadline = Instant::now() + WAIT;
    let outcome = loop {
        let answer = client
            .ask(Request::SearchPane {
                session_id: session_id.clone(),
                pane_id: pane.clone(),
                query: SearchQuery::literal("needle200"),
            })
            .await;
        if let Response::PaneMatches { outcome, .. } = answer {
            if !outcome.is_empty() {
                break outcome;
            }
        }
        assert!(
            Instant::now() < deadline,
            "the shell never printed its two hundredth line"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(outcome.count(), 1, "{:?}", outcome.matches);
    assert!(
        outcome.scrollback_len > 100,
        "two hundred lines on an eight-row screen must leave history behind: {} rows",
        outcome.scrollback_len
    );

    // The row this test cares about: `row 7`, printed a hundred and ninety lines ago, which
    // exists only in the daemon's parser.
    let found = match client
        .ask(Request::SearchPane {
            session_id: session_id.clone(),
            pane_id: pane.clone(),
            query: SearchQuery::literal("row 7 needle7"),
        })
        .await
    {
        Response::PaneMatches { outcome, .. } => outcome,
        other => panic!("expected matches, got {other:?}"),
    };
    // The whole line, so it cannot also match `row 70 needle70`. A trailing space would
    // never match at all: a row's trailing blanks are not part of it.
    assert_eq!(found.count(), 1, "{:?}", found.matches);
    let hit = found.matches[0];
    assert!(
        hit.line < found.scrollback_len,
        "the match must be in the history, not on the screen: line {} of {}",
        hit.line,
        found.scrollback_len
    );
    assert_eq!(hit.cols, "row 7 needle7".len() as u16);
    assert_eq!(hit.col, 0, "the line begins with it");

    // And the window can reach it: the offset the protocol computes, fetched as a window,
    // holds the row the match named.
    let offset = found.offset_for(0).expect("the match has an offset");
    let window: Grid = match client
        .ask(Request::GetPaneHistory {
            session_id: session_id.clone(),
            pane_id: pane.clone(),
            offset,
        })
        .await
    {
        Response::PaneHistory { grid, .. } => *grid,
        other => panic!("expected a history window, got {other:?}"),
    };
    let row = found
        .viewport_row(hit.line, window.scrollback_offset)
        .expect("the match is inside the window it asked for");
    assert!(
        window.row_text(row).contains("row 7 needle7"),
        "row {row} of the window is {:?}",
        window.row_text(row)
    );
    assert_eq!(window.cursor, None, "a history window carries no cursor");
    assert!(window.scrollback_len >= found.scrollback_len);

    // A case-insensitive literal is the default, and a pattern is only a pattern when asked
    // for: `row .` matches nothing literally and every row as a regular expression.
    let literal = match client
        .ask(Request::SearchPane {
            session_id: session_id.clone(),
            pane_id: pane.clone(),
            query: SearchQuery::literal("ROW 7 NEEDLE7"),
        })
        .await
    {
        Response::PaneMatches { outcome, .. } => outcome,
        other => panic!("expected matches, got {other:?}"),
    };
    assert_eq!(
        literal.count(),
        1,
        "case-insensitive by default: {:?}",
        literal.matches
    );

    let pattern = match client
        .ask(Request::SearchPane {
            session_id: session_id.clone(),
            pane_id: pane.clone(),
            query: SearchQuery::regex(r"needle1\d\d\b"),
        })
        .await
    {
        Response::PaneMatches { outcome, .. } => outcome,
        other => panic!("expected matches, got {other:?}"),
    };
    assert_eq!(
        pattern.count(),
        100,
        "needle100 through needle199: {}",
        pattern.count()
    );
    assert!(!pattern.truncated);
    assert!(pattern.count() <= MAX_MATCHES);

    // A pattern that cannot compile is refused with the reason, not answered with silence.
    client.next_id += 1;
    let id = RequestId::new(format!("r-{}", client.next_id));
    client
        .write(&ClientMessage::Request {
            id: id.clone(),
            request: Request::SearchPane {
                session_id: session_id.clone(),
                pane_id: pane.clone(),
                query: SearchQuery::regex("(unclosed"),
            },
        })
        .await;
    loop {
        match client.read_frame().await.message {
            ServerMessage::Error {
                id: Some(answered),
                error,
            } if answered == id => {
                assert_eq!(error.code, turn_proto::ErrorCode::InvalidArgument);
                assert!(
                    error.message.to_lowercase().contains("pattern"),
                    "the message must say what is wrong: {}",
                    error.message
                );
                break;
            }
            ServerMessage::Response { id: answered, .. } if answered == id => {
                panic!("an impossible pattern must not be answered as a search")
            }
            _ => continue,
        }
    }

    // Reading the history left the live screen in view for everybody else: the next screen
    // the daemon hands out is the current one, not the window that was borrowed.
    let live = match client
        .ask(Request::ResyncPane {
            session_id: session_id.clone(),
            pane_id: pane.clone(),
        })
        .await
    {
        Response::Screen { grid, .. } => *grid,
        other => panic!("expected a screen, got {other:?}"),
    };
    assert_eq!(live.scrollback_offset, 0);
    assert!(
        live.text().contains("needle200"),
        "the live screen must still be the live screen: {:?}",
        live.text()
    );

    client
        .ask(Request::CloseSession {
            session_id,
            disposition: turn_proto::CloseDisposition::Kill,
        })
        .await;
}
