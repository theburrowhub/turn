//! The request surface: every protocol operation.
//!
//! Split by subsystem, dispatched from one place so the daemon's whole surface is
//! visible in a single match — the same reason [`turn_proto::Request`] is one flat
//! enum. Every handler is synchronous and runs inside the core task, so a handler
//! reads and writes state without a lock and without the possibility of another
//! request interleaving halfway through.

mod attention;
mod handoff;
mod hierarchy;
mod nodes;
mod panes;
mod scrollback;
mod sessions;
mod workspaces;

use super::command::ClientId;
use super::Core;
use turn_core::model::DropZone;
use turn_proto::{ProtoError, Request, Response};

/// The result every handler returns: a typed response, or the one error shape.
pub type Answer = std::result::Result<Response, ProtoError>;

impl Core {
    /// Routes one request to its handler.
    pub(crate) fn dispatch(&mut self, client: ClientId, request: Request, now_ms: i64) -> Answer {
        // A runtime checkpoint mutates the in-memory Session before SQLite commits
        // it. Let that oldest fact recover first, then refuse every request if the
        // durable barrier is still closed. A read would otherwise expose a
        // projection no client push was allowed to announce; a write could mutate a
        // Layout/name/lease and hitch that rejected action onto the checkpoint's
        // later retry.
        self.retry_failed_ingest_checkpoints(now_ms);
        if !self.failed_ingest_checkpoints.is_empty() {
            tracing::warn!(
                operation = request.op(),
                pending = self.failed_ingest_checkpoints.len(),
                "refused a request behind a failed atomic runtime checkpoint"
            );
            return Err(ProtoError::new(
                turn_proto::ErrorCode::Unavailable,
                "Turn is waiting for an earlier runtime event to reach durable storage",
            ));
        }
        match request {
            // ---------------------------------------------------------- workspaces
            Request::ListWorkspaces { include_archived } => {
                self.list_workspaces(include_archived, now_ms)
            }
            Request::CreateWorkspace { name, root } => self.create_workspace(name, root, now_ms),
            Request::RenameWorkspace { workspace_id, name } => {
                self.rename_workspace(&workspace_id, name, now_ms)
            }
            Request::ArchiveWorkspace {
                workspace_id,
                archived,
            } => self.archive_workspace(&workspace_id, archived, now_ms),
            Request::DuplicateWorkspace { workspace_id, name } => {
                self.duplicate_workspace(&workspace_id, name, now_ms)
            }
            Request::CloseWorkspace {
                workspace_id,
                disposition,
            } => self.close_workspace(&workspace_id, disposition, now_ms),

            // ------------------------------------------------------- unified tree
            Request::GetHierarchy {
                surface_id,
                include_archived,
            } => self.get_hierarchy_for_client(client, surface_id, include_archived, now_ms),
            Request::SetTreeExpanded {
                surface_id,
                key,
                expanded,
            } => {
                self.require_client_surface(client, &surface_id)?;
                self.set_tree_expanded(surface_id, key, expanded, now_ms)
            }
            Request::SelectTreeNode {
                surface_id,
                selected,
            } => {
                self.require_client_surface(client, &surface_id)?;
                self.select_tree_node(surface_id, selected, now_ms)
            }

            // ----------------------------------------------------- checkout lease
            Request::GetWorkspaceWriteLease { workspace_id } => {
                self.workspace_write_lease(&workspace_id)
            }
            Request::AcquireWorkspaceWriteLease {
                workspace_id,
                session_id,
                checkout_id,
            } => {
                self.acquire_workspace_write_lease(&workspace_id, &session_id, &checkout_id, now_ms)
            }
            Request::ReleaseWorkspaceWriteLease {
                workspace_id,
                lease_id,
                expected_generation,
            } => self.release_workspace_write_lease(
                &workspace_id,
                &lease_id,
                expected_generation,
                now_ms,
            ),

            // ------------------------------------------------------------ sessions
            Request::ListSessions {
                workspace_id,
                include_archived,
            } => Ok(Response::Sessions {
                sessions: self.session_summaries(workspace_id.as_ref(), include_archived, now_ms),
            }),
            Request::CreateSession {
                workspace_id,
                name,
                cwd,
                panes,
                note,
                tags,
            } => self.create_session(&workspace_id, name, cwd, panes, note, tags, now_ms),
            Request::CreateReadOnlySession {
                workspace_id,
                name,
                cwd,
                panes,
                note,
                tags,
            } => self.create_read_only_session(&workspace_id, name, cwd, panes, note, tags, now_ms),
            Request::CreateReadOnlySessionFromTemplate {
                workspace_id,
                template_id,
                name,
                cwd,
                branch,
                task,
            } => self.create_read_only_session_from_template(
                &workspace_id,
                &template_id,
                name,
                cwd,
                branch,
                task,
                now_ms,
            ),
            Request::CreateWorktreeSession {
                workspace_id,
                name,
                branch,
                worktree_path,
                panes,
                note,
                tags,
            } => self.create_worktree_session(
                &workspace_id,
                name,
                branch,
                worktree_path,
                panes,
                note,
                tags,
                now_ms,
            ),
            Request::CreateWorktreeSessionFromTemplate {
                workspace_id,
                template_id,
                name,
                cwd,
                template_branch,
                task,
                branch,
                worktree_path,
            } => self.create_worktree_session_from_template(
                &workspace_id,
                &template_id,
                name,
                cwd,
                template_branch,
                task,
                branch,
                worktree_path,
                now_ms,
            ),
            Request::CreateSessionFromTemplate {
                workspace_id,
                template_id,
                name,
                cwd,
                branch,
                task,
            } => self.create_session_from_template(
                &workspace_id,
                &template_id,
                name,
                cwd,
                branch,
                task,
                now_ms,
            ),
            Request::RenameSession { session_id, name } => {
                self.rename_session(&session_id, name, now_ms)
            }
            Request::ArchiveSession {
                session_id,
                archived,
            } => self.archive_session(&session_id, archived, now_ms),
            Request::DuplicateSession { session_id } => self.duplicate_session(&session_id, now_ms),
            Request::CloseSession {
                session_id,
                disposition,
            } => self.close_session(&session_id, disposition, now_ms),
            Request::GetSession { session_id } => {
                let details = self
                    .session_details(&session_id, now_ms)
                    .ok_or_else(|| ProtoError::not_found("session", session_id.as_str()))?;
                Ok(Response::SessionDetails {
                    details: Box::new(details),
                })
            }
            Request::GetProcessTree { session_id } => {
                self.session(&session_id)?;
                Ok(Response::Tree {
                    nodes: self.tree_views(&session_id, now_ms),
                    session_id,
                })
            }
            Request::GetPreviewHistory {
                session_id,
                node_id,
                limit,
            } => self.get_preview_history(&session_id, &node_id, limit),
            Request::SetPreviewVisibility {
                session_id,
                node_id,
                visibility,
            } => self.set_preview_visibility(&session_id, &node_id, visibility, now_ms),
            Request::PrepareContextHandoff {
                session_id,
                source_node_id,
                target_node_id,
                instruction,
            } => self.prepare_context_handoff(
                client,
                &session_id,
                &source_node_id,
                &target_node_id,
                instruction.as_ref(),
                now_ms,
            ),
            Request::DeliverContextHandoff {
                session_id,
                handoff_id,
            } => self.deliver_context_handoff(client, &session_id, &handoff_id, now_ms),

            // ----------------------------------------------------------- templates
            Request::ListTemplates => Ok(Response::Templates {
                templates: self.template_summaries(),
            }),
            Request::CreateLayoutTemplate {
                name,
                layout,
                description,
            } => self.create_layout_template(name, *layout, description, now_ms),
            Request::SaveLayoutAsTemplate {
                session_id,
                name,
                description,
                hotkey,
            } => self.save_layout_as_template(&session_id, name, description, hotkey, now_ms),

            // --------------------------------------------------------------- panes
            Request::SplitPane {
                session_id,
                pane_id,
                direction,
                pane,
            } => self.split_pane(client, &session_id, &pane_id, direction, pane, now_ms),
            Request::ClosePane {
                session_id,
                pane_id,
                disposition,
            } => self.close_pane(client, &session_id, &pane_id, disposition, now_ms),
            Request::ResizePane {
                session_id,
                pane_id,
                delta,
            } => self.resize_pane(client, &session_id, &pane_id, delta),
            Request::ResizeDivider {
                session_id,
                before,
                after,
                delta,
            } => self.resize_divider(client, &session_id, &before, &after, delta),
            Request::EqualizeDivider {
                session_id,
                before,
                after,
            } => self.equalize_divider(client, &session_id, &before, &after),
            Request::ApplyLayoutPreset { session_id, preset } => {
                self.apply_layout_preset(client, &session_id, preset)
            }
            Request::FocusPane { session_id, target } => {
                self.focus_pane(client, &session_id, target)
            }
            Request::RelocatePane {
                session_id,
                moved,
                target,
                zone,
            } => self.relocate_pane(client, &session_id, &moved, &target, zone),
            // The older spelling of a `centre` relocation, served by the same code so
            // the two cannot drift apart.
            Request::SwapPanes { session_id, a, b } => {
                self.relocate_pane(client, &session_id, &a, &b, DropZone::Centre)
            }
            Request::ZoomPane {
                session_id,
                pane_id,
            } => self.zoom_pane(client, &session_id, &pane_id),
            Request::OpenNodeAsTemporaryPane {
                surface_id,
                session_id,
                node_id,
            } => {
                self.require_client_surface(client, &surface_id)?;
                self.open_node_as_temporary_pane(surface_id, &session_id, &node_id, now_ms)
            }
            Request::FocusPaneForNode {
                surface_id,
                session_id,
                node_id,
            } => {
                self.require_client_surface(client, &surface_id)?;
                self.focus_pane_for_node(surface_id, &session_id, &node_id)
            }
            Request::FocusPaneForAttention {
                surface_id,
                session_id,
                subject_node_id,
            } => {
                self.require_client_surface(client, &surface_id)?;
                self.focus_pane_for_attention(surface_id, &session_id, &subject_node_id)
            }
            Request::AttachPane {
                session_id,
                pane_id,
                size,
                stream,
            } => self.attach_pane(client, &session_id, &pane_id, size, stream),
            Request::ResyncPane {
                session_id,
                pane_id,
            } => self.resync_pane(client, &session_id, &pane_id),
            Request::DetachPane {
                session_id,
                pane_id,
            } => self.detach_pane(client, &session_id, &pane_id),
            Request::GetPaneHistory {
                session_id,
                pane_id,
                offset,
            } => self.pane_history(client, &session_id, &pane_id, offset),
            Request::SearchPane {
                session_id,
                pane_id,
                query,
            } => self.search_pane(client, &session_id, &pane_id, &query),

            // ----------------------------------------------------------------- pty
            Request::WritePty {
                session_id,
                node_id,
                data,
            } => self.write_pty(&session_id, &node_id, data.as_slice(), now_ms),
            Request::ResizePty {
                session_id,
                node_id,
                size,
            } => self.resize_pty(client, &session_id, &node_id, size),

            // -------------------------------------------------------- node control
            Request::InterruptNode {
                session_id,
                node_id,
            } => self.interrupt_node(&session_id, &node_id),
            Request::TerminateNode {
                session_id,
                node_id,
            } => self.stop_node(&session_id, &node_id, false, now_ms),
            Request::KillNode {
                session_id,
                node_id,
            } => self.stop_node(&session_id, &node_id, true, now_ms),
            Request::RelaunchNode {
                session_id,
                node_id,
                resume,
            } => self.relaunch_node(&session_id, &node_id, resume, now_ms),

            // ----------------------------------------------------------- attention
            Request::NextAttention => Ok(Response::Attention {
                entry: self.next_attention(now_ms),
            }),
            Request::ListAttention { session_id } => Ok(Response::AttentionList {
                entries: self.list_attention(session_id.as_ref(), now_ms),
            }),
            Request::GotoAttention { attention_id } => {
                self.goto_attention(attention_id.as_ref(), now_ms)
            }
            Request::AcknowledgeAttention { attention_id } => {
                self.acknowledge_attention(&attention_id, now_ms)
            }
            Request::SnoozeAttention {
                attention_id,
                until_ms,
            } => self.snooze_attention(&attention_id, until_ms, now_ms),
            Request::DismissAttention { attention_id } => {
                self.dismiss_attention(&attention_id, now_ms)
            }
            Request::MuteSession {
                session_id,
                until_ms,
            } => self.mute_session(&session_id, until_ms, now_ms),
            Request::CorrectState {
                session_id,
                node_id,
                lifecycle,
                turn,
                note,
            } => self.correct_state(&session_id, &node_id, lifecycle, turn, note, now_ms),

            // Inline images belong to another workflow, whose daemon half is still being
            // written. Answered as unavailable rather than left out of this match: a
            // non-exhaustive dispatch stops the whole crate compiling, which stops every
            // other feature's tests from running. Replace this arm with the real handler —
            // do not delete it, or the client's request has no answer at all.
            Request::PaneImage { .. } => Err(ProtoError::new(
                turn_proto::ErrorCode::Unavailable,
                "this daemon does not serve inline images yet",
            )),

            // ------------------------------------------------------ user behaviour
            Request::UpdateUserActivity { context } => self.update_user_activity(context, now_ms),
        }
    }
}

/// Rejects a name that would leave the user with an unidentifiable row.
pub(super) fn check_name(name: &str) -> std::result::Result<String, ProtoError> {
    // Do not repair user-authored navigation identity. A Workspace called
    // `release\nFAILED`, or one containing a bidi override, is a different and
    // misleading label rather than a harmless spelling mistake. Agent/process
    // metadata is projected safely elsewhere; API names are rejected so the
    // caller can correct what it submitted.
    if name
        .chars()
        .any(|character| !turn_pty::is_display_safe(character))
    {
        return Err(ProtoError::invalid(
            "A name cannot contain control, direction-changing, or invisible characters",
        ));
    }
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(ProtoError::invalid("A name cannot be empty"));
    }
    // A name is a label in a sidebar, not a payload. The cap is generous enough that
    // no real task description hits it.
    const MAX: usize = turn_pty::MAX_TITLE_CHARS;
    if trimmed.chars().count() > MAX {
        return Err(ProtoError::invalid(format!(
            "A name cannot be longer than {MAX} characters"
        )));
    }
    if turn_pty::sanitise_label(trimmed, MAX).as_deref() != Some(trimmed) {
        return Err(ProtoError::invalid(
            "A name must be a single safe display label",
        ));
    }
    Ok(trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::check_name;
    use crate::core::testing::Harness;
    use turn_core::event::{Confidence, EventKind, EventSource, TurnEvent};
    use turn_core::ids::{NodeId, PaneId, SessionId};
    use turn_core::model::ProcessNode;
    use turn_core::state::Lifecycle;
    use turn_proto::{ErrorCode, Request};

    const NOW: i64 = 1_775_000_000_000;

    #[test]
    fn navigation_names_reject_adversarial_text_instead_of_rewriting_it() {
        for hostile in [
            "release\nFAILED",
            "release\rFAILED",
            "release\x1b[2JFAILED",
            "release\u{009b}2JFAILED",
            "safe\u{202e}gpj.elif",
            "zero\u{200b}width",
            "joined\u{200d}label",
        ] {
            let error = check_name(hostile).expect_err(hostile);
            assert_eq!(error.code, turn_proto::ErrorCode::InvalidArgument);
        }

        assert_eq!(
            check_name("  Review current diff  ").unwrap(),
            "Review current diff",
            "ordinary surrounding spaces retain the established API behaviour"
        );
        assert!(check_name(&"x".repeat(turn_pty::MAX_TITLE_CHARS + 1)).is_err());
    }

    #[tokio::test]
    async fn requests_cannot_observe_or_hitch_a_ride_on_a_failed_checkpoint() {
        let mut harness = Harness::new().await;
        let session_id = SessionId::from_stored("sess_request_checkpoint_barrier");
        let pane_id = PaneId::from_stored("pane_request_checkpoint_barrier");
        harness.add_session(session_id.clone(), pane_id.clone(), NOW);
        let original_name = harness.core.sessions[&session_id].name.clone();
        let missing_node = NodeId::from_stored("proc_missing_checkpoint_subject");
        // Make the next Session checkpoint fail through the real pane-to-node
        // foreign key. This models any durable-store failure after event reduction
        // without exposing a test-only failure switch in production code.
        harness
            .core
            .sessions
            .get_mut(&session_id)
            .unwrap()
            .layout
            .get_mut(&pane_id)
            .unwrap()
            .node_id = Some(missing_node.clone());
        harness.core.ingest(
            TurnEvent::new(
                session_id.clone(),
                EventKind::AgentIdle,
                EventSource::Supervisor,
                Confidence::Explicit,
                NOW + 1,
            )
            .with_node(missing_node.clone()),
            NOW + 1,
        );
        assert_eq!(harness.core.failed_ingest_checkpoints.len(), 1);

        let (client, _frames) = harness.add_client(16);
        let error = harness
            .core
            .dispatch(
                client,
                Request::ListSessions {
                    workspace_id: None,
                    include_archived: true,
                },
                NOW + 2,
            )
            .expect_err("a read must not expose the uncommitted projection");
        assert_eq!(error.code, ErrorCode::Unavailable);

        let error = harness
            .core
            .dispatch(
                client,
                Request::RenameSession {
                    session_id: session_id.clone(),
                    name: "must not stick".into(),
                },
                NOW + 2,
            )
            .expect_err("a mutation must stop before its handler runs");
        assert_eq!(error.code, ErrorCode::Unavailable);
        assert_eq!(harness.core.sessions[&session_id].name, original_name);

        let mut recovered = ProcessNode::agent(session_id.clone(), "claude", "/tmp", NOW + 3);
        recovered.id = missing_node;
        recovered.lifecycle = Lifecycle::Alive;
        harness
            .core
            .sessions
            .get_mut(&session_id)
            .unwrap()
            .tree
            .insert(recovered);
        harness
            .core
            .dispatch(
                client,
                Request::ListSessions {
                    workspace_id: None,
                    include_archived: true,
                },
                NOW + 3,
            )
            .unwrap();
        assert!(harness.core.failed_ingest_checkpoints.is_empty());
        assert_eq!(
            harness
                .core
                .store
                .sessions()
                .get(&session_id)
                .unwrap()
                .unwrap()
                .name,
            original_name
        );
    }
}
