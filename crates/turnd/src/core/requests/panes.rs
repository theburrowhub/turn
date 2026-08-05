//! Pane operations, including the one that makes process survival visible.

use super::sessions::pane_from_spec;
use super::workspaces::store;
use super::Answer;
use crate::core::clients::Attachment;
use crate::core::{ClientId, Core};
use turn_core::ids::{PaneId, SessionId};
use turn_core::model::{Direction, LayoutPreset};
use turn_proto::{
    CloseDisposition, ErrorCode, FocusTarget, NewPane, PaneAttachment, PaneStream, ProtoError,
    PtySize, Response, ServerEvent,
};
use turn_pty::ScreenSize;

impl Core {
    pub(super) fn split_pane(
        &mut self,
        client: ClientId,
        session_id: &SessionId,
        pane_id: &PaneId,
        direction: Direction,
        spec: NewPane,
        now_ms: i64,
    ) -> Answer {
        // Reject an escaped cwd before the Layout is mutated or persisted. The
        // process-launch path repeats this check immediately before PTY spawn.
        self.validate_pane_definition_cwd(session_id, spec.cwd.as_deref())?;
        let pane = pane_from_spec(&spec);
        let new_pane = pane.id.clone();
        let session = self.session_mut(session_id)?;
        if session.layout.get(pane_id).is_none() {
            return Err(ProtoError::not_found("pane", pane_id.as_str()));
        }
        if !session.layout.split(pane_id, direction, pane) {
            return Err(ProtoError::new(
                ErrorCode::Conflict,
                "That pane could not be split",
            ));
        }
        self.persist_session(session_id)?;
        // A pane that describes a command starts it now; one that does not is a
        // placeholder until something is put in it.
        if let Err(error) = self.materialise_pane(session_id, &new_pane, now_ms) {
            tracing::warn!(%session_id, %new_pane, %error, "the new pane's process could not start");
        }
        self.persist_session(session_id)?;
        self.push_layout(session_id, Some(client));
        self.push_session_state(session_id, now_ms);
        self.answer_layout(session_id)
    }

    /// Closes a pane, doing what the disposition says with its process.
    pub(super) fn close_pane(
        &mut self,
        client: ClientId,
        session_id: &SessionId,
        pane_id: &PaneId,
        disposition: CloseDisposition,
        now_ms: i64,
    ) -> Answer {
        // Temporary panes are surface-scoped views, not Layout children. Closing
        // one removes only the view binding; stopping the Agent remains a
        // separate explicit node-control operation.
        if self.session(session_id)?.layout.get(pane_id).is_none() {
            let surface_id = self
                .clients
                .get(&client)
                .and_then(|client| client.surface_id.as_deref());
            let binding = self
                .store
                .hierarchy()
                .bindings_for_session(session_id)
                .map_err(store)?
                .into_iter()
                .find(|binding| {
                    binding.pane_id == *pane_id
                        && binding.temporary
                        && binding.surface_id.as_deref() == surface_id
                })
                .ok_or_else(|| ProtoError::not_found("pane", pane_id.as_str()))?;
            if disposition != CloseDisposition::KeepProcesses {
                return Err(ProtoError::refused(
                    "Closing an Agent view cannot stop its process; use Stop Agent explicitly",
                ));
            }
            self.detach_everyone(session_id, pane_id);
            self.store
                .hierarchy()
                .unbind_pane(session_id, pane_id)
                .map_err(store)?;
            self.stop_pump_if_unwatched(&binding.node_id);
            self.bump_hierarchy();
            self.push_pane_bindings(session_id, &binding.node_id, now_ms);
            return self.answer_layout(session_id);
        }

        let session = self.session(session_id)?;
        let pane = session
            .layout
            .get(pane_id)
            .ok_or_else(|| ProtoError::not_found("pane", pane_id.as_str()))?;
        let node = pane.node_id.clone();
        if disposition != CloseDisposition::KeepProcesses
            && node.as_ref().is_some_and(|node_id| {
                session
                    .tree
                    .get(node_id)
                    .is_some_and(|node| node.kind.is_agentic())
            })
        {
            return Err(ProtoError::refused(
                "Closing an Agent pane cannot stop its process; use Stop Agent explicitly",
            ));
        }
        if session.layout.pane_count() <= 1 {
            return Err(ProtoError::new(
                ErrorCode::Conflict,
                "A session must keep at least one pane",
            ));
        }
        if let Some(node_id) = node.as_ref() {
            self.ensure_node_process_stoppable(session_id, node_id, disposition)?;
        }

        self.detach_everyone(session_id, pane_id);

        match (&node, disposition) {
            (Some(node), CloseDisposition::KeepProcesses) => {
                // The process stays, and stays visible: a node with no pane is how a
                // background process keeps its place in the tree.
                self.stop_pump_if_unwatched(node);
            }
            // The pane is going away, so the pty goes with it. That is what closing a
            // terminal does, and it is the only thing that reliably ends a program which
            // ignores a polite signal.
            (Some(node), CloseDisposition::Terminate) => {
                self.stop_and_release(session_id, node, false, now_ms)
            }
            (Some(node), CloseDisposition::Kill) => {
                self.stop_and_release(session_id, node, true, now_ms)
            }
            (None, _) => {}
        }

        let session = self.session_mut(session_id)?;
        if !session.layout.close(pane_id) {
            return Err(ProtoError::new(
                ErrorCode::Conflict,
                "That pane could not be closed",
            ));
        }
        let restore_update = self.resolve_restore_pane(session_id, pane_id);
        self.persist_session(session_id)?;
        if let Some(update) = restore_update {
            self.push_all(update);
        }
        self.push_layout(session_id, Some(client));
        self.push_session_state(session_id, now_ms);
        self.answer_layout(session_id)
    }

    pub(super) fn resize_pane(
        &mut self,
        client: ClientId,
        session_id: &SessionId,
        pane_id: &PaneId,
        delta: f32,
    ) -> Answer {
        // A NaN would propagate into every sibling's size and make the layout
        // unrenderable — and it round-trips through JSON as `null`, so it is worth
        // refusing explicitly rather than trusting the client.
        if !delta.is_finite() {
            return Err(ProtoError::invalid(
                "A resize delta must be a finite number",
            ));
        }
        if delta.abs() > 1.0 {
            return Err(ProtoError::invalid(
                "A resize delta is a fraction of the split, so it cannot exceed 1",
            ));
        }
        let session = self.session_mut(session_id)?;
        if session.layout.get(pane_id).is_none() {
            return Err(ProtoError::not_found("pane", pane_id.as_str()));
        }
        // A single-pane layout has nothing to take space from; the clamp inside
        // `resize` reports that by returning false, and it is not an error.
        session.layout.resize(pane_id, delta);
        self.save_layout(session_id)?;
        self.push_layout(session_id, Some(client));
        self.answer_layout(session_id)
    }

    pub(super) fn resize_divider(
        &mut self,
        client: ClientId,
        session_id: &SessionId,
        before: &PaneId,
        after: &PaneId,
        delta: f32,
    ) -> Answer {
        validate_resize_delta(delta)?;
        let session = self.session_mut(session_id)?;
        for pane in [before, after] {
            if session.layout.get(pane).is_none() {
                return Err(ProtoError::not_found("pane", pane.as_str()));
            }
        }
        if !session.layout.resize_divider(before, after, delta) {
            return Err(ProtoError::new(
                ErrorCode::Conflict,
                "Those panes do not identify one divider",
            ));
        }
        self.save_layout(session_id)?;
        self.push_layout(session_id, Some(client));
        self.answer_layout(session_id)
    }

    pub(super) fn equalize_divider(
        &mut self,
        client: ClientId,
        session_id: &SessionId,
        before: &PaneId,
        after: &PaneId,
    ) -> Answer {
        let session = self.session_mut(session_id)?;
        for pane in [before, after] {
            if session.layout.get(pane).is_none() {
                return Err(ProtoError::not_found("pane", pane.as_str()));
            }
        }
        if !session.layout.equalize_divider(before, after) {
            return Err(ProtoError::new(
                ErrorCode::Conflict,
                "Those panes do not identify one divider",
            ));
        }
        self.save_layout(session_id)?;
        self.push_layout(session_id, Some(client));
        self.answer_layout(session_id)
    }

    pub(super) fn apply_layout_preset(
        &mut self,
        client: ClientId,
        session_id: &SessionId,
        preset: LayoutPreset,
    ) -> Answer {
        let session = self.session_mut(session_id)?;
        if !session.layout.apply_preset(preset) {
            return Err(ProtoError::new(
                ErrorCode::Conflict,
                "The current panes cannot use that layout",
            ));
        }
        self.save_layout(session_id)?;
        self.push_layout(session_id, Some(client));
        self.answer_layout(session_id)
    }

    pub(super) fn focus_pane(
        &mut self,
        client: ClientId,
        session_id: &SessionId,
        target: FocusTarget,
    ) -> Answer {
        let session = self.session_mut(session_id)?;
        let moved = match &target {
            FocusTarget::Pane { pane_id } => session.layout.focus(pane_id),
            FocusTarget::Next => session.layout.focus_next().is_some(),
            FocusTarget::Previous => session.layout.focus_previous().is_some(),
        };
        if !moved {
            if let FocusTarget::Pane { pane_id } = &target {
                return Err(ProtoError::not_found("pane", pane_id.as_str()));
            }
            return Err(ProtoError::new(
                ErrorCode::Conflict,
                "There is no other pane to focus",
            ));
        }
        self.save_layout(session_id)?;
        self.push_layout(session_id, Some(client));
        self.answer_layout(session_id)
    }

    pub(super) fn swap_panes(
        &mut self,
        client: ClientId,
        session_id: &SessionId,
        a: &PaneId,
        b: &PaneId,
    ) -> Answer {
        let session = self.session_mut(session_id)?;
        for pane in [a, b] {
            if session.layout.get(pane).is_none() {
                return Err(ProtoError::not_found("pane", pane.as_str()));
            }
        }
        if !session.layout.swap(a, b) {
            return Err(ProtoError::new(
                ErrorCode::Conflict,
                "Those panes could not be swapped",
            ));
        }
        self.save_layout(session_id)?;
        self.push_layout(session_id, Some(client));
        self.answer_layout(session_id)
    }

    pub(super) fn zoom_pane(
        &mut self,
        client: ClientId,
        session_id: &SessionId,
        pane_id: &PaneId,
    ) -> Answer {
        let session = self.session_mut(session_id)?;
        if !session.layout.toggle_zoom(pane_id) {
            return Err(ProtoError::not_found("pane", pane_id.as_str()));
        }
        self.save_layout(session_id)?;
        self.push_layout(session_id, Some(client));
        self.answer_layout(session_id)
    }

    /// Subscribes a client to a pane and hands back what rebuilds it.
    ///
    /// This is the request the whole daemon exists for. The pty has been running all
    /// along; attaching applies the client's geometry and hands over the screen the
    /// daemon has been keeping. A UI that restarted looks exactly as it did, because
    /// nothing about the terminal ever belonged to the window.
    ///
    /// Cells by default, because the daemon has already parsed the screen and a
    /// renderer without its own terminal emulator can draw them directly. Bytes on
    /// request, for the things that genuinely need the stream itself.
    pub(super) fn attach_pane(
        &mut self,
        client: ClientId,
        session_id: &SessionId,
        pane_id: &PaneId,
        size: PtySize,
        stream: PaneStream,
    ) -> Answer {
        let requested = PtySize::new(size.rows, size.cols);
        // Refused rather than clamped: a client asking to render a screen this size has
        // a layout bug, and silently drawing it something else would hide it. The limit
        // is announced in `welcome`, so it is one the client could have checked.
        let cells = requested.rows as usize * requested.cols as usize;
        if cells > turn_proto::MAX_SCREEN_CELLS {
            return Err(ProtoError::invalid(format!(
                "A screen of {}x{} is {cells} cells, which is too large: the limit is {}",
                requested.rows,
                requested.cols,
                turn_proto::MAX_SCREEN_CELLS
            )));
        }

        let session = self.session(session_id)?;
        let node_id = match session.layout.get(pane_id) {
            Some(pane) => pane.node_id.clone(),
            None => {
                let surface_id = self
                    .clients
                    .get(&client)
                    .and_then(|client| client.surface_id.as_deref());
                let binding = self
                    .store
                    .hierarchy()
                    .bindings_for_session(session_id)
                    .map_err(store)?
                    .into_iter()
                    .find(|binding| {
                        binding.pane_id == *pane_id
                            && binding.temporary
                            && binding.surface_id.as_deref() == surface_id
                    })
                    .ok_or_else(|| ProtoError::not_found("pane", pane_id.as_str()))?;
                match self.node_pane_capability(&binding.node_id) {
                    turn_proto::NodePaneCapability::Terminal { .. } => Some(binding.node_id),
                    turn_proto::NodePaneCapability::PreviewDetails => {
                        return Err(ProtoError::new(
                            ErrorCode::Conflict,
                            "This Agent has semantic preview details but no attachable terminal",
                        ));
                    }
                }
            }
        };

        let mut truncated = false;
        let mut bytes_seen = 0u64;
        let mut resized = false;

        if let Some(node) = &node_id {
            // Subscribed *before* the screen is taken. The subscription happens the
            // moment this is called, not when the pump task first runs, so nothing the
            // process writes in between falls between the two. A row that arrives twice
            // — once in the screen and once in the next update — is idempotent, where a
            // row that never arrives is a pane that disagrees with the process.
            self.start_pump(node);

            if let Some(process) = self.processes.get_mut(node) {
                let screen = ScreenSize::new(requested.rows, requested.cols);
                // Applied before the screen is taken, so what comes back matches the
                // geometry the client is about to render it at.
                resized = process.size != screen;
                if let Err(error) = process.pty.resize(screen) {
                    tracing::warn!(%node, %error, "could not resize a pty on attach");
                }
                process.size = screen;
                if let Ok(buffer) = process.pty.buffer().lock() {
                    truncated = buffer.is_truncated();
                    bytes_seen = buffer.bytes_seen();
                }
            }
            // A node with no live pty — orphaned or lost after a restart — attaches
            // with an empty screen rather than being refused. The pane is real, its
            // state says what happened to it, and the relaunch offer is the way back.
        }

        let attachment = Attachment {
            node_id: node_id.clone(),
            stream,
            next_seq: 0,
            owed_gap: 0,
            owes_full_screen: false,
        };
        let client_entry = self
            .clients
            .get_mut(&client)
            .ok_or_else(|| ProtoError::internal("this connection is not registered"))?;
        // Keyed by session as well as pane: two sessions that share a pane id must not
        // share an attachment.
        client_entry
            .attachments
            .insert((session_id.clone(), pane_id.clone()), attachment);

        // Exactly one of the two payloads, decided by what the client asked for.
        let mut screen = None;
        let mut replay = turn_proto::TerminalBytes::default();
        match (stream, &node_id) {
            (PaneStream::Cells, Some(node)) => {
                let grid = self.screen_for_attach(node, requested);
                if resized {
                    // The geometry moved, so every other client's baseline is the wrong
                    // shape and a row diff against it would be meaningless.
                    self.push_full_screen(node, Some(client));
                }
                screen = Some(Box::new(grid));
            }
            // A pane with no process still gets a screen: a blank one at the client's
            // size is something to draw, where nothing at all is a renderer with a hole
            // in it.
            (PaneStream::Cells, None) => {
                screen = Some(Box::new(turn_proto::Grid::blank(
                    requested.rows,
                    requested.cols,
                )));
            }
            (PaneStream::Bytes, Some(node)) => {
                if let Some(process) = self.processes.get(node) {
                    // The parsed screen re-emitted, not the raw ring: a truncated ring
                    // can begin mid-escape-sequence and corrupt the receiving terminal.
                    replay = turn_proto::TerminalBytes::new(process.pty.replay());
                }
            }
            (PaneStream::Bytes, None) => {}
        }

        if let Some(node) = &node_id {
            self.push_others(
                client,
                ServerEvent::PtyResized {
                    session_id: session_id.clone(),
                    node_id: node.clone(),
                    size: requested,
                },
            );
        }

        Ok(Response::Attached {
            attachment: Box::new(PaneAttachment {
                session_id: session_id.clone(),
                pane_id: pane_id.clone(),
                node_id,
                stream,
                screen,
                replay,
                size: requested,
                scrollback_truncated: truncated,
                bytes_seen,
                next_seq: 0,
            }),
        })
    }

    /// Hands a client the whole screen again, after it missed an update.
    ///
    /// Answers with the daemon's own baseline rather than with a screen read fresh from
    /// the pty, and that is the point: the baseline is the exact grid the next diff will
    /// be computed against, so a row that changed and changed back in between cannot
    /// leave the client holding a value the following update will never correct.
    pub(super) fn resync_pane(
        &mut self,
        client: ClientId,
        session_id: &SessionId,
        pane_id: &PaneId,
    ) -> Answer {
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
            // There is no honest cells answer for a byte attachment: what it lost was
            // bytes, and the way back is to attach again and take a replay.
            return Err(ProtoError::new(
                ErrorCode::Conflict,
                "That pane is attached as a byte stream; attach again to replay it",
            ));
        }
        let node_id = attachment.node_id.clone();
        let next_seq = attachment.next_seq;
        let size = self.node_size_for(&node_id, PtySize::default());

        let grid = match &node_id {
            Some(node) => self.screen_for_attach(node, size),
            None => turn_proto::Grid::blank(size.rows, size.cols),
        };
        // Whatever it was owed has now been said in full.
        if let Some(attachment) = self
            .clients
            .get_mut(&client)
            .and_then(|client| client.attachments.get_mut(&key))
        {
            attachment.owes_full_screen = false;
        }

        Ok(Response::Screen {
            session_id: session_id.clone(),
            pane_id: pane_id.clone(),
            node_id,
            next_seq,
            grid: Box::new(grid),
        })
    }

    /// The geometry a possibly-absent node is drawn at.
    fn node_size_for(&self, node: &Option<turn_core::ids::NodeId>, fallback: PtySize) -> PtySize {
        match node {
            Some(node) => self.node_size(node, fallback),
            None => fallback,
        }
    }

    /// Stops a client's output stream for a pane. The process keeps running.
    pub(super) fn detach_pane(
        &mut self,
        client: ClientId,
        session_id: &SessionId,
        pane_id: &PaneId,
    ) -> Answer {
        self.session(session_id)?;
        let key = (session_id.clone(), pane_id.clone());
        let removed = self
            .clients
            .get_mut(&client)
            .and_then(|client| client.attachments.remove(&key));
        let Some(attachment) = removed else {
            return Err(ProtoError::new(
                ErrorCode::PaneNotAttached,
                "This connection is not attached to that pane",
            ));
        };
        if let Some(node) = attachment.node_id {
            self.stop_pump_if_unwatched(&node);
        }
        Ok(Response::Ack)
    }

    /// Every pane operation answers with the layout it produced, so the UI renders the
    /// daemon's arrangement rather than its own guess at what a clamped resize did.
    fn answer_layout(&self, session_id: &SessionId) -> Answer {
        let session = self.session(session_id)?;
        Ok(Response::Layout {
            session_id: session_id.clone(),
            layout: session.layout.clone(),
        })
    }

    /// Writes just the layout. Splitting and resizing happen constantly and have
    /// nothing to do with the row the sidebar reads.
    fn save_layout(&self, session_id: &SessionId) -> std::result::Result<(), ProtoError> {
        let session = self.session(session_id)?;
        self.store
            .sessions()
            .save_layout(session_id, &session.layout, session.last_activity_ms)
            .map_err(store)
    }
}

fn validate_resize_delta(delta: f32) -> Result<(), ProtoError> {
    if !delta.is_finite() {
        return Err(ProtoError::invalid(
            "A resize delta must be a finite number",
        ));
    }
    if delta.abs() > 1.0 {
        return Err(ProtoError::invalid(
            "A resize delta is a fraction of the split, so it cannot exceed 1",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::testing::Harness;
    use turn_core::model::{Pane, PaneKind, ProcessNode};
    use turn_core::state::Lifecycle;

    #[tokio::test]
    async fn closing_a_pane_never_fabricates_an_orphans_death() {
        let mut harness = Harness::new().await;
        let session_id = SessionId::from_stored("sess_orphan_pane_guard");
        let pane_id = PaneId::from_stored("pane_orphan_pane_guard");
        harness.add_session(session_id.clone(), pane_id.clone(), 10);
        let second = Pane::new(PaneKind::Shell);
        assert!(harness
            .core
            .sessions
            .get_mut(&session_id)
            .unwrap()
            .layout
            .split(&pane_id, Direction::Horizontal, second));

        let mut orphan = ProcessNode::process(
            session_id.clone(),
            turn_core::model::NodeKind::Shell,
            "sh",
            "/tmp",
            10,
        );
        orphan.lifecycle = Lifecycle::Orphaned;
        orphan.pid = Some(424_242);
        let orphan_id = orphan.id.clone();
        {
            let session = harness.core.sessions.get_mut(&session_id).unwrap();
            session.tree.insert(orphan);
            session.layout.get_mut(&pane_id).unwrap().node_id = Some(orphan_id.clone());
        }

        let error = harness
            .core
            .close_pane(
                ClientId(7),
                &session_id,
                &pane_id,
                CloseDisposition::Terminate,
                11,
            )
            .expect_err("an unreachable PTY cannot be claimed as stopped");
        assert_eq!(error.code, ErrorCode::Conflict);
        let session = &harness.core.sessions[&session_id];
        assert_eq!(session.layout.pane_count(), 2);
        assert_eq!(
            session.tree.get(&orphan_id).unwrap().lifecycle,
            Lifecycle::Orphaned
        );
    }
}
