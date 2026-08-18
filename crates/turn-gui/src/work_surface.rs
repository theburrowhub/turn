//! The single selection-driven surface to the right of the hierarchy.
//!
//! A tree row is a navigation target, not a Pane command.  This module keeps that
//! distinction visible in the types: resolving a [`ViewTarget`] borrows the exact
//! Workspace, Session or Node projection and rendering it never edits `Layout`.

use egui::{Align2, FontId, Key, Modifiers, Rect, RichText, Sense, Stroke, Ui, Vec2};
use turn_core::ids::{NodeId, SessionId, WorkspaceId};
use turn_core::model::{
    AgentRuntimeMetadata, LaunchConfiguration, Observable, ObservationSourceKind, QuotaSnapshot,
    QuotaWindow, UsageMeasurement, UsageMeasurementKind, UsageUnit,
};
use turn_core::state::{AwaitingReason, Turn};
use turn_proto::{
    AgentSummary, HierarchyKey, HierarchySnapshot, InspectorDetails, NodePaneCapability,
    SessionTreeView, TreeNodeView, WorkspaceTreeView,
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
        _workspace: &WorkspaceTreeView,
        session: &SessionTreeView,
        node: &TreeNodeView,
        details: Option<&InspectorDetails>,
        state: &mut ViewState,
    ) -> Vec<ViewAction> {
        let mut actions = Vec::new();
        let area = ui.available_rect_before_wrap();
        ui.painter().rect_filled(area, 0.0, theme.background);
        // Hierarchy rows are the low-latency navigation projection. Any text owned by
        // an Agent must cross the inspector redaction boundary before WorkSurface may
        // paint it, expose it through AccessKit, or copy it into an editing draft.
        let safe_node = inspected_node_for(details, &node.node_id);
        let safe_details = safe_node.and(details);

        let attention_height: f32 = if node.needs_user { 34.0 } else { 0.0 };
        // Three compact rows: identity/state, Session/provider, then the six
        // runtime facts the operator must not have to open an inspector to see.
        // A narrow WorkSurface needs one extra wrapped line; reserving it here
        // keeps the quota fact visible instead of painting it behind the body.
        let header_height = (if area.width() < 760.0 {
            118.0_f32
        } else {
            98.0_f32
        })
        .min(area.height());
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
            let copy = attention_copy(node, safe_node);
            ui.painter()
                .rect_filled(attention, 0.0, theme.attention.gamma_multiply(0.14));
            ui.painter().text(
                attention.left_center() + Vec2::new(12.0, 0.0),
                Align2::LEFT_CENTER,
                &copy,
                FontId::new(11.0, egui::FontFamily::Monospace),
                theme.attention,
            );
            let attention_id = ui
                .id()
                .with(("node-work-surface-attention", node.node_id.as_str()));
            ui.ctx().accesskit_node_builder(attention_id, |builder| {
                builder.set_role(egui::accesskit::Role::Alert);
                builder.set_label(copy);
            });
        }
        paint_node_header(
            ui,
            theme,
            header,
            node,
            safe_node,
            safe_details,
            self.now_ms,
        );

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
                safe_node,
                pane,
                content,
                state,
            ));
        } else {
            semantic_primary(
                ui,
                theme,
                primary,
                node,
                safe_node,
                safe_details,
                state,
                self.now_ms,
            );
        }
        node_details(
            ui,
            theme,
            detail_rect,
            snapshot,
            session,
            node,
            safe_node,
            safe_details,
            state,
            self.now_ms,
        );

        let id = ui.id().with(("node-work-surface", node.node_id.as_str()));
        ui.ctx().accesskit_node_builder(id, |builder| {
            builder.set_role(egui::accesskit::Role::Group);
            builder.set_label(format!(
                "WorkSurface for exact {} {}",
                node_kind_label(node.kind),
                work_surface_title(node, safe_node)
            ));
        });
        actions
    }

    pub(crate) fn exact_terminal<'a>(
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
        _surface_id: &str,
        node: &TreeNodeView,
        safe_node: Option<&TreeNodeView>,
        _binding: &turn_core::model::PaneNodeBinding,
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
            "EXACT TERMINAL",
            FontId::new(10.0, egui::FontFamily::Monospace),
            theme.text_dim,
        );
        let mirror_id = ui
            .id()
            .with(("node-terminal-mirror-label", node.node_id.as_str()));
        ui.ctx().accesskit_node_builder(mirror_id, |builder| {
            builder.set_role(egui::accesskit::Role::Group);
            builder.set_label(format!(
                "Exact operational terminal for {}",
                work_surface_title(node, safe_node)
            ));
        });
        let terminal_rect = Rect::from_min_max(strip.left_bottom(), rect.max).shrink(6.0);
        let resize_epoch = content
            .runtime_id
            .as_ref()
            .and_then(|runtime_id| state.resize_owner_epoch(runtime_id, &content.pane_id));
        let options = PaneOptions {
            focused: true,
            accepts_input: !state.is_sensitive()
                && self.write_conflict.is_none()
                && self.link_confirmation.is_none(),
            now_ms: self.now_ms,
            scrolled: content.scrolled,
            history_complete: content.history_complete,
        };
        let interaction = state.pane(&content.pane_id);
        // This exact-node view is the primary surface while selected, so its complete
        // body owns the shared runtime geometry. The Pane identity remains the saved
        // binding; changing views does not create a second terminal or lose selection.
        terminal::show_pane(
            ui,
            interaction,
            terminal::PaneInput {
                theme,
                rect: terminal_rect,
                grid: content.grid,
                options,
                id: ui
                    .id()
                    .with(("node-terminal-mirror", node.node_id.as_str())),
                resize_claim: content.runtime_id.as_ref().and_then(|runtime_id| {
                    resize_epoch.map(|owner_epoch| terminal::ResizeClaim {
                        runtime_id,
                        owner_epoch,
                    })
                }),
                chrome: None,
            },
        )
        .actions
        .into_iter()
        .map(|action| match action {
            PaneAction::Copy(_)
            | PaneAction::Scroll(_)
            | PaneAction::Resize(_)
            | PaneAction::GeometryUnavailable
            | PaneAction::Focus
            | PaneAction::Write(_) => ViewAction::Pane {
                pane_id: content.pane_id.clone(),
                action,
            },
        })
        .collect()
    }
}

fn paint_node_header(
    ui: &mut Ui,
    theme: &Theme,
    rect: Rect,
    node: &TreeNodeView,
    safe_node: Option<&TreeNodeView>,
    details: Option<&InspectorDetails>,
    now_ms: i64,
) {
    ui.painter().rect_filled(rect, 0.0, theme.panel);
    ui.painter()
        .hline(rect.x_range(), rect.max.y, Stroke::new(1.0, theme.border));
    ui.scope_builder(
        region(rect.shrink2(Vec2::new(14.0, 8.0)), "node-view-header"),
        |ui| {
            ui.horizontal(|ui| {
                ui.heading(RichText::new(work_surface_title(node, safe_node)).color(theme.text));
                let (colour, glyph) = theme.state_marker(node.display_state);
                ui.label(
                    RichText::new(format!("{glyph} {}", node.display_state.label()))
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
                let session_name = match details {
                    Some(InspectorDetails::Agent { session_name, .. })
                    | Some(InspectorDetails::Process { session_name, .. }) => {
                        Some(session_name.as_str())
                    }
                    _ => None,
                };
                ui.label(
                    RichText::new(match session_name {
                        Some(session_name) => format!("SESSION  /  {session_name}"),
                        None => "SESSION  /  Loading safe details…".to_string(),
                    })
                    .color(theme.text_dim),
                );
                if let Some(agent) = safe_node.and_then(|node| node.agent.as_ref()) {
                    if let Some(provider) = agent.agent.provider.as_deref() {
                        ui.label(RichText::new(provider).monospace().color(theme.text_faint));
                    }
                }
            });
            if node.is_agentic {
                let facts = safe_node
                    .and_then(|node| node.agent.as_ref())
                    .map(|agent| agent_header_facts(agent, now_ms))
                    .unwrap_or_else(CompactAgentFacts::loading);
                ui.horizontal_wrapped(|ui| {
                    // Wrapped facts are a dense status strip, not separate form
                    // rows; no extra vertical gutter is needed between them.
                    ui.spacing_mut().item_spacing.y = 0.0;
                    compact_fact(ui, theme, "MODEL", &facts.model);
                    compact_fact(ui, theme, "MODE", &facts.mode);
                    compact_fact(ui, theme, "EFFORT", &facts.effort);
                    compact_fact(ui, theme, "THINK", &facts.thinking);
                    compact_fact(ui, theme, "CONTEXT", &facts.context);
                    compact_fact(ui, theme, "QUOTA", &facts.quota);
                });
            }
        },
    );
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeFactState {
    Observed,
    Waiting,
    Unsupported,
    Stale,
    Failed,
    Unreported,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CompactFact {
    value: String,
    state: RuntimeFactState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CompactAgentFacts {
    model: CompactFact,
    mode: CompactFact,
    effort: CompactFact,
    thinking: CompactFact,
    context: CompactFact,
    quota: CompactFact,
}

impl CompactAgentFacts {
    fn loading() -> Self {
        let loading = || CompactFact {
            value: "loading".into(),
            state: RuntimeFactState::Waiting,
        };
        Self {
            model: loading(),
            mode: loading(),
            effort: loading(),
            thinking: loading(),
            context: loading(),
            quota: loading(),
        }
    }
}

fn compact_fact(ui: &mut Ui, theme: &Theme, label: &str, fact: &CompactFact) {
    let colour = match fact.state {
        RuntimeFactState::Observed => theme.text_dim,
        RuntimeFactState::Waiting
        | RuntimeFactState::Unsupported
        | RuntimeFactState::Unreported => theme.text_faint,
        RuntimeFactState::Stale => theme.provisional,
        RuntimeFactState::Failed => theme.failure,
    };
    // A fact is one scanning unit. Let the wrapped row move the whole unit to
    // the next line instead of splitting a value such as `54000/200000 tok`.
    ui.add(
        egui::Label::new(
            RichText::new(format!("{label}  {}", fact.value))
                .monospace()
                .small()
                .color(colour),
        )
        .extend(),
    );
}

fn agent_header_facts(agent: &AgentSummary, now_ms: i64) -> CompactAgentFacts {
    CompactAgentFacts {
        model: launch_header_fact(
            &agent.runtime,
            LaunchField::Model,
            agent.agent.model.as_deref(),
            now_ms,
        ),
        mode: launch_header_fact(
            &agent.runtime,
            LaunchField::PermissionMode,
            agent.permission_mode.as_deref(),
            now_ms,
        ),
        effort: launch_header_fact(&agent.runtime, LaunchField::EffortLevel, None, now_ms),
        thinking: launch_header_fact(&agent.runtime, LaunchField::ThinkingEnabled, None, now_ms),
        context: observation_header_fact(&agent.runtime.context, now_ms, |context| {
            format_measurement(&context.measurement)
        }),
        quota: observation_header_fact(&agent.runtime.quota, now_ms, format_quota_compact),
    }
}

#[derive(Clone, Copy)]
enum LaunchField {
    Model,
    PermissionMode,
    EffortLevel,
    ThinkingEnabled,
}

fn launch_field(configuration: &LaunchConfiguration, field: LaunchField) -> Option<String> {
    match field {
        LaunchField::Model => compact_model(configuration),
        LaunchField::PermissionMode => configuration.permission_mode.clone(),
        LaunchField::EffortLevel => configuration.effort_level.clone(),
        LaunchField::ThinkingEnabled => configuration
            .thinking_enabled
            .map(|enabled| if enabled { "enabled" } else { "disabled" }.to_string()),
    }
}

fn compact_model(configuration: &LaunchConfiguration) -> Option<String> {
    match (
        configuration.model.as_deref(),
        configuration.model_display_name.as_deref(),
    ) {
        (Some(model), _) => Some(model.to_string()),
        (None, Some(display_name)) => Some(display_name.to_string()),
        (None, None) => None,
    }
}

fn launch_header_fact(
    runtime: &AgentRuntimeMetadata,
    field: LaunchField,
    legacy: Option<&str>,
    now_ms: i64,
) -> CompactFact {
    let observations = [
        &runtime.launch.requested,
        &runtime.launch.effective,
        &runtime.launch.current,
    ];
    let mut values = Vec::<String>::new();
    for observation in observations {
        if let Some(value) = observation
            .value()
            .and_then(|configuration| launch_field(configuration, field))
            .filter(|value| !value.is_empty())
        {
            if values.last() != Some(&value) {
                values.push(value);
            }
        }
    }

    let current_state = observable_state_at(&runtime.launch.current, now_ms);
    if values.is_empty() {
        if let Some(legacy) = legacy.filter(|value| !value.is_empty()) {
            values.push(legacy.to_string());
        } else {
            let (value, state) = match current_state {
                RuntimeFactState::Observed => {
                    ("unreported".to_string(), RuntimeFactState::Unreported)
                }
                RuntimeFactState::Stale => {
                    ("stale · unreported".to_string(), RuntimeFactState::Stale)
                }
                RuntimeFactState::Waiting
                | RuntimeFactState::Unsupported
                | RuntimeFactState::Failed => {
                    (runtime_state_label(current_state).into(), current_state)
                }
                RuntimeFactState::Unreported => unreachable!("observations cannot be unreported"),
            };
            return CompactFact { value, state };
        }
    }

    let mut value = values.join("→");
    let current_reports_field = runtime
        .launch
        .current
        .value()
        .and_then(|configuration| launch_field(configuration, field))
        .is_some_and(|value| !value.is_empty());
    let state = if current_reports_field {
        match current_state {
            RuntimeFactState::Stale => {
                value.push_str(" · stale");
                RuntimeFactState::Stale
            }
            _ => RuntimeFactState::Observed,
        }
    } else {
        match current_state {
            RuntimeFactState::Observed => {
                value.push_str(" · current unreported");
                RuntimeFactState::Unreported
            }
            RuntimeFactState::Waiting
            | RuntimeFactState::Unsupported
            | RuntimeFactState::Stale
            | RuntimeFactState::Failed => {
                value.push_str(" · current ");
                value.push_str(runtime_state_label(current_state));
                current_state
            }
            RuntimeFactState::Unreported => unreachable!("observations cannot be unreported"),
        }
    };
    CompactFact { value, state }
}

fn observation_header_fact<T>(
    observation: &Observable<T>,
    now_ms: i64,
    render: impl FnOnce(&T) -> String,
) -> CompactFact {
    let state = observable_state_at(observation, now_ms);
    let value = match observation.value() {
        Some(value) => {
            let mut rendered = render(value);
            if state == RuntimeFactState::Stale {
                rendered.push_str(" · stale");
            }
            rendered
        }
        None => runtime_state_label(state).into(),
    };
    CompactFact { value, state }
}

fn observable_state_at<T>(observation: &Observable<T>, now_ms: i64) -> RuntimeFactState {
    if observation.is_stale_at(now_ms) {
        return RuntimeFactState::Stale;
    }
    match observation {
        Observable::Waiting => RuntimeFactState::Waiting,
        Observable::Observed { .. } => RuntimeFactState::Observed,
        Observable::Unsupported { .. } => RuntimeFactState::Unsupported,
        Observable::Stale { .. } => RuntimeFactState::Stale,
        Observable::Failed { .. } => RuntimeFactState::Failed,
    }
}

fn runtime_state_label(state: RuntimeFactState) -> &'static str {
    match state {
        RuntimeFactState::Observed => "observed",
        RuntimeFactState::Waiting => "waiting",
        RuntimeFactState::Unsupported => "unsupported",
        RuntimeFactState::Stale => "stale",
        RuntimeFactState::Failed => "failed",
        RuntimeFactState::Unreported => "unreported",
    }
}

fn format_quota_compact(quota: &QuotaSnapshot) -> String {
    let scope = quota.scope_label.as_deref().or(quota.scope_id.as_deref());
    match (scope, quota.windows.first()) {
        (Some(scope), Some(window)) => {
            format!(
                "{scope} · {} {}",
                window.label,
                format_measurement(&window.measurement)
            )
        }
        (None, Some(window)) => format!(
            "{} {}",
            window.label,
            format_measurement(&window.measurement)
        ),
        (Some(scope), None) => format!("{scope} · no windows reported"),
        (None, None) => "observed · no windows reported".into(),
    }
}

fn format_measurement(measurement: &UsageMeasurement) -> String {
    let amount = format_usage_amount(measurement.amount, &measurement.unit);
    let amount = match measurement.total {
        Some(total) => format!("{amount}/{}", format_usage_amount(total, &measurement.unit)),
        None => amount,
    };
    let semantics = match measurement.kind {
        UsageMeasurementKind::Used => "used",
        UsageMeasurementKind::Remaining => "remaining",
        UsageMeasurementKind::ProviderPercent => "provider",
    };
    format!("{amount} {semantics}")
}

fn format_usage_amount(amount: f64, unit: &UsageUnit) -> String {
    let number = if amount.fract() == 0.0 {
        format!("{amount:.0}")
    } else {
        format!("{amount:.1}")
    };
    match unit {
        UsageUnit::Tokens => format!("{number} tok"),
        UsageUnit::Percent => format!("{number}%"),
        UsageUnit::Requests => format!("{number} req"),
        UsageUnit::Credits => format!("{number} credits"),
        UsageUnit::Other(unit) => format!("{number} {unit}"),
    }
}

fn format_launch_configuration(configuration: &LaunchConfiguration) -> String {
    let mut facts = Vec::new();
    match (
        configuration.model.as_deref(),
        configuration.model_display_name.as_deref(),
    ) {
        (Some(model), Some(display_name)) if display_name != model => {
            facts.push(format!("model {model} ({display_name})"));
        }
        (Some(model), _) => facts.push(format!("model {model}")),
        (None, Some(display_name)) => facts.push(format!("model {display_name}")),
        (None, None) => {}
    }
    if let Some(mode) = configuration.permission_mode.as_deref() {
        facts.push(format!("permission {mode}"));
    }
    if let Some(mode) = configuration.approval_mode.as_deref() {
        facts.push(format!("approval {mode}"));
    }
    if let Some(mode) = configuration.sandbox_mode.as_deref() {
        facts.push(format!("sandbox {mode}"));
    }
    if let Some(effort) = configuration.effort_level.as_deref() {
        facts.push(format!("effort {effort}"));
    }
    if let Some(enabled) = configuration.thinking_enabled {
        facts.push(format!(
            "thinking {}",
            if enabled { "enabled" } else { "disabled" }
        ));
    }
    if !configuration.safe_flags.is_empty() {
        facts.push(format!("flags {}", configuration.safe_flags.join(" ")));
    }
    if facts.is_empty() {
        "no launch fields reported".into()
    } else {
        facts.join(" · ")
    }
}

fn inspector_runtime_observation<T>(
    ui: &mut Ui,
    theme: &Theme,
    label: &str,
    observation: &Observable<T>,
    now_ms: i64,
    render: impl FnOnce(&T) -> String,
) {
    let state = observable_state_at(observation, now_ms);
    let value = match observation {
        Observable::Observed { value, .. } | Observable::Stale { value, .. } => {
            format!("{} · {}", runtime_state_label(state), render(value))
        }
        Observable::Failed { message, .. } => format!("failed · {message}"),
        Observable::Waiting | Observable::Unsupported { .. } => {
            runtime_state_label(state).to_string()
        }
    };
    inspector_value(ui, theme, label, &value);

    if let Some(source) = observation.source() {
        let source = source.label.as_deref().unwrap_or(match source.kind {
            ObservationSourceKind::Unknown => "unknown source",
            ObservationSourceKind::LaunchRequest => "launch request",
            ObservationSourceKind::Adapter => "adapter",
            ObservationSourceKind::Provider => "provider",
            ObservationSourceKind::Process => "process",
            ObservationSourceKind::Cache => "cache",
        });
        inspector_value(ui, theme, &format!("{label} source"), source);
    }
    if let Some(observed_at_ms) = observation.observed_at_ms() {
        inspector_value(
            ui,
            theme,
            &format!("{label} observed"),
            &format_relative_time(observed_at_ms, now_ms),
        );
    }
    let expires_at_ms = match observation {
        Observable::Observed { expires_at_ms, .. } | Observable::Stale { expires_at_ms, .. } => {
            *expires_at_ms
        }
        Observable::Waiting | Observable::Unsupported { .. } | Observable::Failed { .. } => None,
    };
    if let Some(expires_at_ms) = expires_at_ms {
        inspector_value(
            ui,
            theme,
            &format!("{label} expires"),
            &format_relative_time(expires_at_ms, now_ms),
        );
    }
}

fn format_relative_time(timestamp_ms: i64, now_ms: i64) -> String {
    if timestamp_ms <= now_ms {
        format!(
            "{} ago",
            format_duration(now_ms.saturating_sub(timestamp_ms))
        )
    } else {
        format!(
            "in {}",
            format_duration(timestamp_ms.saturating_sub(now_ms))
        )
    }
}

#[allow(clippy::too_many_arguments)]
fn semantic_primary(
    ui: &mut Ui,
    theme: &Theme,
    rect: Rect,
    node: &TreeNodeView,
    safe_node: Option<&TreeNodeView>,
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
                match safe_node.and_then(|node| {
                    node.agent
                        .as_ref()
                        .and_then(|agent| agent.current_task.as_deref())
                }) {
                    Some(task) => {
                        ui.label(RichText::new(task).size(17.0).color(theme.text));
                    }
                    None if !node.is_agentic || safe_node.is_some() => {
                        inspector_empty(ui, theme, "No task has been reported for this node")
                    }
                    None => inspector_empty(ui, theme, "Loading safe task details…"),
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

                if let Some(last) = safe_node.and_then(|node| {
                    node.agent
                        .as_ref()
                        .and_then(|agent| agent.last_message.as_deref())
                }) {
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

fn inspected_node_for<'a>(
    details: Option<&'a InspectorDetails>,
    node_id: &NodeId,
) -> Option<&'a TreeNodeView> {
    match details {
        Some(InspectorDetails::Agent { node, .. })
        | Some(InspectorDetails::Process { node, .. })
            if node.node_id == *node_id =>
        {
            Some(node)
        }
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn node_details(
    ui: &mut Ui,
    theme: &Theme,
    rect: Rect,
    snapshot: &HierarchySnapshot,
    session: &SessionTreeView,
    node: &TreeNodeView,
    safe_node: Option<&TreeNodeView>,
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
                inspector_value(ui, theme, "state", node.display_state.label());
                if let Some(safe_node) = safe_node {
                    inspector_value(
                        ui,
                        theme,
                        "lifecycle",
                        lifecycle_label(&safe_node.lifecycle),
                    );
                } else {
                    inspector_value(ui, theme, "lifecycle", "Loading safe details…");
                }
                inspector_value(ui, theme, "runtime", &format_duration(node.runtime_ms));
                inspector_optional_owned(ui, theme, "pid", node.pid.map(|pid| pid.to_string()));
                if let Some(agent) = safe_node.and_then(|node| node.agent.as_ref()) {
                    inspector_optional(ui, theme, "provider", agent.agent.provider.as_deref());
                    inspector_optional(ui, theme, "tool", agent.agent.tool.as_deref());
                    inspector_optional(ui, theme, "Agent type", agent.agent_type.as_deref());
                    inspector_optional(ui, theme, "branch", agent.git_branch.as_deref());
                } else if node.is_agentic {
                    inspector_value(ui, theme, "agent metadata", "Loading safe details…");
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

                if let Some(agent) = safe_node.and_then(|node| node.agent.as_ref()) {
                    inspector_section(ui, theme, "LAUNCH RECEIPT");
                    inspector_runtime_observation(
                        ui,
                        theme,
                        "requested",
                        &agent.runtime.launch.requested,
                        now_ms,
                        format_launch_configuration,
                    );
                    inspector_runtime_observation(
                        ui,
                        theme,
                        "effective",
                        &agent.runtime.launch.effective,
                        now_ms,
                        format_launch_configuration,
                    );
                    inspector_runtime_observation(
                        ui,
                        theme,
                        "current",
                        &agent.runtime.launch.current,
                        now_ms,
                        format_launch_configuration,
                    );

                    inspector_section(ui, theme, "CONVERSATION CONTEXT");
                    inspector_runtime_observation(
                        ui,
                        theme,
                        "usage",
                        &agent.runtime.context,
                        now_ms,
                        |context| format_measurement(&context.measurement),
                    );
                    if let Some(context) = agent.runtime.context.value() {
                        inspector_optional(ui, theme, "scope", context.scope_id.as_deref());
                        inspector_optional_owned(
                            ui,
                            theme,
                            "effective window",
                            context
                                .effective_window
                                .as_ref()
                                .map(format_measurement),
                        );
                    }

                    inspector_section(ui, theme, "PROVIDER QUOTA");
                    inspector_runtime_observation(
                        ui,
                        theme,
                        "quota",
                        &agent.runtime.quota,
                        now_ms,
                        format_quota_compact,
                    );
                    if let Some(quota) = agent.runtime.quota.value() {
                        inspector_optional(ui, theme, "scope id", quota.scope_id.as_deref());
                        inspector_optional(ui, theme, "scope", quota.scope_label.as_deref());
                        for window in &quota.windows {
                            let value = format_quota_window(window, now_ms);
                            inspector_value(ui, theme, &window.label, &value);
                        }
                    }

                    if agent.tokens_used.is_some() || agent.cost_usd.is_some() {
                        inspector_section(ui, theme, "SESSION TOTALS");
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
                if node.is_agentic {
                    let safe_agent_node = safe_node.filter(|node| node.is_agentic);
                    let rename = ui
                        .add_enabled(safe_agent_node.is_some(), egui::Button::new("Rename Agent…"))
                        .on_disabled_hover_text("Loading safe Agent details…");
                    if rename.clicked() {
                        // The same Enter that activates this button must not immediately
                        // submit the editor that is drawn later in this frame.
                        ui.input_mut(|input| input.consume_key(Modifiers::NONE, Key::Enter));
                        state.node_edit = Some(super::NodeEditDraft::rename(
                            safe_agent_node.expect("the enabled action has safe Agent details"),
                        ));
                    }
                    let relationship = ui
                        .add_enabled(
                            safe_agent_node.is_some(),
                            egui::Button::new("Correct relationship…"),
                        )
                        .on_disabled_hover_text("Loading safe Agent details…");
                    if relationship.clicked() {
                        ui.input_mut(|input| input.consume_key(Modifiers::NONE, Key::Enter));
                        state.node_edit = Some(super::NodeEditDraft::relationship(
                            safe_agent_node.expect("the enabled action has safe Agent details"),
                        ));
                    }
                    let has_target = session.nodes.iter().any(|candidate| {
                        candidate.is_agentic && candidate.node_id != node.node_id
                    });
                    let handoff = ui
                        .add_enabled(
                            safe_agent_node.is_some() && has_target,
                            egui::Button::new("Pass context to Agent…"),
                        )
                        .on_disabled_hover_text(if safe_agent_node.is_none() {
                            "Loading safe Agent details…"
                        } else {
                            "This Session has no other Agent to receive context."
                        });
                    if handoff.clicked() {
                        ui.input_mut(|input| input.consume_key(Modifiers::NONE, Key::Enter));
                        state.context_handoff = Some(super::ContextHandoffDraft::new(
                            session,
                            safe_agent_node.expect("the enabled action has safe Agent details"),
                        ));
                    }
                }
            });
    });
}

fn format_quota_window(window: &QuotaWindow, now_ms: i64) -> String {
    let mut value = format_measurement(&window.measurement);
    if let Some(reset) = window.resets_at_ms {
        value.push_str(" · reset ");
        value.push_str(&format_relative_time(reset, now_ms));
    }
    if window.exhausted == Some(true) {
        value.push_str(" · exhausted");
    }
    match window.hard_limit {
        Some(true) => value.push_str(" · hard limit"),
        Some(false) => value.push_str(" · soft limit"),
        None if window.exhausted == Some(true) => value.push_str(" · hardness unknown"),
        None => {}
    }
    value
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

fn attention_copy(node: &TreeNodeView, safe_node: Option<&TreeNodeView>) -> String {
    if let Some(agent) = safe_node.and_then(|node| node.agent.as_ref()) {
        if let Some(permission) = &agent.pending_permission {
            return format!("NEEDS YOU · PERMISSION · {}", permission.summary);
        }
        if let Some(question) = agent.pending_question.as_deref() {
            return format!("NEEDS YOU · QUESTION · {question}");
        }
    }
    let kind = match node.turn.as_ref() {
        Some(Turn::AwaitingUser {
            reason: AwaitingReason::Permission,
        }) => "PERMISSION",
        Some(Turn::AwaitingUser {
            reason: AwaitingReason::Question,
        }) => "QUESTION",
        Some(Turn::AwaitingUser {
            reason: AwaitingReason::Credentials,
        }) => "CREDENTIALS",
        Some(Turn::AwaitingUser {
            reason: AwaitingReason::Input,
        }) => "INPUT",
        _ => node.display_state.label(),
    };
    if safe_node.is_none() {
        format!("NEEDS YOU · {kind} · Loading safe details…")
    } else {
        format!("NEEDS YOU · {kind}")
    }
}

/// Agent names are free text. Until the exact inspector row arrives, the right-hand
/// surface identifies the structural kind but never borrows the low-latency row title.
fn work_surface_title<'a>(node: &'a TreeNodeView, safe_node: Option<&'a TreeNodeView>) -> &'a str {
    safe_node.map(process_title).unwrap_or(if node.is_agentic {
        "Agent details loading…"
    } else {
        "Process details loading…"
    })
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

    fn source() -> turn_core::model::ObservationSource {
        turn_core::model::ObservationSource::new(
            ObservationSourceKind::Provider,
            "provider runtime",
        )
    }

    #[test]
    fn compact_header_promotes_launch_mismatch_and_current_unsupported_state() {
        let mut node = ProcessNode::agent(
            SessionId::from_stored("session-runtime"),
            "claude",
            "/repo",
            T0,
        );
        let runtime = &mut node.agent.as_mut().unwrap().runtime;
        runtime.launch.requested = Observable::observed(
            LaunchConfiguration {
                model: Some("sonnet".into()),
                permission_mode: Some("default".into()),
                effort_level: Some("medium".into()),
                thinking_enabled: Some(false),
                ..LaunchConfiguration::default()
            },
            source(),
            T0,
            None,
        );
        runtime.launch.effective = Observable::observed(
            LaunchConfiguration {
                model: Some("opus".into()),
                permission_mode: Some("bypass".into()),
                effort_level: Some("high".into()),
                thinking_enabled: Some(true),
                ..LaunchConfiguration::default()
            },
            source(),
            T0 + 1,
            None,
        );
        runtime.launch.current = Observable::unsupported(source(), T0 + 2);

        let facts = agent_header_facts(&AgentSummary::from_node(&node, T0).unwrap(), T0);
        assert_eq!(facts.model.value, "sonnet→opus · current unsupported");
        assert_eq!(facts.model.state, RuntimeFactState::Unsupported);
        assert_eq!(facts.mode.value, "default→bypass · current unsupported");
        assert_eq!(facts.effort.value, "medium→high · current unsupported");
        assert_eq!(facts.effort.state, RuntimeFactState::Unsupported);
        assert_eq!(
            facts.thinking.value,
            "disabled→enabled · current unsupported"
        );
        assert_eq!(facts.thinking.state, RuntimeFactState::Unsupported);
    }

    #[test]
    fn compact_launch_facts_keep_waiting_stale_and_unreported_distinct() {
        let mut node = ProcessNode::agent(
            SessionId::from_stored("session-launch-states"),
            "claude",
            "/repo",
            T0,
        );
        let runtime = &mut node.agent.as_mut().unwrap().runtime;
        let requested = LaunchConfiguration {
            effort_level: Some("high".into()),
            thinking_enabled: Some(true),
            ..LaunchConfiguration::default()
        };
        runtime.launch.requested = Observable::observed(requested.clone(), source(), T0, None);
        runtime.launch.effective = Observable::observed(requested.clone(), source(), T0 + 1, None);

        let mut summary = AgentSummary::from_node(&node, T0).unwrap();
        let waiting = agent_header_facts(&summary, T0 + 2);
        assert_eq!(waiting.effort.value, "high · current waiting");
        assert_eq!(waiting.effort.state, RuntimeFactState::Waiting);
        assert_eq!(waiting.thinking.value, "enabled · current waiting");
        assert_eq!(waiting.thinking.state, RuntimeFactState::Waiting);

        summary.runtime.launch.current = Observable::stale(
            LaunchConfiguration {
                effort_level: Some("xhigh".into()),
                thinking_enabled: Some(false),
                ..LaunchConfiguration::default()
            },
            source(),
            T0 + 2,
            Some(T0 + 3),
        );
        let stale = agent_header_facts(&summary, T0 + 3);
        assert_eq!(stale.effort.value, "high→xhigh · stale");
        assert_eq!(stale.effort.state, RuntimeFactState::Stale);
        assert_eq!(stale.thinking.value, "enabled→disabled · stale");
        assert_eq!(stale.thinking.state, RuntimeFactState::Stale);

        summary.runtime.launch.current =
            Observable::observed(LaunchConfiguration::default(), source(), T0 + 4, None);
        let unreported = agent_header_facts(&summary, T0 + 4);
        assert_eq!(
            unreported.effort.value, "high · current unreported",
            "an observed partial sample must not invent a current effort"
        );
        assert_eq!(unreported.effort.state, RuntimeFactState::Unreported);
        assert_eq!(unreported.thinking.value, "enabled · current unreported");
        assert_eq!(unreported.thinking.state, RuntimeFactState::Unreported);

        summary.runtime.launch.requested = Observable::Waiting;
        summary.runtime.launch.effective = Observable::Waiting;
        let absent = agent_header_facts(&summary, T0 + 4);
        assert_eq!(absent.effort.value, "unreported");
        assert_eq!(absent.effort.state, RuntimeFactState::Unreported);
        assert_eq!(absent.thinking.value, "unreported");
        assert_eq!(absent.thinking.state, RuntimeFactState::Unreported);

        summary.runtime.launch.current = Observable::stale(
            LaunchConfiguration::default(),
            source(),
            T0 + 5,
            Some(T0 + 6),
        );
        let absent_stale = agent_header_facts(&summary, T0 + 6);
        assert_eq!(absent_stale.effort.value, "stale · unreported");
        assert_eq!(absent_stale.effort.state, RuntimeFactState::Stale);
        assert_eq!(absent_stale.thinking.value, "stale · unreported");
        assert_eq!(absent_stale.thinking.state, RuntimeFactState::Stale);
    }

    #[test]
    fn compact_model_uses_display_only_and_keeps_id_and_label_unambiguous() {
        let mut node = ProcessNode::agent(
            SessionId::from_stored("session-model-display"),
            "claude",
            "/repo",
            T0,
        );
        let runtime = &mut node.agent.as_mut().unwrap().runtime;
        runtime.launch.current = Observable::observed(
            LaunchConfiguration {
                model_display_name: Some("Opus".into()),
                ..LaunchConfiguration::default()
            },
            source(),
            T0,
            None,
        );

        let mut summary = AgentSummary::from_node(&node, T0).unwrap();
        let display_only = agent_header_facts(&summary, T0);
        assert_eq!(display_only.model.value, "Opus");
        assert_eq!(display_only.model.state, RuntimeFactState::Observed);

        summary.runtime.launch.current = Observable::observed(
            LaunchConfiguration {
                model: Some("claude-opus-5".into()),
                model_display_name: Some("Opus 5".into()),
                ..LaunchConfiguration::default()
            },
            source(),
            T0 + 1,
            None,
        );
        let identified = agent_header_facts(&summary, T0 + 1);
        assert_eq!(identified.model.value, "claude-opus-5");
        assert_eq!(identified.model.state, RuntimeFactState::Observed);

        summary.runtime.launch.current = Observable::observed(
            LaunchConfiguration {
                model: Some("same-name".into()),
                model_display_name: Some("same-name".into()),
                ..LaunchConfiguration::default()
            },
            source(),
            T0 + 2,
            None,
        );
        assert_eq!(
            agent_header_facts(&summary, T0 + 2).model.value,
            "same-name",
            "an identical provider label must not be repeated"
        );

        summary.runtime.launch.requested = Observable::observed(
            LaunchConfiguration {
                model: Some("claude-opus-5".into()),
                ..LaunchConfiguration::default()
            },
            source(),
            T0,
            None,
        );
        summary.runtime.launch.effective = Observable::observed(
            LaunchConfiguration {
                model: Some("claude-opus-5".into()),
                ..LaunchConfiguration::default()
            },
            source(),
            T0 + 1,
            None,
        );
        summary.runtime.launch.current = Observable::observed(
            LaunchConfiguration {
                model: Some("claude-opus-5".into()),
                model_display_name: Some("Opus 5".into()),
                ..LaunchConfiguration::default()
            },
            source(),
            T0 + 2,
            None,
        );
        assert_eq!(
            agent_header_facts(&summary, T0 + 2).model.value,
            "claude-opus-5",
            "adding a display label to the same id is enrichment, not a model transition"
        );
    }

    #[test]
    fn launch_receipt_formats_every_safe_field_without_omissions() {
        let configuration = LaunchConfiguration {
            model: Some("claude-opus-5".into()),
            model_display_name: Some("Opus 5".into()),
            permission_mode: Some("Autonomous".into()),
            approval_mode: Some("bypassed".into()),
            sandbox_mode: Some("disabled".into()),
            effort_level: Some("high".into()),
            thinking_enabled: Some(true),
            safe_flags: vec!["--model".into(), "--permission-mode".into()],
        };

        assert_eq!(
            format_launch_configuration(&configuration),
            "model claude-opus-5 (Opus 5) · permission Autonomous · approval bypassed · \
             sandbox disabled · effort high · thinking enabled · flags --model \
             --permission-mode"
        );
        assert_eq!(
            format_launch_configuration(&LaunchConfiguration::default()),
            "no launch fields reported"
        );
    }

    #[test]
    fn compact_capacity_facts_keep_observed_stale_and_unsupported_distinct() {
        use turn_core::model::{ContextUsageSnapshot, QuotaWindow};

        let mut node = ProcessNode::agent(
            SessionId::from_stored("session-capacity"),
            "codex",
            "/repo",
            T0,
        );
        let runtime = &mut node.agent.as_mut().unwrap().runtime;
        runtime.context = Observable::stale(
            ContextUsageSnapshot {
                scope_id: Some("conversation".into()),
                measurement: UsageMeasurement {
                    kind: UsageMeasurementKind::Used,
                    amount: 42_000.0,
                    unit: UsageUnit::Tokens,
                    total: None,
                },
                effective_window: None,
                window_size_tokens: None,
                used_percentage: None,
                remaining_percentage: None,
                current_usage: None,
            },
            source(),
            T0,
            Some(T0 + 1),
        );
        runtime.quota = Observable::observed(
            QuotaSnapshot {
                scope_id: Some("account".into()),
                scope_label: Some("team".into()),
                windows: vec![QuotaWindow {
                    label: "five hour".into(),
                    measurement: UsageMeasurement {
                        kind: UsageMeasurementKind::ProviderPercent,
                        amount: 61.0,
                        unit: UsageUnit::Percent,
                        total: None,
                    },
                    resets_at_ms: None,
                    exhausted: None,
                    hard_limit: None,
                }],
            },
            source(),
            T0,
            None,
        );

        let mut summary = AgentSummary::from_node(&node, T0).unwrap();
        let facts = agent_header_facts(&summary, T0);
        assert_eq!(facts.context.value, "42000 tok used · stale");
        assert_eq!(facts.context.state, RuntimeFactState::Stale);
        assert_eq!(facts.quota.value, "team · five hour 61% provider");
        assert_eq!(facts.quota.state, RuntimeFactState::Observed);
        assert!(
            !facts.context.value.contains('%') && !facts.context.value.contains("remaining"),
            "no total means no derived percentage or complement"
        );

        summary.runtime.quota = Observable::unsupported(source(), T0 + 1);
        let facts = agent_header_facts(&summary, T0);
        assert_eq!(facts.quota.value, "unsupported");
        assert_eq!(facts.quota.state, RuntimeFactState::Unsupported);
    }

    #[test]
    fn compact_capacity_facts_age_after_a_received_projection_expires() {
        use turn_core::model::ContextUsageSnapshot;

        let mut node = ProcessNode::agent(
            SessionId::from_stored("session-live-expiry"),
            "claude",
            "/repo",
            T0,
        );
        let runtime = &mut node.agent.as_mut().unwrap().runtime;
        runtime.context = Observable::observed(
            ContextUsageSnapshot {
                scope_id: Some("conversation".into()),
                measurement: UsageMeasurement {
                    kind: UsageMeasurementKind::Used,
                    amount: 42.0,
                    unit: UsageUnit::Tokens,
                    total: None,
                },
                effective_window: None,
                window_size_tokens: None,
                used_percentage: None,
                remaining_percentage: None,
                current_usage: None,
            },
            source(),
            T0,
            Some(T0 + 10),
        );
        runtime.quota = Observable::observed(
            QuotaSnapshot {
                scope_id: Some("account".into()),
                scope_label: None,
                windows: Vec::new(),
            },
            source(),
            T0,
            Some(T0 + 10),
        );
        let summary = AgentSummary::from_node(&node, T0).unwrap();
        assert!(matches!(
            summary.runtime.context,
            Observable::Observed { .. }
        ));

        let facts = agent_header_facts(&summary, T0 + 10);
        assert_eq!(facts.context.state, RuntimeFactState::Stale);
        assert_eq!(facts.context.value, "42 tok used · stale");
        assert_eq!(facts.quota.state, RuntimeFactState::Stale);
        assert_eq!(facts.quota.value, "account · no windows reported · stale");
    }

    #[test]
    fn exhausted_provider_limit_does_not_claim_unknown_hardness() {
        let mut window = QuotaWindow {
            label: "5h".into(),
            measurement: UsageMeasurement {
                kind: UsageMeasurementKind::Remaining,
                amount: 0.0,
                unit: UsageUnit::Percent,
                total: Some(100.0),
            },
            resets_at_ms: None,
            exhausted: Some(true),
            hard_limit: None,
        };

        assert_eq!(
            format_quota_window(&window, T0),
            "0%/100% remaining · exhausted · hardness unknown"
        );
        window.hard_limit = Some(true);
        assert_eq!(
            format_quota_window(&window, T0),
            "0%/100% remaining · exhausted · hard limit"
        );
    }
}
