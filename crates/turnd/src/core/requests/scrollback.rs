//! Reading and searching a pane's scrollback.
//!
//! Both operations are reads of the parser the daemon already keeps. The scrollback is only
//! reachable by moving the parsed screen's own viewport, which every other reader of that
//! buffer shares, so both go through [`TerminalBuffer::with_history`] — it puts the viewport
//! back, on the panicking path as well as the normal one. Without that, one client's search
//! would leave every other client's pane rendering history as though it were live.
//!
//! Neither needs a lock beyond the buffer's own, and neither mutates daemon state: the
//! offset is borrowed and returned, so a search cannot move somebody else's screen.

use super::Answer;
use crate::core::command::ClientId;
use crate::core::Core;
use turn_core::ids::{NodeId, PaneId, SessionId};
use turn_proto::search::SearchQuery;
use turn_proto::{ErrorCode, Grid, ProtoError, PtySize, Response};

impl Core {
    /// A screen-shaped window of a pane's history, as cells.
    pub(super) fn pane_history(
        &mut self,
        client: ClientId,
        session_id: &SessionId,
        pane_id: &PaneId,
        offset: usize,
    ) -> Answer {
        let node_id = self.attached_cells_node(client, session_id, pane_id)?;
        // The geometry the screen is drawn at, so a pane with no live pty still gets a
        // window of the right shape to draw rather than nothing at all.
        let size = match &node_id {
            Some(node) => self.node_size(node, PtySize::default()),
            None => PtySize::default(),
        };
        let grid = match self.buffer_of(&node_id) {
            Some(shared) => match shared.lock() {
                Ok(mut buffer) => {
                    buffer.with_history(|screen| turn_proto::history_grid(screen, offset))
                }
                // Poisoned only if a pty reader thread panicked while holding it. The pane
                // is still real, so a blank window is the honest answer.
                Err(_) => Grid::blank(size.rows, size.cols),
            },
            None => Grid::blank(size.rows, size.cols),
        };
        Ok(Response::PaneHistory {
            session_id: session_id.clone(),
            pane_id: pane_id.clone(),
            node_id,
            grid: Box::new(grid),
        })
    }

    /// Everything the daemon retains for a pane, searched: the history, then the live screen.
    pub(super) fn search_pane(
        &mut self,
        client: ClientId,
        session_id: &SessionId,
        pane_id: &PaneId,
        query: &SearchQuery,
    ) -> Answer {
        let node_id = self.attached_cells_node(client, session_id, pane_id)?;
        let outcome = match self.buffer_of(&node_id) {
            Some(shared) => match shared.lock() {
                Ok(mut buffer) => buffer
                    .with_history(|screen| turn_proto::search_screen(screen, query))
                    // A pattern that will not compile is the user's to fix, and the message
                    // says what is wrong with it: "invalid regular expression" on its own is
                    // a dead end.
                    .map_err(|error| ProtoError::invalid(error.to_string()))?,
                Err(_) => turn_proto::search::SearchOutcome::default(),
            },
            None => turn_proto::search::SearchOutcome::default(),
        };
        Ok(Response::PaneMatches {
            session_id: session_id.clone(),
            pane_id: pane_id.clone(),
            node_id,
            outcome: Box::new(outcome),
        })
    }

    /// The node behind a pane this client is rendering as cells.
    ///
    /// Requires an attachment for the same reason `resync_pane` does: these answer questions
    /// about a screen the client is drawing, and a client that is not drawing it is asking
    /// about a pane it has no business reading.
    fn attached_cells_node(
        &self,
        client: ClientId,
        session_id: &SessionId,
        pane_id: &PaneId,
    ) -> Result<Option<NodeId>, ProtoError> {
        self.session(session_id)?;
        let key = (session_id.clone(), pane_id.clone());
        let attachment = self
            .clients
            .get(&client)
            .and_then(|client| client.attachments.get(&key))
            .ok_or_else(|| {
                ProtoError::new(
                    ErrorCode::PaneNotAttached,
                    "This connection is not attached to that pane",
                )
            })?;
        if attachment.stream.is_bytes() {
            return Err(ProtoError::new(
                ErrorCode::Conflict,
                "That pane is attached as a byte stream; its history is not cells",
            ));
        }
        Ok(attachment.node_id.clone())
    }

    /// The parsed buffer behind a node, when it has a live pty.
    fn buffer_of(
        &self,
        node_id: &Option<NodeId>,
    ) -> Option<std::sync::Arc<std::sync::Mutex<turn_pty::TerminalBuffer>>> {
        let node = node_id.as_ref()?;
        Some(self.processes.get(node)?.pty.buffer())
    }
}
