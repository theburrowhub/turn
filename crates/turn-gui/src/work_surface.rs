//! The single selection-driven surface to the right of the hierarchy.
//!
//! A tree row is a navigation target, not a Pane command.  This module keeps that
//! distinction visible in the types: resolving a [`ViewTarget`] borrows the exact
//! Workspace, Session or Node projection and rendering it never edits `Layout`.

use egui::{Align2, FontId, Rect, RichText, Sense, Stroke, Ui, Vec2};
use turn_core::ids::{NodeId, SessionId, WorkspaceId};
use turn_proto::{
    HierarchyKey, HierarchySnapshot, InspectorDetails, NodePaneCapability, SessionTreeView,
    TreeNodeView, WorkspaceTreeView,
};

use super::{
    format_duration, inspector_empty, inspector_handoffs, inspector_history, inspector_optional,
    inspector_optional_owned, inspector_section, inspector_value, lifecycle_label, node_kind_label,
    preview_source_label, process_title, region, visible_preview, HierarchyAction, PaneContent,
    TurnView, ViewAction, ViewState,
};
use crate::terminal::{self, PaneAction, PaneOptions};
use crate::theme::Theme;

/// The closed navigation algebra for the current window surface.
///
/// It deliberately contains no Pane id. A Pane is one possible representation of
/// a Node, not its identity and not the destination of a tree click.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewTarget {
    Workspace(WorkspaceId),
    Session(SessionId),
    Node(NodeId),
}

/// Measured geometry from the most recently drawn Node WorkSurface.
///
/// This is diagnostic/test state rather than persisted layout state. Keeping the
/// numbers makes the "use the available surface" contract deterministic without
/// teaching the domain model about pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WorkSurfaceGeometry {
    pub width: f32,
    pub height: f32,
    pub header_height: f32,
    pub primary_width: f32,
    pub primary_height: f32,
    pub details_width: f32,
    pub stacked: bool,
}

#[derive(Clone, Copy)]
pub(super) enum ResolvedViewTarget<'a> {
    Workspace(&'a WorkspaceTreeView),
    Session {
        workspace: &'a WorkspaceTreeView,
        session: &'a SessionTreeView,
    },
    Node {
        workspace: &'a WorkspaceTreeView,
        session: &'a SessionTreeView,
        node: &'a TreeNodeView,
    },
}

impl ResolvedViewTarget<'_> {
    pub(super) fn public(self) -> ViewTarget {
        match self {
            Self::Workspace(workspace) => ViewTarget::Workspace(workspace.workspace.id.clone()),
            Self::Session { session, .. } => ViewTarget::Session(session.session.id.clone()),
            Self::Node { node, .. } => ViewTarget::Node(node.node_id.clone()),
        }
    }

    pub(super) fn is_node(self) -> bool {
        matches!(self, Self::Node { .. })
    }
}

pub(super) fn resolve<'a>(
    snapshot: &'a HierarchySnapshot,
    selected: Option<&HierarchyKey>,
    active_session: Option<&SessionId>,
) -> Option<ResolvedViewTarget<'a>> {
    let selected = selected
        .cloned()
        .or_else(|| active_session.cloned().map(HierarchyKey::session))?;
    match selected {
        HierarchyKey::Workspace { workspace_id } => snapshot
            .workspaces
            .iter()
            .find(|workspace| workspace.workspace.id == workspace_id)
            .map(ResolvedViewTarget::Workspace),
        HierarchyKey::Session { session_id } => snapshot.workspaces.iter().find_map(|workspace| {
            workspace
                .sessions
                .iter()
                .find(|session| session.session.id == session_id)
                .map(|session| ResolvedViewTarget::Session { workspace, session })
        }),
        HierarchyKey::Process { node_id } => snapshot.workspaces.iter().find_map(|workspace| {
            workspace.sessions.iter().find_map(|session| {
                session
                    .nodes
                    .iter()
                    .find(|node| node.node_id == node_id)
                    .map(|node| ResolvedViewTarget::Node {
                        workspace,
                        session,
                        node,
                    })
            })
        }),
    }
}

/// Starts the two bounded read-only projections used by a Node view.
///
/// The request keys are also the response fences: inspector data is rendered only
/// when `InspectorDetails::key()` equals the current key, and activity is cached by
/// exact NodeId. A late sibling response can populate its own cache entry but cannot
/// replace the visible subject.
pub(super) fn request_node_projection(
    state: &mut ViewState,
    snapshot: &HierarchySnapshot,
    target: ResolvedViewTarget<'_>,
) {
    let ResolvedViewTarget::Node { node, .. } = target else {
        state.node_preview_key = None;
        return;
    };
    let key = HierarchyKey::process(node.node_id.clone());
    if state.inspector_key.as_ref() != Some(&key) {
        state.inspector_key = Some(key.clone());
        state.push_hierarchy_action(HierarchyAction::Inspect { key });
    }
    if state.node_preview_key.as_ref() != Some(&node.node_id) {
        state.node_preview_key = Some(node.node_id.clone());
        state.push_hierarchy_action(HierarchyAction::QuickPreview {
            surface_id: snapshot.tree_state.surface_id.clone(),
            session_id: node.session_id.clone(),
            node_id: node.node_id.clone(),
        });
    }
}

impl TurnView<'_> {
    pub(super) fn workspace_work_surface(
        &self,
        ui: &mut Ui,
        theme: &Theme,
        workspace: &WorkspaceTreeView,
    ) -> Vec<ViewAction> {
        let area = ui.available_rect_before_wrap();
        ui.painter().rect_filled(area, 0.0, theme.background);
        let content = area.shrink2(Vec2::new(24.0, 20.0));
        ui.scope_builder(region(content, "workspace-work-surface"), |ui| {
            ui.label(
                RichText::new("WORKSPACE")
                    .monospace()
                    .small()
                    .color(theme.text_faint),
            );
            ui.heading(RichText::new(&workspace.workspace.name).color(theme.text));
            ui.label(
                RichText::new(&workspace.workspace.root)
                    .monospace()
                    .color(theme.text_dim),
            );
            ui.add_space(16.0);
            ui.horizontal_wrapped(|ui| {
                metric(ui, theme, "SESSIONS", workspace.sessions.len().to_string());
                metric(
                    ui,
                    theme,
                    "RUNNING",
                    workspace
                        .sessions
                        .iter()
                        .map(|session| session.session.running_count)
                        .sum::<usize>()
                        .to_string(),
                );
                metric(
                    ui,
                    theme,
                    "NEEDS YOU",
                    workspace
                        .sessions
                        .iter()
                        .map(|session| session.session.badge_count)
                        .sum::<usize>()
                        .to_string(),
                );
            });
            ui.add_space(18.0);
            ui.label(
                RichText::new(
                    "Select a Session for its saved layout or a child for its exact view.",
                )
                .color(theme.text_dim),
            );
        });
        Vec::new()
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn node_work_surface(
        &self,
        ui: &mut Ui,
        theme: &Theme,
        snapshot: &HierarchySnapshot,
        workspace: &WorkspaceTreeView,
        session: &SessionTreeView,
        node: &TreeNodeView,
        details: Option<&InspectorDetails>,
        state: &mut ViewState,
    ) -> Vec<ViewAction> {
        let mut actions = Vec::new();
        let area = ui.available_rect_before_wrap();
        ui.painter().rect_filled(area, 0.0, theme.background);

        let attention_height: f32 = if node.needs_user { 34.0 } else { 0.0 };
        let header_height = 76.0_f32.min(area.height());
        let attention = Rect::from_min_size(
            area.min,
            Vec2::new(area.width(), attention_height.min(area.height())),
        );
        let header = Rect::from_min_size(
            attention.left_bottom(),
            Vec2::new(
                area.width(),
                header_height.min((area.height() - attention_height).max(0.0)),
            ),
        );
        let body =
            Rect::from_min_max(header.left_bottom(), area.max).shrink2(Vec2::new(12.0, 10.0));

        if node.needs_user {
            ui.painter()
                .rect_filled(attention, 0.0, theme.attention.gamma_multiply(0.14));
            ui.painter().text(
                attention.left_center() + Vec2::new(12.0, 0.0),
                Align2::LEFT_CENTER,
                attention_copy(node),
                FontId::new(11.0, egui::FontFamily::Monospace),
                theme.attention,
            );
        }
        paint_node_header(ui, theme, header, workspace, session, node);

        let stacked = body.width() < 760.0;
        let gap = 12.0_f32.min(body.width());
        let (primary, detail_rect) = if stacked {
            let details_height = (body.height() * 0.36)
                .clamp(150.0, 260.0)
                .min(body.height());
            let primary_height = (body.height() - details_height - gap).max(0.0);
            (
                Rect::from_min_size(body.min, Vec2::new(body.width(), primary_height)),
                Rect::from_min_max(body.min + Vec2::new(0.0, primary_height + gap), body.max),
            )
        } else {
            let details_width = 320.0_f32
                .min(body.width() * 0.34)
                .max(240.0)
                .min(body.width());
            let primary_width = (body.width() - details_width - gap).max(0.0);
            (
                Rect::from_min_size(body.min, Vec2::new(primary_width, body.height())),
                Rect::from_min_max(body.min + Vec2::new(primary_width + gap, 0.0), body.max),
            )
        };
        state.work_surface_geometry = Some(WorkSurfaceGeometry {
            width: area.width(),
            height: area.height(),
            header_height: attention_height + header.height(),
            primary_width: primary.width(),
            primary_height: primary.height(),
            details_width: detail_rect.width(),
            stacked,
        });

        panel(ui, theme, primary);
        panel(ui, theme, detail_rect);
        if let Some((pane, content)) = self.exact_terminal(node) {
            actions.extend(self.terminal_primary(
                ui,
                theme,
                primary,
                &snapshot.tree_state.surface_id,
                node,
                pane,
                content,
                state,
            ));
        } else {
            semantic_primary(ui, theme, primary, node, details, state, self.now_ms);
        }
        node_details(
            ui,
            theme,
            detail_rect,
            snapshot,
            session,
            node,
            details,
            state,
            self.now_ms,
        );

        let id = ui.id().with(("node-work-surface", node.node_id.as_str()));
        ui.ctx().accesskit_node_builder(id, |builder| {
            builder.set_role(egui::accesskit::Role::Group);
            builder.set_label(format!(
                "WorkSurface for exact {} {}",
                node_kind_label(node.kind),
                process_title(node)
            ));
        });
        actions
    }

    fn exact_terminal<'a>(
        &'a self,
        node: &'a TreeNodeView,
    ) -> Option<(&'a turn_core::model::PaneNodeBinding, &'a PaneContent<'a>)> {
        if !matches!(node.pane_capability, NodePaneCapability::Terminal { .. }) {
            return None;
        }
        node.pane_bindings.iter().find_map(|binding| {
            (!binding.temporary)
                .then(|| {
                    self.panes
                        .iter()
                        .find(|pane| pane.pane_id == binding.pane_id)
                        .map(|pane| (binding, pane))
                })
                .flatten()
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn terminal_primary(
        &self,
        ui: &mut Ui,
        theme: &Theme,
        rect: Rect,
        surface_id: &str,
        node: &TreeNodeView,
        binding: &turn_core::model::PaneNodeBinding,
        content: &PaneContent<'_>,
        state: &mut ViewState,
    ) -> Vec<ViewAction> {
        let strip = Rect::from_min_size(
            rect.min,
            Vec2::new(rect.width(), 30.0_f32.min(rect.height())),
        );
        ui.painter().rect_filled(strip, 6.0, theme.panel);
        ui.painter().text(
            strip.left_center() + Vec2::new(10.0, 0.0),
            Align2::LEFT_CENTER,
            "EXACT TERMINAL · READ-ONLY MIRROR",
            FontId::new(10.0, egui::FontFamily::Monospace),
            theme.text_dim,
        );
        let mirror_id = ui
            .id()
            .with(("node-terminal-mirror-label", node.node_id.as_str()));
        ui.ctx().accesskit_node_builder(mirror_id, |builder| {
            builder.set_role(egui::accesskit::Role::Group);
            builder.set_label(format!(
                "Exact read-only terminal mirror for {}",
                process_title(node)
            ));
        });
        let focus_rect = Rect::from_min_size(
            strip.right_top() - Vec2::new(128.0, -3.0),
            Vec2::new(122.0, 24.0),
        );
        if ui
            .put(focus_rect, egui::Button::new("Focus exact Pane"))
            .clicked()
        {
            state.push_hierarchy_action(HierarchyAction::FocusPaneForNode {
                surface_id: surface_id.to_string(),
                session_id: binding.session_id.clone(),
                node_id: node.node_id.clone(),
            });
        }
        let terminal_rect = Rect::from_min_max(strip.left_bottom(), rect.max).shrink(6.0);
        let interaction = state
            .node_terminal_views
            .entry(node.node_id.clone())
            .or_default();
        let options = PaneOptions {
            focused: false,
            accepts_input: false,
            now_ms: self.now_ms,
            scrolled: content.scrolled,
            history_complete: content.history_complete,
        };
        // A separate interaction object and a filtered outcome are both intentional:
        // measuring this larger read-only mirror must not resize or focus the saved Pane.
        terminal::show(
            ui,
            theme,
            terminal_rect,
            content.grid,
            interaction,
            options,
            ui.id()
                .with(("node-terminal-mirror", node.node_id.as_str())),
        )
        .into_iter()
        .filter_map(|action| match action {
            PaneAction::Copy(_) | PaneAction::Scroll(_) => Some(ViewAction::Pane {
                pane_id: content.pane_id.clone(),
                action,
            }),
            PaneAction::Resize(_) | PaneAction::Focus | PaneAction::Write(_) => None,
        })
        .collect()
    }
}

fn paint_node_header(
    ui: &mut Ui,
    theme: &Theme,
    rect: Rect,
    workspace: &WorkspaceTreeView,
    session: &SessionTreeView,
    node: &TreeNodeView,
) {
    ui.painter().rect_filled(rect, 0.0, theme.panel);
    ui.painter()
        .hline(rect.x_range(), rect.max.y, Stroke::new(1.0, theme.border));
    ui.scope_builder(
        region(rect.shrink2(Vec2::new(14.0, 8.0)), "node-view-header"),
        |ui| {
            ui.horizontal(|ui| {
                ui.heading(RichText::new(process_title(node)).color(theme.text));
                let (colour, glyph) = theme.state_marker(node.display_state);
                ui.label(
                    RichText::new(format!("{glyph} {}", node.state_label))
                        .monospace()
                        .color(colour),
                );
                ui.label(
                    RichText::new(node_kind_label(node.kind))
                        .monospace()
                        .small()
                        .color(theme.text_dim),
                );
            });
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(format!(
                        "{}  /  {}",
                        workspace.workspace.name, session.session.name
                    ))
                    .color(theme.text_dim),
                );
                if let Some(agent) = &node.agent {
                    if let Some(provider) = agent.agent.provider.as_deref() {
                        ui.label(RichText::new(provider).monospace().color(theme.text_faint));
                    }
                    if let Some(model) = agent.agent.model.as_deref() {
                        ui.label(RichText::new(model).monospace().color(theme.text_faint));
                    }
                }
            });
        },
    );
}

fn semantic_primary(
    ui: &mut Ui,
    theme: &Theme,
    rect: Rect,
    node: &TreeNodeView,
    details: Option<&InspectorDetails>,
    state: &ViewState,
    now_ms: i64,
) {
    let inner = rect.shrink(14.0);
    ui.scope_builder(region(inner, "semantic-node-primary"), |ui| {
        egui::ScrollArea::vertical()
            .id_salt(("node-primary-scroll", node.node_id.as_str()))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                inspector_section(ui, theme, "TASK");
                match node
                    .agent
                    .as_ref()
                    .and_then(|agent| agent.current_task.as_deref())
                {
                    Some(task) => {
                        ui.label(RichText::new(task).size(17.0).color(theme.text));
                    }
                    None => inspector_empty(ui, theme, "No task has been reported for this node"),
                }

                ui.add_space(12.0);
                inspector_section(ui, theme, "ACTIVITY");
                let history = state
                    .preview_history
                    .get(&node.node_id)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                if history.is_empty() {
                    if let Some(preview) = visible_preview(node) {
                        activity_card(ui, theme, preview);
                    } else {
                        inspector_empty(ui, theme, "No stable, safe activity has arrived yet");
                    }
                } else {
                    for preview in history {
                        if !preview.contains_sensitive_data || preview.redacted {
                            activity_card(ui, theme, preview);
                        }
                    }
                }

                if let Some(last) = node
                    .agent
                    .as_ref()
                    .and_then(|agent| agent.last_message.as_deref())
                {
                    ui.add_space(12.0);
                    inspector_section(ui, theme, "LATEST AGENT MESSAGE");
                    ui.label(RichText::new(last).color(theme.text));
                }

                match details {
                    Some(InspectorDetails::Agent { history, .. })
                    | Some(InspectorDetails::Process { history, .. })
                        if !history.is_empty() =>
                    {
                        ui.add_space(12.0);
                        inspector_section(ui, theme, "EVENT HISTORY");
                        inspector_history(ui, theme, history, now_ms);
                    }
                    _ => {}
                }
            });
    });
}

#[allow(clippy::too_many_arguments)]
fn node_details(
    ui: &mut Ui,
    theme: &Theme,
    rect: Rect,
    snapshot: &HierarchySnapshot,
    session: &SessionTreeView,
    node: &TreeNodeView,
    details: Option<&InspectorDetails>,
    state: &mut ViewState,
    now_ms: i64,
) {
    ui.scope_builder(region(rect.shrink(12.0), "node-view-details"), |ui| {
        egui::ScrollArea::vertical()
            .id_salt(("node-details-scroll", node.node_id.as_str()))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                inspector_section(ui, theme, "IDENTITY & RUNTIME");
                inspector_value(ui, theme, "type", node_kind_label(node.kind));
                inspector_value(ui, theme, "state", &node.state_label);
                inspector_value(ui, theme, "lifecycle", lifecycle_label(&node.lifecycle));
                inspector_value(ui, theme, "runtime", &format_duration(node.runtime_ms));
                inspector_optional_owned(ui, theme, "pid", node.pid.map(|pid| pid.to_string()));
                if let Some(agent) = &node.agent {
                    inspector_optional(ui, theme, "provider", agent.agent.provider.as_deref());
                    inspector_optional(ui, theme, "tool", agent.agent.tool.as_deref());
                    inspector_optional(ui, theme, "model", agent.agent.model.as_deref());
                    inspector_optional(ui, theme, "Agent type", agent.agent_type.as_deref());
                    inspector_optional(ui, theme, "permission mode", agent.permission_mode.as_deref());
                    inspector_optional(ui, theme, "branch", agent.git_branch.as_deref());
                }

                inspector_section(ui, theme, "VIEW CAPABILITY");
                match node.pane_capability {
                    NodePaneCapability::PreviewDetails => {
                        inspector_value(ui, theme, "primary", "structured activity and details");
                        ui.label(
                            RichText::new("This semantic node has no terminal of its own. Turn will never substitute its parent's terminal.")
                                .small()
                                .color(theme.text_faint),
                        );
                    }
                    NodePaneCapability::Terminal { .. } => {
                        inspector_value(ui, theme, "primary", "exact terminal binding when available");
                    }
                }

                if let Some(agent) = &node.agent {
                    inspector_section(ui, theme, "USAGE");
                    inspector_optional_owned(
                        ui,
                        theme,
                        "tokens",
                        agent.tokens_used.map(|tokens| tokens.to_string()),
                    );
                    inspector_optional_owned(
                        ui,
                        theme,
                        "cost",
                        agent.cost_usd.map(|cost| format!("${cost:.4}")),
                    );
                }

                match details {
                    Some(InspectorDetails::Agent {
                        parent,
                        origin,
                        handoffs,
                        ..
                    }) => {
                        inspector_section(ui, theme, "RELATIONSHIP");
                        inspector_value(ui, theme, "origin", &origin.label);
                        if let Some(parent) = parent {
                            inspector_value(ui, theme, "parent", &parent.name);
                        } else {
                            inspector_empty(ui, theme, "No parent relationship is known");
                        }
                        if !handoffs.is_empty() {
                            inspector_section(ui, theme, "CONTEXT HANDOFFS");
                            inspector_handoffs(ui, theme, handoffs, now_ms);
                        }
                    }
                    Some(InspectorDetails::Process { parent, origin, .. }) => {
                        inspector_section(ui, theme, "RELATIONSHIP");
                        inspector_value(ui, theme, "origin", &origin.label);
                        if let Some(parent) = parent {
                            inspector_value(ui, theme, "parent", &parent.name);
                        }
                    }
                    _ => {}
                }

                inspector_section(ui, theme, "ACTIONS");
                if matches!(node.pane_capability, NodePaneCapability::Terminal { .. })
                    && !node.pane_bindings.is_empty()
                    && ui.button("Focus exact Pane").clicked()
                {
                    state.push_hierarchy_action(HierarchyAction::FocusPaneForNode {
                        surface_id: snapshot.tree_state.surface_id.clone(),
                        session_id: session.session.id.clone(),
                        node_id: node.node_id.clone(),
                    });
                }
                if node.is_agentic && ui.button("Rename Agent…").clicked() {
                    state.node_edit = Some(super::NodeEditDraft::rename(node));
                }
                if node.is_agentic && ui.button("Correct relationship…").clicked() {
                    state.node_edit = Some(super::NodeEditDraft::relationship(node));
                }
                if node.is_agentic
                    && session.nodes.iter().any(|candidate| {
                        candidate.is_agentic && candidate.node_id != node.node_id
                    })
                    && ui.button("Pass context to Agent…").clicked()
                {
                    state.context_handoff = Some(super::ContextHandoffDraft::new(session, node));
                }
            });
    });
}

fn activity_card(ui: &mut Ui, theme: &Theme, preview: &turn_core::model::ActivityPreview) {
    ui.group(|ui| {
        ui.set_min_width(ui.available_width());
        ui.label(RichText::new(&preview.normalized_text).color(theme.text));
        ui.label(
            RichText::new(format!(
                "{} · {}{}",
                preview_source_label(preview.source),
                preview.confidence.label(),
                if preview.redacted { " · redacted" } else { "" }
            ))
            .monospace()
            .small()
            .color(theme.text_faint),
        );
    });
    ui.add_space(6.0);
}

fn attention_copy(node: &TreeNodeView) -> String {
    if let Some(agent) = &node.agent {
        if let Some(permission) = &agent.pending_permission {
            return format!("NEEDS YOU · PERMISSION · {}", permission.summary);
        }
        if let Some(question) = agent.pending_question.as_deref() {
            return format!("NEEDS YOU · QUESTION · {question}");
        }
    }
    format!("NEEDS YOU · {}", node.state_label)
}

fn panel(ui: &Ui, theme: &Theme, rect: Rect) {
    ui.painter().rect_filled(rect, 7.0, theme.panel);
    ui.painter().rect_stroke(
        rect,
        7.0,
        Stroke::new(1.0, theme.border),
        egui::StrokeKind::Inside,
    );
}

fn metric(ui: &mut Ui, theme: &Theme, label: &str, value: String) {
    let response = ui.allocate_response(Vec2::new(112.0, 64.0), Sense::hover());
    ui.painter().rect_filled(response.rect, 6.0, theme.panel);
    ui.painter().rect_stroke(
        response.rect,
        6.0,
        Stroke::new(1.0, theme.border),
        egui::StrokeKind::Inside,
    );
    ui.painter().text(
        response.rect.left_top() + Vec2::new(10.0, 9.0),
        Align2::LEFT_TOP,
        label,
        FontId::new(10.0, egui::FontFamily::Monospace),
        theme.text_faint,
    );
    ui.painter().text(
        response.rect.left_bottom() + Vec2::new(10.0, -10.0),
        Align2::LEFT_BOTTOM,
        value,
        FontId::new(22.0, egui::FontFamily::Proportional),
        theme.text,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use turn_core::model::{Layout, Pane, PaneKind, ProcessNode, Session, Workspace};
    use turn_proto::{SessionSummary, TreeSurfaceState, WorkspaceSummary};

    const T0: i64 = 1_700_000_000_000;

    fn targets() -> (HierarchySnapshot, WorkspaceId, SessionId, NodeId, NodeId) {
        let workspace = Workspace::new("turn", "/repo/turn", T0);
        let mut session = Session::new(
            workspace.id.clone(),
            "work surface",
            "/repo/turn",
            Layout::single(Pane::new(PaneKind::Agent)),
            T0,
        );
        let first = session.tree.insert(ProcessNode::agent(
            session.id.clone(),
            "first",
            "/repo/turn",
            T0,
        ));
        let second = session.tree.insert(ProcessNode::agent(
            session.id.clone(),
            "second",
            "/repo/turn",
            T0 + 1,
        ));
        let summary = SessionSummary::from_session(&session, 0, false, T0 + 2);
        let snapshot = HierarchySnapshot {
            revision: 1,
            tree_state: TreeSurfaceState::empty("surface"),
            workspaces: vec![WorkspaceTreeView {
                workspace: WorkspaceSummary::from_workspace(
                    &workspace,
                    std::slice::from_ref(&summary),
                ),
                checkouts: Vec::new(),
                write_lease: None,
                sessions: vec![SessionTreeView {
                    session: summary,
                    nodes: TreeNodeView::for_session(&session, T0 + 2),
                }],
            }],
        };
        (snapshot, workspace.id, session.id, first, second)
    }

    #[test]
    fn sibling_keys_resolve_to_distinct_exact_node_targets() {
        let (snapshot, _, _, first, second) = targets();
        let first_target = resolve(&snapshot, Some(&HierarchyKey::process(first.clone())), None)
            .expect("first target")
            .public();
        let second_target = resolve(
            &snapshot,
            Some(&HierarchyKey::process(second.clone())),
            None,
        )
        .expect("second target")
        .public();
        assert_eq!(first_target, ViewTarget::Node(first));
        assert_eq!(second_target, ViewTarget::Node(second));
        assert_ne!(first_target, second_target);
    }

    #[test]
    fn every_hierarchy_level_has_one_closed_view_target() {
        let (snapshot, workspace, session, first, _) = targets();
        assert_eq!(
            resolve(
                &snapshot,
                Some(&HierarchyKey::workspace(workspace.clone())),
                None
            )
            .map(ResolvedViewTarget::public),
            Some(ViewTarget::Workspace(workspace))
        );
        assert_eq!(
            resolve(
                &snapshot,
                Some(&HierarchyKey::session(session.clone())),
                None
            )
            .map(ResolvedViewTarget::public),
            Some(ViewTarget::Session(session))
        );
        assert_eq!(
            resolve(&snapshot, Some(&HierarchyKey::process(first.clone())), None)
                .map(ResolvedViewTarget::public),
            Some(ViewTarget::Node(first))
        );
    }
}
