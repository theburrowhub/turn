//! Pane operations, including the one that makes process survival visible.

use super::sessions::pane_from_spec;
use super::workspaces::store;
use super::Answer;
use crate::core::clients::Attachment;
use crate::core::{ClientId, Core};
use turn_core::ids::{PaneId, SessionId};
use turn_core::model::{
    Direction, DropZone, LayoutPreset, NodeKind, Pane, PaneGeometry, PaneKind, PaneNodeBinding,
    PanePlacement,
};
use turn_proto::{
    CloseDisposition, ErrorCode, FocusTarget, NewPane, PaneAttachment, PaneStream, ProtoError,
    PtySize, Response, ServerEvent,
};
use turn_pty::ScreenSize;

pub(super) struct PaneDestination<'a> {
    pub target: &'a PaneId,
    pub placement: PanePlacement,
}

impl Core {
    pub(super) fn create_pane(
        &mut self,
        client: ClientId,
        session_id: &SessionId,
        target: &PaneId,
        placement: PanePlacement,
        spec: NewPane,
        now_ms: i64,
    ) -> Answer {
        if placement == PanePlacement::Temporary {
            return Err(ProtoError::invalid(
                "A new command Pane must be placed in the saved Layout",
            ));
        }
        self.validate_pane_definition_cwd(session_id, spec.cwd.as_deref())?;
        let pane = pane_from_spec(&spec);
        let new_pane = pane.id.clone();
        if self.session(session_id)?.layout.get(target).is_none() {
            return Err(ProtoError::not_found("pane", target.as_str()));
        }
        let replaced = {
            let session = self.session_mut(session_id)?;
            match placement {
                PanePlacement::ReplaceCurrent => session.layout.replace(target, pane),
                PanePlacement::SplitRight => {
                    if !session.layout.split(target, Direction::Horizontal, pane) {
                        return Err(ProtoError::new(
                            ErrorCode::Conflict,
                            "That Pane could not be opened to the right",
                        ));
                    }
                    None
                }
                PanePlacement::SplitBelow => {
                    if !session.layout.split(target, Direction::Vertical, pane) {
                        return Err(ProtoError::new(
                            ErrorCode::Conflict,
                            "That Pane could not be opened below",
                        ));
                    }
                    None
                }
                PanePlacement::Temporary => unreachable!("refused above"),
            }
        };
        if let Some(replaced) = replaced {
            self.detach_everyone(session_id, &replaced.id);
        }
        // `persist_session` synchronises the complete durable binding set in the same
        // SQLite transaction as the Layout. In particular, replacing a Pane cannot
        // leave a binding gap if persistence fails halfway through.
        self.persist_session(session_id)?;
        if let Err(error) = self.materialise_pane(session_id, &new_pane, now_ms) {
            tracing::warn!(%session_id, %new_pane, %error, "the new pane's process could not start");
        }
        self.persist_session(session_id)?;
        self.push_layout(session_id, Some(client));
        self.push_session_state(session_id, now_ms);
        self.answer_layout(session_id)
    }

    /// Opens a second, explicit view of an existing Process/Agent.
    ///
    /// This never materialises a Pane command: the runtime already exists and the
    /// Pane merely points at it. That separation is what guarantees that opening,
    /// replacing, promoting or later closing this view cannot restart or stop work.
    pub(super) fn open_node_as_pane(
        &mut self,
        client: ClientId,
        session_id: &SessionId,
        node_id: &turn_core::ids::NodeId,
        target: &PaneId,
        placement: PanePlacement,
        now_ms: i64,
    ) -> Answer {
        if placement == PanePlacement::Temporary {
            return Err(ProtoError::invalid(
                "Use open_node_as_temporary_pane for a surface-scoped Pane",
            ));
        }
        let node = self
            .session(session_id)?
            .tree
            .get(node_id)
            .cloned()
            .ok_or_else(|| ProtoError::not_found("process node", node_id.as_str()))?;
        if self.session(session_id)?.layout.get(target).is_none() {
            return Err(ProtoError::not_found("pane", target.as_str()));
        }

        let kind = pane_kind_for_node(node.kind, self.terminal_node(node_id).is_some());
        let mut pane = Pane::new(kind).with_title(node.resolved_title().0);
        pane.node_id = Some(node_id.clone());
        let pane_id = pane.id.clone();
        let replaced = {
            let session = self.session_mut(session_id)?;
            match placement {
                PanePlacement::ReplaceCurrent => session.layout.replace(target, pane),
                PanePlacement::SplitRight => {
                    if !session.layout.split(target, Direction::Horizontal, pane) {
                        return Err(ProtoError::new(
                            ErrorCode::Conflict,
                            "That Process view could not be opened to the right",
                        ));
                    }
                    None
                }
                PanePlacement::SplitBelow => {
                    if !session.layout.split(target, Direction::Vertical, pane) {
                        return Err(ProtoError::new(
                            ErrorCode::Conflict,
                            "That Process view could not be opened below",
                        ));
                    }
                    None
                }
                PanePlacement::Temporary => unreachable!("handled above"),
            }
        };

        if let Some(replaced) = replaced {
            self.detach_everyone(session_id, &replaced.id);
        }
        self.persist_session(session_id)?;
        self.store
            .hierarchy()
            .bind_pane(&PaneNodeBinding {
                pane_id,
                session_id: session_id.clone(),
                node_id: node_id.clone(),
                temporary: false,
                surface_id: None,
                opened_ms: now_ms,
            })
            .map_err(store)?;
        self.bump_hierarchy();
        self.push_layout(session_id, Some(client));
        self.push_pane_bindings(session_id, node_id, now_ms);
        self.answer_layout(session_id)
    }

    /// Turns the surface-scoped view into a durable Layout Pane in-place.
    pub(super) fn promote_temporary_pane(
        &mut self,
        client: ClientId,
        surface_id: &str,
        session_id: &SessionId,
        pane_id: &PaneId,
        destination: PaneDestination<'_>,
        now_ms: i64,
    ) -> Answer {
        let PaneDestination { target, placement } = destination;
        if placement == PanePlacement::Temporary {
            return Err(ProtoError::invalid(
                "A temporary Pane must be placed in the Layout to promote it",
            ));
        }
        let binding = self
            .store
            .hierarchy()
            .bindings_for_session(session_id)
            .map_err(store)?
            .into_iter()
            .find(|binding| {
                binding.pane_id == *pane_id
                    && binding.temporary
                    && binding.surface_id.as_deref() == Some(surface_id)
            })
            .ok_or_else(|| ProtoError::not_found("temporary pane", pane_id.as_str()))?;
        let node = self
            .session(session_id)?
            .tree
            .get(&binding.node_id)
            .cloned()
            .ok_or_else(|| ProtoError::not_found("process node", binding.node_id.as_str()))?;
        if self.session(session_id)?.layout.get(target).is_none() {
            return Err(ProtoError::not_found("pane", target.as_str()));
        }

        let mut pane = Pane::new(pane_kind_for_node(
            node.kind,
            self.terminal_node(&node.id).is_some(),
        ))
        .with_title(node.resolved_title().0);
        pane.id = pane_id.clone();
        pane.node_id = Some(binding.node_id.clone());
        let replaced = {
            let session = self.session_mut(session_id)?;
            match placement {
                PanePlacement::ReplaceCurrent => session.layout.replace(target, pane),
                PanePlacement::SplitRight => {
                    if !session.layout.split(target, Direction::Horizontal, pane) {
                        return Err(ProtoError::new(
                            ErrorCode::Conflict,
                            "That temporary Pane could not be promoted to the right",
                        ));
                    }
                    None
                }
                PanePlacement::SplitBelow => {
                    if !session.layout.split(target, Direction::Vertical, pane) {
                        return Err(ProtoError::new(
                            ErrorCode::Conflict,
                            "That temporary Pane could not be promoted below",
                        ));
                    }
                    None
                }
                PanePlacement::Temporary => unreachable!("refused above"),
            }
        };
        if let Some(replaced) = replaced {
            self.detach_everyone(session_id, &replaced.id);
        }
        self.persist_session(session_id)?;
        let mut durable = binding;
        durable.temporary = false;
        durable.surface_id = None;
        self.store.hierarchy().bind_pane(&durable).map_err(store)?;
        self.bump_hierarchy();
        self.push_layout(session_id, Some(client));
        self.push_pane_bindings(session_id, &durable.node_id, now_ms);
        self.answer_layout(session_id)
    }

    pub(super) fn duplicate_pane(
        &mut self,
        client: ClientId,
        session_id: &SessionId,
        pane_id: &PaneId,
        now_ms: i64,
    ) -> Answer {
        let node_id = self
            .session(session_id)?
            .layout
            .get(pane_id)
            .ok_or_else(|| ProtoError::not_found("pane", pane_id.as_str()))?
            .node_id
            .clone();
        let duplicate = self
            .session_mut(session_id)?
            .layout
            .duplicate(pane_id)
            .ok_or_else(|| {
                ProtoError::new(ErrorCode::Conflict, "That Pane could not be duplicated")
            })?;
        self.persist_session(session_id)?;
        if let Some(node_id) = node_id {
            self.store
                .hierarchy()
                .bind_pane(&PaneNodeBinding {
                    pane_id: duplicate,
                    session_id: session_id.clone(),
                    node_id: node_id.clone(),
                    temporary: false,
                    surface_id: None,
                    opened_ms: now_ms,
                })
                .map_err(store)?;
            self.bump_hierarchy();
            self.push_pane_bindings(session_id, &node_id, now_ms);
        }
        self.push_layout(session_id, Some(client));
        self.answer_layout(session_id)
    }

    pub(super) fn change_pane_kind(
        &mut self,
        client: ClientId,
        session_id: &SessionId,
        pane_id: &PaneId,
        kind: PaneKind,
    ) -> Answer {
        if !self
            .session_mut(session_id)?
            .layout
            .change_kind(pane_id, kind)
        {
            return Err(ProtoError::not_found("pane", pane_id.as_str()));
        }
        self.save_layout(session_id)?;
        self.push_layout(session_id, Some(client));
        self.answer_layout(session_id)
    }

    pub(super) fn float_pane(
        &mut self,
        client: ClientId,
        session_id: &SessionId,
        pane_id: &PaneId,
        geometry: PaneGeometry,
    ) -> Answer {
        if !geometry.is_valid() {
            return Err(ProtoError::invalid("Floating Pane geometry is not usable"));
        }
        if !self
            .session_mut(session_id)?
            .layout
            .float(pane_id, geometry)
        {
            return Err(ProtoError::new(
                ErrorCode::Conflict,
                "Keep at least one Pane docked in the Session",
            ));
        }
        self.save_layout(session_id)?;
        self.push_layout(session_id, Some(client));
        self.answer_layout(session_id)
    }

    pub(super) fn dock_pane(
        &mut self,
        client: ClientId,
        session_id: &SessionId,
        pane_id: &PaneId,
    ) -> Answer {
        if !self.session_mut(session_id)?.layout.dock(pane_id) {
            return Err(ProtoError::not_found("floating pane", pane_id.as_str()));
        }
        self.save_layout(session_id)?;
        self.push_layout(session_id, Some(client));
        self.answer_layout(session_id)
    }

    pub(super) fn set_floating_pane_geometry(
        &mut self,
        client: ClientId,
        session_id: &SessionId,
        pane_id: &PaneId,
        geometry: PaneGeometry,
    ) -> Answer {
        if !self
            .session_mut(session_id)?
            .layout
            .set_floating_geometry(pane_id, geometry)
        {
            return Err(ProtoError::invalid(
                "That floating Pane or its geometry is not usable",
            ));
        }
        self.save_layout(session_id)?;
        self.push_layout(session_id, Some(client));
        self.answer_layout(session_id)
    }

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
            // The pump belongs to whichever node owns the terminal this view was fed
            // from, which for a hosted agent is the shell it runs in.
            let watched = self
                .terminal_node(&binding.node_id)
                .unwrap_or_else(|| binding.node_id.clone());
            self.stop_pump_if_unwatched(&watched);
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
        // Either shape counts: the pane's own process being an agent, or the pane's shell
        // running one. Closing a view must not end an agent's work whichever of the two
        // it is, and the hosted shape is now the ordinary one.
        let closes_an_agent = node.as_ref().is_some_and(|node_id| {
            session
                .tree
                .get(node_id)
                .is_some_and(|node| node.kind.is_agentic())
                || self.hosted_agent_of(node_id).is_some_and(|hosted| {
                    session
                        .tree
                        .get(&hosted)
                        .is_some_and(|node| node.kind.is_agentic() && node.is_running())
                })
        });
        if disposition != CloseDisposition::KeepProcesses && closes_an_agent {
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
        self.store
            .hierarchy()
            .unbind_pane(session_id, pane_id)
            .map_err(store)?;
        if let Some(node_id) = node.as_ref() {
            self.bump_hierarchy();
            self.push_pane_bindings(session_id, node_id, now_ms);
        }
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

    /// Moves a pane next to another one, which is a Layout change and nothing else.
    ///
    /// No process is started, stopped or re-parented: the pane keeps its id and its
    /// node binding, so the runtime behind it never learns it was moved. That is what
    /// makes rearranging a session full of running agents a safe thing to do, and it
    /// is why only the Layout is written back.
    ///
    /// Also serves the older `swap_panes`, which is this operation with
    /// [`DropZone::Centre`].
    pub(super) fn relocate_pane(
        &mut self,
        client: ClientId,
        session_id: &SessionId,
        moved: &PaneId,
        target: &PaneId,
        zone: DropZone,
    ) -> Answer {
        let session = self.session_mut(session_id)?;
        for pane in [moved, target] {
            if session.layout.get(pane).is_none() {
                return Err(ProtoError::not_found("pane", pane.as_str()));
            }
        }
        if !session.layout.relocate(moved, target, zone) {
            return Err(ProtoError::new(
                ErrorCode::Conflict,
                "That pane could not be moved there",
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
            // A permanent Agent Pane binds to the semantic Agent identity even
            // when its screen belongs to the hosting shell. Resolve the terminal
            // at attach time, exactly as temporary panes already do.
            Some(pane) => pane.node_id.as_ref().map(|node| {
                // Hosted Agents resolve to their shell's PTY. A direct binding remains
                // direct even when its process is currently absent: attachments carry
                // identity as well as bytes, and dropping it here would make two
                // Sessions with the same PaneId indistinguishable to the client.
                self.terminal_node(node).unwrap_or_else(|| node.clone())
            }),
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
                // Resolved rather than taken literally: an agent hosted in a pane's
                // shell has a terminal — the shell's — and that is the screen this
                // attachment has to be fed from.
                match self.terminal_node(&binding.node_id) {
                    Some(node) => Some(node),
                    None => {
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
            } else if let Some(buffer) = self.recovered_terminals.get(node) {
                truncated = buffer.is_truncated();
                bytes_seen = buffer.bytes_seen();
            }
            // A recovered terminal is display-only. The node remains Orphaned/Lost;
            // retained output is never treated as proof that its process is alive.
        }

        let attached_size = node_id
            .as_ref()
            .map(|node| self.node_size(node, requested))
            .unwrap_or(requested);

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
        let mut scrollback = turn_proto::Scrollback::default();
        let mut replay = turn_proto::TerminalBytes::default();
        match (stream, &node_id) {
            (PaneStream::Cells, Some(node)) => {
                let grid = self.screen_for_attach(node, attached_size);
                let (history, history_truncated) = self.scrollback_for_attach(node);
                scrollback = history;
                truncated |= history_truncated;
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
                } else if let Some(buffer) = self.recovered_terminals.get(node) {
                    replay = turn_proto::TerminalBytes::new(buffer.replay());
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
                    size: attached_size,
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
                scrollback,
                replay,
                size: attached_size,
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

fn pane_kind_for_node(kind: NodeKind, has_terminal: bool) -> PaneKind {
    if !has_terminal {
        return PaneKind::ProcessDetails;
    }
    match kind {
        NodeKind::Agent | NodeKind::Subagent => PaneKind::Agent,
        NodeKind::Shell => PaneKind::Shell,
        NodeKind::Tui => PaneKind::Tui,
        NodeKind::Server => PaneKind::Server,
        NodeKind::TestRunner | NodeKind::Build => PaneKind::TestOutput,
        NodeKind::TmuxSession | NodeKind::TmuxPane => PaneKind::TmuxTerminal,
        NodeKind::Terminal | NodeKind::Watcher | NodeKind::Background | NodeKind::Unknown => {
            PaneKind::Terminal
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::testing::Harness;
    use turn_core::model::{LayoutNode, Pane, PaneKind, ProcessNode};
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

    /// A relocation is written through immediately, not left to the next flush.
    ///
    /// Asserted against the store rather than across a restart on purpose: a clean
    /// shutdown flushes every session anyway, so a restart would still show the new
    /// arrangement even if the request had persisted nothing — and the arrangement
    /// would then be lost by a daemon that died instead of exiting.
    #[tokio::test]
    async fn relocating_a_pane_writes_the_new_arrangement_through_to_the_store() {
        let mut harness = Harness::new().await;
        let session_id = SessionId::from_stored("sess_relocate_store");
        let left = PaneId::from_stored("pane_relocate_left");
        harness.add_session(session_id.clone(), left.clone(), 10);
        let right = Pane::new(PaneKind::Shell);
        let right_id = right.id.clone();
        assert!(harness
            .core
            .sessions
            .get_mut(&session_id)
            .unwrap()
            .layout
            .split(&left, Direction::Horizontal, right));

        let answer = harness
            .core
            .relocate_pane(ClientId(3), &session_id, &right_id, &left, DropZone::Below)
            .expect("a pane may be moved under its sibling");
        let returned = match answer {
            Response::Layout { layout, .. } => layout,
            other => panic!("expected the resulting layout, got {other:?}"),
        };
        let LayoutNode::Split(split) = &returned.root else {
            panic!("expected a split, got {:?}", returned.root);
        };
        assert_eq!(split.direction, Direction::Vertical);

        let stored = harness
            .core
            .store
            .sessions()
            .layout(&session_id)
            .expect("the store must answer")
            .expect("the session must have a stored layout");
        assert_eq!(
            stored, returned,
            "the client was shown an arrangement the daemon had not written down"
        );
        assert!(stored.sizes_are_normalised());
    }

    /// Moving a pane is a view change: the process behind it is not touched, so
    /// nothing about the node — least of all its pid — may change.
    #[tokio::test]
    async fn relocating_a_pane_leaves_its_process_exactly_where_it_was() {
        let mut harness = Harness::new().await;
        let session_id = SessionId::from_stored("sess_relocate_pids");
        let first = PaneId::from_stored("pane_relocate_first");
        harness.add_session(session_id.clone(), first.clone(), 10);
        let second = Pane::new(PaneKind::Shell);
        let second_id = second.id.clone();
        assert!(harness
            .core
            .sessions
            .get_mut(&session_id)
            .unwrap()
            .layout
            .split(&first, Direction::Horizontal, second));

        let mut node = ProcessNode::process(
            session_id.clone(),
            turn_core::model::NodeKind::Shell,
            "sh",
            "/tmp",
            10,
        );
        node.lifecycle = Lifecycle::Alive;
        node.pid = Some(31_337);
        let node_id = node.id.clone();
        {
            let session = harness.core.sessions.get_mut(&session_id).unwrap();
            session.tree.insert(node);
            session.layout.get_mut(&first).unwrap().node_id = Some(node_id.clone());
        }
        let nodes_before: Vec<(turn_core::ids::NodeId, Option<u32>, Lifecycle)> =
            harness.core.sessions[&session_id]
                .tree
                .iter()
                .map(|node| (node.id.clone(), node.pid, node.lifecycle.clone()))
                .collect();

        harness
            .core
            .relocate_pane(
                ClientId(3),
                &session_id,
                &first,
                &second_id,
                DropZone::Right,
            )
            .expect("a pane may be moved beside its sibling");

        let session = &harness.core.sessions[&session_id];
        let nodes_after: Vec<(turn_core::ids::NodeId, Option<u32>, Lifecycle)> = session
            .tree
            .iter()
            .map(|node| (node.id.clone(), node.pid, node.lifecycle.clone()))
            .collect();
        assert_eq!(
            nodes_after, nodes_before,
            "a relocation must not start, stop or re-identify a process"
        );
        assert_eq!(session.tree.get(&node_id).unwrap().pid, Some(31_337));
        assert_eq!(
            session.layout.get(&first).unwrap().node_id.as_ref(),
            Some(&node_id),
            "the moved pane kept showing the same process"
        );
        assert_eq!(session.layout.pane_count(), 2);
    }

    #[tokio::test]
    async fn permanent_open_duplicate_float_and_close_never_control_the_process() {
        let mut harness = Harness::new().await;
        let session_id = SessionId::from_stored("sess_permanent_node_panes");
        let target = PaneId::from_stored("pane_permanent_target");
        harness.add_session(session_id.clone(), target.clone(), 10);
        let mut agent =
            ProcessNode::process(session_id.clone(), NodeKind::Agent, "claude", "/tmp", 10);
        agent.lifecycle = Lifecycle::Alive;
        agent.pid = Some(54_321);
        let node_id = agent.id.clone();
        harness
            .core
            .sessions
            .get_mut(&session_id)
            .unwrap()
            .tree
            .insert(agent);

        let opened = harness
            .core
            .open_node_as_pane(
                ClientId(3),
                &session_id,
                &node_id,
                &target,
                PanePlacement::SplitRight,
                11,
            )
            .expect("an explicit permanent open succeeds");
        let layout = match opened {
            Response::Layout { layout, .. } => layout,
            other => panic!("expected Layout, got {other:?}"),
        };
        let opened_id = layout.active.clone().expect("new Pane is active");
        assert_eq!(
            layout.get(&opened_id).unwrap().node_id.as_ref(),
            Some(&node_id)
        );
        let binding = harness
            .core
            .store
            .hierarchy()
            .bindings_for_session(&session_id)
            .unwrap()
            .into_iter()
            .find(|binding| binding.pane_id == opened_id)
            .expect("durable binding persisted");
        assert!(!binding.temporary);

        harness
            .core
            .duplicate_pane(ClientId(3), &session_id, &opened_id, 12)
            .expect("a bound view duplicates");
        let duplicate = harness.core.sessions[&session_id]
            .layout
            .active
            .clone()
            .unwrap();
        assert_ne!(duplicate, opened_id);
        assert_eq!(
            harness.core.sessions[&session_id]
                .layout
                .get(&duplicate)
                .unwrap()
                .node_id
                .as_ref(),
            Some(&node_id)
        );
        let geometry = PaneGeometry {
            x: 60.0,
            y: 70.0,
            width: 640.0,
            height: 400.0,
        };
        harness
            .core
            .float_pane(ClientId(3), &session_id, &duplicate, geometry)
            .expect("the duplicate may float");
        let stored = harness
            .core
            .store
            .sessions()
            .layout(&session_id)
            .unwrap()
            .unwrap();
        assert_eq!(stored.floating_geometry(&duplicate), Some(geometry));

        harness
            .core
            .close_pane(
                ClientId(3),
                &session_id,
                &opened_id,
                CloseDisposition::KeepProcesses,
                13,
            )
            .expect("closing one view keeps the Process");
        let node = harness.core.sessions[&session_id]
            .tree
            .get(&node_id)
            .unwrap();
        assert_eq!(node.pid, Some(54_321));
        assert_eq!(node.lifecycle, Lifecycle::Alive);
    }

    #[tokio::test]
    async fn replacing_a_pane_removes_only_the_old_view() {
        let mut harness = Harness::new().await;
        let session_id = SessionId::from_stored("sess_replace_view_only");
        let target = PaneId::from_stored("pane_replace_view_only");
        harness.add_session(session_id.clone(), target.clone(), 10);
        let mut old_process = ProcessNode::process(
            session_id.clone(),
            NodeKind::Shell,
            "long-running-shell",
            "/tmp",
            10,
        );
        old_process.lifecycle = Lifecycle::Alive;
        old_process.pid = Some(44_444);
        let old_node_id = old_process.id.clone();
        let mut agent =
            ProcessNode::process(session_id.clone(), NodeKind::Agent, "claude", "/tmp", 10);
        agent.lifecycle = Lifecycle::Alive;
        agent.pid = Some(55_555);
        let agent_id = agent.id.clone();
        {
            let session = harness.core.sessions.get_mut(&session_id).unwrap();
            session.tree.insert(old_process);
            session.tree.insert(agent);
            session.layout.get_mut(&target).unwrap().node_id = Some(old_node_id.clone());
        }
        harness.core.persist_session(&session_id).unwrap();

        let response = harness
            .core
            .open_node_as_pane(
                ClientId(3),
                &session_id,
                &agent_id,
                &target,
                PanePlacement::ReplaceCurrent,
                11,
            )
            .expect("the visible Pane can be replaced");
        let layout = match response {
            Response::Layout { layout, .. } => layout,
            other => panic!("expected Layout, got {other:?}"),
        };

        assert_eq!(layout.pane_count(), 1);
        assert!(layout.get(&target).is_none(), "the old view was removed");
        assert_eq!(
            harness.core.sessions[&session_id]
                .tree
                .get(&old_node_id)
                .unwrap()
                .pid,
            Some(44_444),
            "replacing a view must not stop its Process"
        );
        assert_eq!(
            harness.core.sessions[&session_id]
                .tree
                .get(&agent_id)
                .unwrap()
                .pid,
            Some(55_555)
        );
    }

    #[tokio::test]
    async fn promoting_a_temporary_pane_keeps_its_identity_and_process() {
        let mut harness = Harness::new().await;
        let session_id = SessionId::from_stored("sess_promote_temporary");
        let target = PaneId::from_stored("pane_promote_target");
        harness.add_session(session_id.clone(), target.clone(), 10);
        let mut process = ProcessNode::process(
            session_id.clone(),
            NodeKind::Background,
            "worker",
            "/tmp",
            10,
        );
        process.lifecycle = Lifecycle::Alive;
        process.pid = Some(65_432);
        let node_id = process.id.clone();
        harness
            .core
            .sessions
            .get_mut(&session_id)
            .unwrap()
            .tree
            .insert(process);
        harness.core.persist_session(&session_id).unwrap();
        let temporary = harness
            .core
            .open_node_as_temporary_pane("main-window".into(), &session_id, &node_id, 11)
            .expect("temporary view opens");
        let pane_id = match temporary {
            Response::NodePane { pane } => pane.binding.pane_id,
            other => panic!("expected NodePane, got {other:?}"),
        };

        harness
            .core
            .promote_temporary_pane(
                ClientId(3),
                "main-window",
                &session_id,
                &pane_id,
                PaneDestination {
                    target: &target,
                    placement: PanePlacement::SplitBelow,
                },
                12,
            )
            .expect("the view promotes");
        let session = &harness.core.sessions[&session_id];
        assert_eq!(
            session.layout.get(&pane_id).unwrap().node_id.as_ref(),
            Some(&node_id)
        );
        let binding = harness
            .core
            .store
            .hierarchy()
            .bindings_for_session(&session_id)
            .unwrap()
            .into_iter()
            .find(|binding| binding.pane_id == pane_id)
            .unwrap();
        assert!(!binding.temporary);
        assert!(binding.surface_id.is_none());
        assert_eq!(session.tree.get(&node_id).unwrap().pid, Some(65_432));
        assert_eq!(
            session.tree.get(&node_id).unwrap().lifecycle,
            Lifecycle::Alive
        );
    }
}
