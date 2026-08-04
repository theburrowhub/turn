//! The request surface: all forty-one operations.
//!
//! Split by subsystem, dispatched from one place so the daemon's whole surface is
//! visible in a single match — the same reason [`turn_proto::Request`] is one flat
//! enum. Every handler is synchronous and runs inside the core task, so a handler
//! reads and writes state without a lock and without the possibility of another
//! request interleaving halfway through.

mod attention;
mod nodes;
mod panes;
mod sessions;
mod workspaces;

use super::command::ClientId;
use super::Core;
use turn_proto::{ProtoError, Request, Response};

/// The result every handler returns: a typed response, or the one error shape.
pub type Answer = std::result::Result<Response, ProtoError>;

impl Core {
    /// Routes one request to its handler.
    pub(crate) fn dispatch(&mut self, client: ClientId, request: Request, now_ms: i64) -> Answer {
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

            // ----------------------------------------------------------- templates
            Request::ListTemplates => Ok(Response::Templates {
                templates: self.template_summaries(),
            }),
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
            Request::FocusPane { session_id, target } => {
                self.focus_pane(client, &session_id, target)
            }
            Request::SwapPanes { session_id, a, b } => self.swap_panes(client, &session_id, &a, &b),
            Request::ZoomPane {
                session_id,
                pane_id,
            } => self.zoom_pane(client, &session_id, &pane_id),
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

            // ------------------------------------------------------ user behaviour
            Request::UpdateUserActivity { context } => self.update_user_activity(context, now_ms),
        }
    }
}

/// Rejects a name that would leave the user with an unidentifiable row.
pub(super) fn check_name(name: &str) -> std::result::Result<String, ProtoError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(ProtoError::invalid("A name cannot be empty"));
    }
    // A name is a label in a sidebar, not a payload. The cap is generous enough that
    // no real task description hits it.
    const MAX: usize = 200;
    if trimmed.chars().count() > MAX {
        return Err(ProtoError::invalid(format!(
            "A name cannot be longer than {MAX} characters"
        )));
    }
    Ok(trimmed.to_string())
}
