//! The cell screens attached clients render, and the diffs that keep them current.
//!
//! The daemon already parses every PTY-backed node's output — it has to, because previews
//! and the output heuristics work with no client attached. So a client that renders cells
//! costs almost nothing extra: the screen is there, and what travels is the
//! difference between it and the screen that client last saw.
//!
//! Three decisions here are deliberate.
//!
//! **One baseline per node, not per client.** The diff has to be computed against
//! something both ends agree on, and every attachment to a node shares its geometry,
//! so one grid per *watched* node is enough. A client joining late is handed that same
//! grid by `attach_pane`, which puts it exactly in step; a client that lost a frame is
//! marked as owing a whole screen and gets one on the next update. Keeping a grid per
//! client instead would multiply the memory by the number of windows and buy nothing.
//!
//! **Nothing is kept for a pane nobody watches.** The baseline appears when the first
//! cells attachment does and goes when the last watcher leaves, so thirty idle sessions
//! cost thirty pty buffers and no grids.
//!
//! **A lagging pty read is not a gap for a cells client.** The byte stream has to admit
//! dropped output because the bytes are gone; a screen does not, because it is rebuilt
//! from the authoritative buffer every time. Whatever the pump missed is already in the
//! screen it reads. The only loss a cells client can suffer is a dropped *frame*, and
//! the repair for that is the next update carrying everything.

use super::clients::AttachmentKey;
use super::{ClientId, Core};
use turn_core::ids::NodeId;
use turn_proto::{Grid, PtySize, ScreenUpdate, ServerEvent};

impl Core {
    /// Reads the pane's current screen as cells.
    ///
    /// A node with no live pty — orphaned or lost after a restart — reads as a blank
    /// screen at the client's own size rather than as nothing: the pane is real, and a
    /// renderer with no grid at all has nothing to draw.
    pub(crate) fn build_grid(&self, node: &NodeId, size: PtySize) -> Grid {
        let Some(process) = self.processes.get(node) else {
            return Grid::blank(size.rows, size.cols);
        };
        let shared = process.pty.buffer();
        let grid = match shared.lock() {
            // The one reading of what a parsed screen means as cells, shared with the
            // client — see `turn_proto::cells::from_screen`. Palette indices are
            // resolved and reversed video is applied there.
            Ok(buffer) => turn_proto::from_screen(buffer.screen()),
            // The mutex is only poisoned if a pty reader thread panicked while holding
            // it. The pane is still real, so a blank screen is the honest answer.
            Err(_) => Grid::blank(size.rows, size.cols),
        };
        grid
    }

    /// The geometry a node's screen is currently drawn at.
    pub(crate) fn node_size(&self, node: &NodeId, fallback: PtySize) -> PtySize {
        self.processes
            .get(node)
            .map(|process| PtySize::new(process.size.rows, process.size.cols))
            .unwrap_or(fallback)
    }

    /// The grid every cells attachment on this node is in step with, building it if
    /// this is the first one.
    ///
    /// Returned as a clone rather than the live pty screen, deliberately: the grid a
    /// client is given has to be the exact anchor the next diff is computed against, or
    /// a row that changed and changed back between the two would never be corrected.
    pub(crate) fn screen_for_attach(&mut self, node: &NodeId, size: PtySize) -> Grid {
        let wanted = self.node_size(node, size);
        if let Some(existing) = self.screens.get(node) {
            if existing.rows == wanted.rows && existing.cols == wanted.cols {
                return existing.clone();
            }
        }
        let grid = self.build_grid(node, wanted);
        self.screens.insert(node.clone(), grid.clone());
        grid
    }

    /// Drops a node's baseline, once nobody is watching it.
    pub(crate) fn forget_screen(&mut self, node: &NodeId) {
        self.screens.remove(node);
    }

    /// Every cells attachment watching a node, across clients.
    fn cells_targets(&self, node: &NodeId) -> Vec<(ClientId, AttachmentKey)> {
        let mut targets = Vec::new();
        for (id, client) in self.clients.iter() {
            for (key, attachment) in client.attachments.iter() {
                if attachment.node_id.as_ref() == Some(node) && attachment.stream.is_cells() {
                    targets.push((*id, key.clone()));
                }
            }
        }
        targets
    }

    /// Sends what changed on a node's screen to everyone rendering it as cells.
    ///
    /// Called once per coalesced read, so a pane producing a flood produces one update
    /// per batching window rather than one per write.
    pub(crate) fn deliver_screen(&mut self, node: &NodeId) {
        let targets = self.cells_targets(node);
        if targets.is_empty() {
            return;
        }

        let size = self.node_size(node, PtySize::default());
        let grid = self.build_grid(node, size);
        let update = match self.screens.get(node) {
            Some(previous) if previous == &grid => None,
            Some(previous) => Some(ScreenUpdate::between(previous, &grid)),
            // No baseline: whoever is attached has not been given a screen yet, so the
            // only truthful thing to send is all of it.
            None => Some(ScreenUpdate::full(grid.clone())),
        };
        self.screens.insert(node.clone(), grid.clone());

        for (client_id, key) in targets {
            let owed = self
                .clients
                .get(&client_id)
                .and_then(|client| client.attachments.get(&key))
                .is_some_and(|attachment| attachment.owes_full_screen);
            // A client that lost a frame cannot be sent rows: it would apply them to a
            // screen that is already wrong. It gets the whole thing, whatever changed.
            let payload = match (owed, &update) {
                (true, _) => ScreenUpdate::full(grid.clone()),
                (false, Some(update)) => update.clone(),
                // Nothing changed and nothing is owed: no frame at all. A bell or a
                // mode change that leaves the cells alone is not a redraw.
                (false, None) => continue,
            };
            self.send_screen(client_id, &key, node, payload);
        }
    }

    /// Rebuilds the baseline and sends the whole screen to everyone rendering it.
    ///
    /// The path a resize takes. A row diff across a resize is meaningless — the rows do
    /// not correspond — and a client that resized has no grid at its new geometry until
    /// the program next draws, which for a program that does not redraw is never.
    pub(crate) fn push_full_screen(&mut self, node: &NodeId, except: Option<ClientId>) {
        let watching = self.cells_targets(node);
        if watching.is_empty() {
            // Nobody renders this node as cells, so a baseline at the old geometry is
            // only something for a later attach to trip over.
            self.screens.remove(node);
            return;
        }
        // Rebuilt whether or not anyone is being told: the client excluded here is the
        // one that was handed this grid directly, and the baseline has to be the grid
        // every attachment is in step with.
        let size = self.node_size(node, PtySize::default());
        let grid = self.build_grid(node, size);
        self.screens.insert(node.clone(), grid.clone());

        for (client_id, key) in watching {
            if Some(client_id) == except {
                continue;
            }
            self.send_screen(client_id, &key, node, ScreenUpdate::full(grid.clone()));
        }
    }

    /// Sends one update, and records whether the client is now behind.
    ///
    /// The sequence number advances whether or not the frame lands, so a client can
    /// spot the jump and ask for a resync itself. The daemon does not depend on it
    /// doing so: the attachment is marked as owing a whole screen, and the next update
    /// carries one.
    fn send_screen(
        &mut self,
        client_id: ClientId,
        key: &AttachmentKey,
        node: &NodeId,
        update: ScreenUpdate,
    ) {
        let (session_id, pane_id) = key.clone();
        let Some(client) = self.clients.get_mut(&client_id) else {
            return;
        };
        let Some(attachment) = client.attachments.get_mut(key) else {
            return;
        };
        let seq = attachment.next_seq;
        attachment.next_seq += 1;

        let delivered = client.push_screen(ServerEvent::PaneScreen {
            session_id,
            pane_id,
            node_id: Some(node.clone()),
            seq,
            update,
        });
        if let Some(attachment) = client.attachments.get_mut(key) {
            attachment.owes_full_screen = !delivered;
        }
        if !delivered {
            tracing::debug!(
                client = %client_id, %node, seq,
                "a screen update was dropped; the next one will carry the whole screen"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::core::testing::Harness;
    use turn_core::ids::{PaneId, SessionId};
    use turn_proto::{Grid, PaneStream, PtySize, Request, Response, ServerEvent, ServerMessage};

    const NOW: i64 = 1_775_000_000_000;

    fn drain(
        frames: &mut tokio::sync::mpsc::Receiver<turn_proto::ServerFrame>,
    ) -> Vec<ServerEvent> {
        let mut out = Vec::new();
        while let Ok(frame) = frames.try_recv() {
            if let ServerMessage::Event { event } = frame.message {
                out.push(event);
            }
        }
        out
    }

    /// Every screen update this client was sent, applied in order, the way a real
    /// client applies them.
    fn apply_all(screen: &mut Grid, events: Vec<ServerEvent>) -> Vec<u64> {
        let mut seqs = Vec::new();
        for event in events {
            if let ServerEvent::PaneScreen { seq, update, .. } = event {
                update.apply(screen).expect("an update must apply cleanly");
                seqs.push(seq);
            }
        }
        seqs
    }

    /// A pane with a real process, attached as cells, and the client's own copy of the
    /// screen it was handed.
    async fn attached_pane(
        capacity: usize,
    ) -> (
        Harness,
        Grid,
        tokio::sync::mpsc::Receiver<turn_proto::ServerFrame>,
        SessionId,
        PaneId,
        turn_core::ids::NodeId,
        crate::core::ClientId,
    ) {
        let mut harness = Harness::new().await;
        let session = SessionId::from_stored("sess_cells001");
        let pane = PaneId::from_stored("pane_cells001");
        harness.add_session(session.clone(), pane.clone(), NOW);
        let node = harness.spawn_process(&session, &pane, NOW).await;

        let (client, mut frames) = harness.add_client(capacity);
        let response = harness
            .core
            .dispatch(
                client,
                Request::AttachPane {
                    session_id: session.clone(),
                    pane_id: pane.clone(),
                    size: PtySize::new(10, 40),
                    stream: PaneStream::Cells,
                },
                NOW,
            )
            .expect("attaching must succeed");
        let screen = match response {
            Response::Attached { attachment } => *attachment
                .screen
                .expect("a cells attachment carries the screen"),
            other => panic!("expected an attachment, got {other:?}"),
        };
        drain(&mut frames);
        (harness, screen, frames, session, pane, node, client)
    }

    /// The everyday path: a process prints, and the cells that arrive say what it
    /// printed — with the colour it asked for resolved to a concrete value.
    #[tokio::test]
    async fn what_a_process_prints_arrives_as_cells_with_its_colour_resolved() {
        let (mut harness, mut screen, mut frames, _session, _pane, node, _client) =
            attached_pane(64).await;

        harness.feed(&node, b"\x1b[31mred\x1b[0m plain\r\n").await;
        let seqs = apply_all(&mut screen, drain(&mut frames));
        assert_eq!(
            seqs,
            vec![0],
            "the first update starts the sequence at zero"
        );
        assert!(
            screen.text().contains("red plain"),
            "the cells must carry what the process printed: {:?}",
            screen.text()
        );

        let coloured = screen
            .cells
            .iter()
            .find(|cell| cell.text == "r")
            .expect("the first character of the coloured word");
        assert_eq!(
            coloured.fg,
            Some(turn_proto::indexed_rgb(1)),
            "a palette index must reach the client already resolved"
        );
        let uncoloured = screen
            .cells
            .iter()
            .find(|cell| cell.text == "p")
            .expect("a character after the reset");
        assert_eq!(
            uncoloured.fg, None,
            "and an unstyled cell must stay the theme's business"
        );
    }

    /// The point of diffing: a single line of output does not resend the screen.
    #[tokio::test]
    async fn a_line_of_output_sends_the_rows_that_changed_rather_than_the_screen() {
        let (mut harness, mut screen, mut frames, _session, _pane, node, _client) =
            attached_pane(64).await;

        harness.feed(&node, b"one\r\n").await;
        let events = drain(&mut frames);
        let update = events
            .iter()
            .find_map(|event| match event {
                ServerEvent::PaneScreen { update, .. } => Some(update.clone()),
                _ => None,
            })
            .expect("one update arrived");
        assert!(
            !update.is_full(),
            "a single line must not cost the whole screen: {update:?}"
        );
        assert!(update.row_count() <= 2, "got {} rows", update.row_count());
        apply_all(&mut screen, events);
        assert!(screen.text().contains("one"));
    }

    /// A pane nobody is looking at costs nothing. Its buffer is enough, and a later
    /// attach reads the screen out of it.
    #[tokio::test]
    async fn an_unattached_pane_produces_no_screen_updates_at_all() {
        let mut harness = Harness::new().await;
        let session = SessionId::from_stored("sess_quiet001");
        let pane = PaneId::from_stored("pane_quiet001");
        harness.add_session(session.clone(), pane.clone(), NOW);
        let node = harness.spawn_process(&session, &pane, NOW).await;

        // A client is connected but has attached to nothing.
        let (_client, mut frames) = harness.add_client(64);
        harness.feed(&node, b"nobody is watching\r\n").await;

        let events = drain(&mut frames);
        assert!(
            !events.iter().any(|event| matches!(
                event,
                ServerEvent::PaneScreen { .. } | ServerEvent::PaneOutput { .. }
            )),
            "an unwatched pane must not produce frames: {events:?}"
        );
        assert!(
            harness.core.screens.is_empty(),
            "and no screen is kept for it"
        );
    }

    /// The repair the daemon performs on its own. A client with no room loses an
    /// update; the next one is the whole screen, so it can never end up applying rows
    /// to a screen that is already wrong.
    #[tokio::test]
    async fn a_client_that_lost_an_update_is_sent_the_whole_screen_next_time() {
        // One frame of room, and the attach already consumed nothing — so the first
        // update fits and the second is lost while it is not draining.
        let (mut harness, mut screen, mut frames, _session, pane, node, client) =
            attached_pane(1).await;

        harness.feed(&node, b"first\r\n").await;
        harness.feed(&node, b"second\r\n").await;
        let behind = harness
            .core
            .clients
            .get(&client)
            .expect("the client")
            .attachments
            .values()
            .next()
            .expect("the attachment");
        assert!(
            behind.owes_full_screen,
            "a lost screen frame must be remembered"
        );

        // It starts draining again. Whatever it applies now must end up exactly right.
        let _ = drain(&mut frames);
        harness.feed(&node, b"third\r\n").await;
        let events = drain(&mut frames);
        let repaired = events
            .iter()
            .find_map(|event| match event {
                ServerEvent::PaneScreen { update, .. } => Some(update.clone()),
                _ => None,
            })
            .expect("an update arrived");
        assert!(
            repaired.is_full(),
            "the repair is the whole screen, not more rows: {repaired:?}"
        );
        repaired.apply(&mut screen).expect("it applies");
        for line in ["first", "second", "third"] {
            assert!(
                screen.text().contains(line),
                "{line:?} is missing from {:?}",
                screen.text()
            );
        }
        assert!(
            !harness
                .core
                .clients
                .get(&client)
                .expect("the client")
                .attachments[&(SessionId::from_stored("sess_cells001"), pane)]
                .owes_full_screen,
            "and the debt is cleared once the screen has been delivered"
        );
    }

    /// The client's own recovery path, for when it notices the gap before the pane
    /// produces anything else. The grid it gets back is the anchor the next diff is
    /// computed against, so applying that diff afterwards is exact.
    #[tokio::test]
    async fn a_client_can_ask_for_the_whole_screen_after_missing_an_update() {
        let (mut harness, _screen, mut frames, session, pane, node, client) =
            attached_pane(64).await;

        harness.feed(&node, b"printed before the gap\r\n").await;
        // The update is thrown away unread, as a client that fell over would.
        drain(&mut frames);

        let response = harness
            .core
            .dispatch(
                client,
                Request::ResyncPane {
                    session_id: session.clone(),
                    pane_id: pane.clone(),
                },
                NOW,
            )
            .expect("a resync must be answerable");
        let (mut recovered, next_seq) = match response {
            Response::Screen { grid, next_seq, .. } => (*grid, next_seq),
            other => panic!("expected a screen, got {other:?}"),
        };
        assert!(
            recovered.text().contains("printed before the gap"),
            "the recovered screen must hold what was missed: {:?}",
            recovered.text()
        );

        // And the sequence continues from what the resync reported, with the next diff
        // applying cleanly on top of it.
        harness.feed(&node, b"after the gap\r\n").await;
        let seqs = apply_all(&mut recovered, drain(&mut frames));
        assert_eq!(seqs, vec![next_seq]);
        assert!(recovered.text().contains("printed before the gap"));
        assert!(recovered.text().contains("after the gap"));
    }

    /// A resize has no row correspondence, so the screen is sent whole — including to
    /// a client whose own resize caused it, which otherwise would have no grid at its
    /// new geometry until the program happened to redraw.
    #[tokio::test]
    async fn a_resize_hands_every_watcher_a_whole_screen_at_the_new_geometry() {
        let (mut harness, mut screen, mut frames, session, _pane, node, client) =
            attached_pane(64).await;
        harness.feed(&node, b"before the resize\r\n").await;
        apply_all(&mut screen, drain(&mut frames));

        harness
            .core
            .dispatch(
                client,
                Request::ResizePty {
                    session_id: session,
                    node_id: node.clone(),
                    size: PtySize::new(20, 60),
                },
                NOW,
            )
            .expect("resizing must succeed");

        let events = drain(&mut frames);
        let update = events
            .iter()
            .find_map(|event| match event {
                ServerEvent::PaneScreen { update, .. } => Some(update.clone()),
                _ => None,
            })
            .expect("the resizing client is sent the new screen too");
        assert!(update.is_full(), "got {update:?}");
        assert_eq!(update.size(), PtySize::new(20, 60));
        update.apply(&mut screen).expect("it applies");
        assert_eq!((screen.rows, screen.cols), (20, 60));
    }

    /// A pane asked for as bytes still gets bytes, and never a grid. The escape stream
    /// is what a log capture or a client with its own emulator needs.
    #[tokio::test]
    async fn a_byte_attachment_gets_the_escape_stream_and_no_cells() {
        let mut harness = Harness::new().await;
        let session = SessionId::from_stored("sess_bytes001");
        let pane = PaneId::from_stored("pane_bytes001");
        harness.add_session(session.clone(), pane.clone(), NOW);
        let node = harness.spawn_process(&session, &pane, NOW).await;

        let (client, mut frames) = harness.add_client(64);
        let response = harness
            .core
            .dispatch(
                client,
                Request::AttachPane {
                    session_id: session.clone(),
                    pane_id: pane.clone(),
                    size: PtySize::new(10, 40),
                    stream: PaneStream::Bytes,
                },
                NOW,
            )
            .expect("attaching as bytes must succeed");
        match response {
            Response::Attached { attachment } => {
                assert!(attachment.screen.is_none(), "no grid was asked for");
                assert_eq!(attachment.stream, PaneStream::Bytes);
            }
            other => panic!("expected an attachment, got {other:?}"),
        }
        drain(&mut frames);

        harness.feed(&node, b"\x1b[32mgreen\x1b[0m\r\n").await;
        let mut bytes: Vec<u8> = Vec::new();
        for event in drain(&mut frames) {
            match event {
                ServerEvent::PaneOutput { data, .. } => bytes.extend(data.as_slice()),
                ServerEvent::PaneScreen { .. } => {
                    panic!("a byte attachment must not be sent cells")
                }
                _ => {}
            }
        }
        assert!(
            bytes.windows(5).any(|w| w == b"green"),
            "the raw stream must arrive as it was written"
        );
        assert!(
            bytes.windows(5).any(|w| w == b"\x1b[32m"),
            "including the escape sequences, which is the whole point of this stream"
        );
        assert!(
            harness.core.screens.is_empty(),
            "and no screen is built for a pane nobody renders as cells"
        );
    }

    /// The cap, enforced where a client would hit it: a geometry no terminal has is
    /// refused rather than turned into a grid nobody can send.
    #[tokio::test]
    async fn attaching_at_an_absurd_geometry_is_refused_rather_than_truncated() {
        let mut harness = Harness::new().await;
        let session = SessionId::from_stored("sess_huge0001");
        let pane = PaneId::from_stored("pane_huge0001");
        harness.add_session(session.clone(), pane.clone(), NOW);
        let (client, _frames) = harness.add_client(8);

        let error = harness
            .core
            .dispatch(
                client,
                Request::AttachPane {
                    session_id: session,
                    pane_id: pane,
                    size: PtySize::new(4_000, 4_000),
                    stream: PaneStream::Cells,
                },
                NOW,
            )
            .expect_err("sixteen million cells is not a screen");
        assert_eq!(error.code, turn_proto::ErrorCode::InvalidArgument);
        assert!(
            error.message.contains("too large"),
            "the message must say what is wrong: {}",
            error.message
        );
    }
}
