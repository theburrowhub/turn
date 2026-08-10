//! Snapshot and behaviour tests for the window.
//!
//! These render the real widget tree through `wgpu` with no display attached and compare
//! the result against a committed PNG. That is what makes a native GPU frontend
//! reviewable at all: without it, "the window looks right" is a claim nobody can check in
//! CI, and the previous frontend's visual behaviour could only be verified by a human
//! opening a browser.
//!
//! Run `UPDATE_SNAPSHOTS=1 cargo test -p turn-gui` to re-record after an intentional
//! visual change; the diff is then a reviewable image.
//!
//! Two of the tests here are not snapshots and are the more important half:
//!
//! * `an_idle_window_settles_instead_of_repainting_in_a_loop` measures the product's most
//!   explicit performance criterion. `Harness::run` returns the number of frames it took
//!   before nothing asked for another one, so a window that repainted continuously would
//!   run to the harness's step limit and fail rather than quietly burning a core.
//! * `every_hierarchy_level_is_a_reachable_tree_item` drives the same AccessKit tree a
//!   screen reader would read. A GPU-drawn terminal has no DOM, so this is the one
//!   requirement no snapshot can cover.

use std::collections::{BTreeMap, HashMap};

use egui_kittest::kittest::{NodeT, Queryable};
use egui_kittest::Harness;
use turn_core::event::{AgentRef, Confidence, Risk};
use turn_core::ids::{AttentionId, CheckoutId, NodeId, PaneId, SessionId, WorkspaceId};
use turn_core::model::{
    ActivityPreview, AgentName, Direction, DropZone, Layout, LeaseState, NodeKind, Pane, PaneKind,
    PaneNodeBinding, PreviewSource, ProcessNode, Relation, RestoreState, Session, SessionMode,
    Template, Workspace, WorkspaceCheckout, WorkspaceWriteLease,
};
use turn_core::state::{AwaitingReason, DisplayState, Lifecycle, Turn};
use turn_proto::cells::{Cell, CellAttrs, Grid, Rgb};
use turn_proto::{
    CloseDisposition, HierarchyKey, HierarchySnapshot, NodePaneCapability, NodePaneView,
    PaneRestoreOutcome, ProtoErrorContext, PtySize, SessionConflictAlternative, SessionSummary,
    SessionTreeView, TemplateSummary, TreeNodeView, TreeSurfaceState, Welcome, WorkspaceSummary,
    WorkspaceTreeView, WriteLeaseOwnerView,
};

use turn_gui::keymap::{Keymap, Overrides, Platform};
use turn_gui::terminal::menu::{MenuItem, PaneCommand, PaneContext, PaneMenu, PaneShortcuts};
use turn_gui::terminal::selection::{CellPos, Selection, SelectionKind};
use turn_gui::terminal::{PaneAction, PaneInteraction, PaneOptions};
use turn_gui::theme::Theme;
use turn_gui::transport::{ConnectionState, DaemonIdentity};
use turn_gui::view::{
    LayoutEditorOrigin, LayoutTemplateDraft, LifecycleConfirmation, PaneContent, PendingPermission,
    QueueItem, SessionDraft, SessionRestoreView, SessionRow, TemporaryPaneContent, TurnView,
    ViewAction, ViewState, WorkspaceDraft,
};

const T0: i64 = 1_700_000_000_000;

/// A moment where a blinking cursor is in its visible phase.
///
/// The cursor blinks as a function of the clock, so a snapshot taken at an arbitrary
/// instant catches it off half the time and the recorded image would flap.
fn cursor_on() -> i64 {
    T0 - T0.rem_euclid(2 * turn_gui::repaint::CURSOR_BLINK.as_millis() as i64)
}

/// A window state, owning its screens so the view can borrow them.
///
/// Owning them here rather than in [`TurnView`] is the same decision the application
/// makes: a grid is the largest thing on screen and cloning one per pane per frame would
/// undo the work the run encoding does.
#[derive(Default)]
struct Fixture {
    /// The normative Workspace -> Session -> Process navigation projection.
    hierarchy: Option<HierarchySnapshot>,
    workspaces: Vec<WorkspaceSummary>,
    templates: Vec<TemplateSummary>,
    sessions: Vec<SessionRow>,
    selected: Option<SessionId>,
    layout: Option<Layout>,
    grids: BTreeMap<PaneId, Grid>,
    titles: BTreeMap<PaneId, String>,
    focused: Option<PaneId>,
    scrolled: Vec<PaneId>,
    incomplete_history: Vec<PaneId>,
    permission: Option<PendingPermission>,
    queue: Vec<QueueItem>,
    connection: Option<ConnectionState>,
    notice: Option<String>,
    preview_history: HashMap<NodeId, Vec<ActivityPreview>>,
    temporary_pane: Option<NodePaneView>,
    temporary_previews: Vec<ActivityPreview>,
    write_conflict: Option<ProtoErrorContext>,
    link_confirmation: Option<turn_gui::terminal::links::LinkRequest>,
    settings: Option<turn_proto::SettingsView>,
    restore: Option<SessionRestoreView>,
    recovery_lease: Option<WorkspaceWriteLease>,
    /// The Settings preference. Off by default, as it is in the window, so a fixture that
    /// contains an archived row has to say it wants it shown.
    include_archived: bool,
}

impl Fixture {
    fn view(&self) -> TurnView<'_> {
        let panes = self
            .grids
            .iter()
            .map(|(pane_id, grid)| PaneContent {
                pane_id: pane_id.clone(),
                title: self
                    .titles
                    .get(pane_id)
                    .cloned()
                    .unwrap_or_else(|| "shell".to_string()),
                grid,
                focused: self.focused.as_ref() == Some(pane_id),
                scrolled: self.scrolled.contains(pane_id),
                history_complete: !self.incomplete_history.contains(pane_id),
            })
            .collect();
        let temporary_pane = self.temporary_pane.as_ref().map(|pane| {
            let node = self.hierarchy.as_ref().and_then(|snapshot| {
                snapshot
                    .workspaces
                    .iter()
                    .flat_map(|workspace| &workspace.sessions)
                    .flat_map(|session| &session.nodes)
                    .find(|node| node.node_id == pane.binding.node_id)
            });
            TemporaryPaneContent {
                pane,
                node,
                previews: &self.temporary_previews,
                grid: self.grids.get(&pane.binding.pane_id),
            }
        });
        TurnView {
            workspaces: &self.workspaces,
            templates: &self.templates,
            sessions: self.sessions.clone(),
            selected: self.selected.clone(),
            layout: self.layout.clone(),
            panes,
            temporary_pane,
            restore: self.restore.as_ref(),
            recovery_lease: self.recovery_lease.as_ref(),
            unreachable_processes: 0,
            relaunching: Vec::new(),
            reclaiming_workspaces: Vec::new(),
            reclaiming_write_access: false,
            permission: self.permission.clone(),
            queue: self.queue.clone(),
            connection: self.connection.clone(),
            notice: self.notice.clone(),
            write_conflict: self.write_conflict.as_ref(),
            link_confirmation: self.link_confirmation.as_ref(),
            settings: self.settings.as_ref(),
            include_archived: self.include_archived,
            policy: None,
            now_ms: cursor_on(),
        }
    }
}

/// Everything one harness needs.
struct Window {
    fixture: Fixture,
    state: ViewState,
    theme: Theme,
    keymap: Keymap,
    actions: Vec<ViewAction>,
}

fn window(fixture: Fixture) -> Window {
    let mut state = ViewState::default();
    state.hierarchy = fixture.hierarchy.clone();
    state.preview_history = fixture.preview_history.clone();
    Window {
        fixture,
        state,
        theme: Theme::dark(),
        keymap: Keymap::build(&Overrides::new(), Platform::MAC),
        actions: Vec::new(),
    }
}

fn harness(fixture: Fixture) -> Harness<'static, Window> {
    Harness::builder()
        .with_size(egui::vec2(1280.0, 760.0))
        .build_ui_state(
            |ui, window| {
                let Window {
                    fixture,
                    state,
                    theme,
                    keymap,
                    actions,
                } = window;
                theme.install(ui.ctx());
                actions.extend(fixture.view().ui(ui, theme, keymap, state));
            },
            window(fixture),
        )
}

fn connected() -> ConnectionState {
    DaemonIdentity::new().observe(&Welcome::new(1, "0.1.0", 51234, T0))
}

fn workspace_without_sessions() -> Fixture {
    let starter = Template::two_shells(T0);
    let template = TemplateSummary::from_template(&starter);
    let mut workspace = Workspace::new("turn", "/Users/x/personal-workspace/turn", T0);
    workspace.id = WorkspaceId::from_stored("ws_onboarding_turn");
    workspace.default_template = Some(starter.id.clone());
    let summary = WorkspaceSummary::from_workspace(&workspace, &[]);
    Fixture {
        hierarchy: Some(HierarchySnapshot {
            revision: 2,
            tree_state: TreeSurfaceState {
                surface_id: "window-snapshot".into(),
                selected: Some(HierarchyKey::workspace(workspace.id.clone())),
                expanded: vec![HierarchyKey::workspace(workspace.id)],
            },
            workspaces: vec![WorkspaceTreeView {
                workspace: summary.clone(),
                checkouts: Vec::new(),
                write_lease: None,
                sessions: Vec::new(),
            }],
        }),
        workspaces: vec![summary],
        templates: vec![template],
        connection: Some(connected()),
        ..Fixture::default()
    }
}

/// A grid of a fixed height, padded like a real terminal screen.
fn screen(lines: &[&str], rows: u16, cols: u16) -> Grid {
    let mut grid = Grid::blank(rows, cols);
    for (row, line) in lines.iter().enumerate().take(rows as usize) {
        for (col, ch) in line.chars().enumerate().take(cols as usize) {
            if let Some(cell) = grid.cell_mut(row as u16, col as u16) {
                cell.text = ch.to_string();
            }
        }
    }
    grid
}

/// An agent's screen, with colour and attributes so the snapshot proves they are painted.
fn agent_screen() -> Grid {
    let mut grid = screen(
        &[
            "$ claude",
            "I'll fix the climbing bug. Running the tests first.",
            "",
            "  Bash(cargo test -p physics)",
            "  Do you want to allow this?  (y/n)",
            "",
            "❯ ",
        ],
        40,
        56,
    );
    for col in 2..10u16 {
        if let Some(cell) = grid.cell_mut(3, col) {
            cell.fg = Some(Rgb::new(0x6a, 0x9e, 0xd8));
            cell.attrs = CellAttrs::default().with(CellAttrs::BOLD);
        }
    }
    if let Some(cell) = grid.cell_mut(4, 2) {
        *cell = Cell {
            text: "D".into(),
            fg: Some(Rgb::new(0x0d, 0x0f, 0x12)),
            bg: Some(Rgb::new(0xe8, 0xa8, 0x3a)),
            attrs: CellAttrs::default(),
        };
    }
    for col in 0..12u16 {
        if let Some(cell) = grid.cell_mut(1, col) {
            cell.attrs = CellAttrs::default().with(CellAttrs::UNDERLINE);
        }
    }
    // A wide glyph, so the snapshot shows an emoji does not shift the rest of the row.
    grid.set_wide(5, 2, "🔥");
    grid.cursor = Some((6, 2));
    grid
}

fn permission(risk: Risk, provisional: bool) -> PendingPermission {
    PendingPermission {
        attention_id: Some(AttentionId::from_stored("att_snapshot001")),
        session_id: SessionId::from_stored("sess_fixclimbing"),
        session: "Fix climbing bugs".into(),
        summary: "Run `cargo test -p physics`".into(),
        command: Some("cargo test -p physics -- --nocapture".into()),
        cwd: "/Users/x/space-troopers".into(),
        tool: "Bash".into(),
        risk,
        blocked_secs: 47,
        provisional,
    }
}

fn queue_item(
    name: &str,
    reason: AwaitingReason,
    actionable: bool,
    provisional: bool,
) -> QueueItem {
    QueueItem {
        attention_id: AttentionId::new(),
        session_id: SessionId::from_stored(format!("sess_{:0>11}", name.len())),
        session_name: name.into(),
        reason,
        summary: Some("waiting on you".into()),
        provisional,
        actionable,
    }
}

/// A layout of three panes: one tall on the left, two stacked on the right.
fn three_pane_layout() -> (Layout, Vec<PaneId>) {
    let agent = Pane::new(PaneKind::Agent).with_title("claude · agent · sonnet");
    let mut layout = Layout::single(agent);
    let first = layout.panes()[0].id.clone();
    layout.split(
        &first,
        Direction::Horizontal,
        Pane::new(PaneKind::Tui).with_title("fang · files"),
    );
    let second = layout.panes()[1].id.clone();
    layout.split(
        &second,
        Direction::Vertical,
        Pane::new(PaneKind::Shell).with_title("zsh"),
    );
    layout.active = Some(first.clone());
    let ids = layout
        .panes()
        .into_iter()
        .map(|pane| pane.id.clone())
        .collect();
    (layout, ids)
}

struct UnifiedHierarchy {
    snapshot: HierarchySnapshot,
    session_id: SessionId,
    reviewer_id: NodeId,
    workspace_id: WorkspaceId,
    checkout_id: CheckoutId,
    lease: WorkspaceWriteLease,
}

fn add_preview(
    node: &mut ProcessNode,
    text: &str,
    source: PreviewSource,
    confidence: Confidence,
) -> ActivityPreview {
    let preview = ActivityPreview {
        node_id: node.id.clone(),
        raw_source_sequence: Some(42),
        normalized_text: text.into(),
        source,
        confidence,
        stable: true,
        contains_sensitive_data: false,
        redacted: false,
        updated_ms: T0 + 12_000,
    };
    node.activity_preview = Some(preview.clone());
    preview
}

/// A production-shaped hierarchy fixture. It is built from domain entities and projected
/// through the same protocol views as `turnd`; no `SessionRow` or second agent tree is
/// fabricated for the screenshot.
fn unified_hierarchy(layout: &Layout, panes: &[PaneId]) -> UnifiedHierarchy {
    let mut workspace = Workspace::new(
        "space-troopers",
        "/Users/x/personal-workspace/space-troopers",
        T0,
    );
    workspace.id = WorkspaceId::from_stored("ws_space_troopers");
    workspace.default_agent = Some("claude".into());
    let checkout_id = CheckoutId::from_stored("checkout_space_primary");

    let mut session = Session::new(
        workspace.id.clone(),
        "Fix climbing bugs",
        workspace.root.clone(),
        layout.clone(),
        T0,
    );
    session.id = SessionId::from_stored("sess_fixclimbing");
    session.mode = SessionMode::MainCheckout;
    session.checkout_id = checkout_id.clone();
    session.git_branch = Some("fix/climbing-bugs".into());
    session.last_activity_ms = T0 + 12_000;

    let mut claude = ProcessNode::agent(
        session.id.clone(),
        "claude",
        session.cwd.clone(),
        T0 + 1_000,
    );
    claude.id = NodeId::from_stored("agent_claude_main");
    claude.lifecycle = Lifecycle::Alive;
    claude.turn = Some(Turn::AwaitingUser {
        reason: AwaitingReason::Permission,
    });
    claude.interaction_pending = true;
    let claude_info = claude.agent.as_mut().expect("agent detail");
    claude_info.name = AgentName::declared("Claude Code");
    claude_info.agent = AgentRef {
        provider: Some("anthropic".into()),
        tool: Some("claude-code".into()),
        model: Some("claude-3.5-sonnet".into()),
        external_id: Some("claude-main".into()),
    };
    claude_info.current_task = Some("Fix the climbing transition and verify it".into());
    add_preview(
        &mut claude,
        "Would you like me to commit these changes?",
        PreviewSource::SemanticEvent,
        Confidence::Explicit,
    );
    let claude_id = session.tree.insert(claude);

    let mut reviewer = ProcessNode::agent(
        session.id.clone(),
        "claude --subagent reviewer",
        session.cwd.clone(),
        T0 + 3_000,
    );
    reviewer.id = NodeId::from_stored("agent_reviewer");
    reviewer.kind = NodeKind::Subagent;
    reviewer.lifecycle = Lifecycle::Alive;
    reviewer.turn = Some(Turn::Active);
    reviewer.link_to(claude_id.clone(), Relation::Confirmed);
    let reviewer_info = reviewer.agent.as_mut().expect("agent detail");
    reviewer_info.name = AgentName::declared("Reviewer");
    reviewer_info.agent = AgentRef {
        provider: Some("anthropic".into()),
        tool: Some("claude-code".into()),
        model: Some("claude-3.5-sonnet".into()),
        external_id: Some("reviewer".into()),
    };
    reviewer_info.current_task = Some("Review the climbing logic changes".into());
    add_preview(
        &mut reviewer,
        "Reviewing climb_system.gd…",
        PreviewSource::AdapterState,
        Confidence::Integrated,
    );
    let reviewer_id = session.tree.insert(reviewer);

    let mut tests = ProcessNode::agent(
        session.id.clone(),
        "claude --subagent tests",
        session.cwd.clone(),
        T0 + 3_500,
    );
    tests.id = NodeId::from_stored("agent_tests");
    tests.kind = NodeKind::Subagent;
    tests.lifecycle = Lifecycle::Alive;
    tests.turn = Some(Turn::Active);
    tests.link_to(claude_id.clone(), Relation::Confirmed);
    tests.agent.as_mut().expect("agent detail").name = AgentName::declared("Tests");
    add_preview(
        &mut tests,
        "Running integration tests — 12/18",
        PreviewSource::RelevantAction,
        Confidence::Integrated,
    );
    let tests_id = session.tree.insert(tests);

    let mut jest = ProcessNode::process(
        session.id.clone(),
        NodeKind::TestRunner,
        "Jest worker",
        session.cwd.clone(),
        T0 + 4_000,
    );
    jest.id = NodeId::from_stored("proc_jest_worker");
    jest.title = "Jest worker".into();
    jest.lifecycle = Lifecycle::Alive;
    jest.link_to(tests_id.clone(), Relation::Confirmed);
    session.tree.insert(jest);

    let mut typecheck = ProcessNode::process(
        session.id.clone(),
        NodeKind::Background,
        "Typecheck",
        session.cwd.clone(),
        T0 + 4_200,
    );
    typecheck.id = NodeId::from_stored("proc_typecheck");
    typecheck.title = "Typecheck".into();
    typecheck.lifecycle = Lifecycle::Exited { code: 0 };
    typecheck.ended_ms = Some(T0 + 11_000);
    typecheck.link_to(tests_id.clone(), Relation::Confirmed);
    session.tree.insert(typecheck);

    let mut shell = ProcessNode::process(
        session.id.clone(),
        NodeKind::Shell,
        "zsh",
        session.cwd.clone(),
        T0 + 1_500,
    );
    shell.id = NodeId::from_stored("proc_shell");
    shell.title = "Shell".into();
    shell.lifecycle = Lifecycle::Alive;
    let shell_id = session.tree.insert(shell);

    let mut fang = ProcessNode::process(
        session.id.clone(),
        NodeKind::Tui,
        "fang",
        session.cwd.clone(),
        T0 + 1_800,
    );
    fang.id = NodeId::from_stored("proc_fang");
    fang.title = "Fang (files)".into();
    fang.lifecycle = Lifecycle::Alive;
    add_preview(
        &mut fang,
        "src/ai/climb_system.gd",
        PreviewSource::StableScreenLine,
        Confidence::Integrated,
    );
    let fang_id = session.tree.insert(fang);

    let bindings = vec![
        PaneNodeBinding {
            pane_id: panes[0].clone(),
            session_id: session.id.clone(),
            node_id: claude_id.clone(),
            temporary: false,
            surface_id: None,
            opened_ms: T0 + 2_000,
        },
        PaneNodeBinding {
            pane_id: panes[1].clone(),
            session_id: session.id.clone(),
            node_id: fang_id,
            temporary: false,
            surface_id: None,
            opened_ms: T0 + 2_000,
        },
        PaneNodeBinding {
            pane_id: panes[2].clone(),
            session_id: session.id.clone(),
            node_id: shell_id,
            temporary: false,
            surface_id: None,
            opened_ms: T0 + 2_000,
        },
    ];
    let nodes =
        TreeNodeView::for_session_with_panes(&session, &bindings, &HashMap::new(), T0 + 15_000);
    let session_summary = SessionSummary::from_session(&session, 1, false, T0 + 15_000);
    let workspace_summary =
        WorkspaceSummary::from_workspace(&workspace, std::slice::from_ref(&session_summary));
    let checkout = WorkspaceCheckout {
        id: checkout_id.clone(),
        workspace_id: workspace.id.clone(),
        path: workspace.root.clone(),
        canonical_path: workspace.root.clone(),
        branch: session.git_branch.clone(),
        primary: true,
        shared_resources: vec!["Docker daemon".into(), "localhost:3000".into()],
        created_ms: T0,
    };
    let lease = WorkspaceWriteLease::active(
        workspace.id.clone(),
        session.id.clone(),
        checkout_id.clone(),
        T0,
    );
    let session_id = session.id.clone();
    let workspace_id = workspace.id.clone();
    let first_workspace = WorkspaceTreeView {
        workspace: workspace_summary,
        checkouts: vec![checkout],
        write_lease: Some(lease.clone()),
        sessions: vec![SessionTreeView {
            session: session_summary,
            nodes,
        }],
    };

    let mut turn_workspace = Workspace::new("turn", "/Users/x/personal-workspace/turn", T0);
    turn_workspace.id = WorkspaceId::from_stored("ws_turn");
    let mut turn_session = Session::new(
        turn_workspace.id.clone(),
        "Build persistent PTY backend",
        turn_workspace.root.clone(),
        Layout::single(Pane::new(PaneKind::Agent).with_command("codex")),
        T0,
    );
    turn_session.id = SessionId::from_stored("sess_build_pty");
    turn_session.mode = SessionMode::MainCheckout;
    let mut codex = ProcessNode::agent(
        turn_session.id.clone(),
        "codex",
        turn_session.cwd.clone(),
        T0 + 2_000,
    );
    codex.id = NodeId::from_stored("agent_codex");
    codex.lifecycle = Lifecycle::Alive;
    codex.turn = Some(Turn::Active);
    codex.agent.as_mut().expect("agent detail").name = AgentName::declared("Codex");
    add_preview(
        &mut codex,
        "Implementing reconnect protocol…",
        PreviewSource::SemanticEvent,
        Confidence::Integrated,
    );
    turn_session.tree.insert(codex);
    let turn_summary = SessionSummary::from_session(&turn_session, 0, false, T0 + 15_000);
    let turn_workspace_summary =
        WorkspaceSummary::from_workspace(&turn_workspace, std::slice::from_ref(&turn_summary));
    let second_workspace = WorkspaceTreeView {
        workspace: turn_workspace_summary,
        checkouts: Vec::new(),
        write_lease: None,
        sessions: vec![SessionTreeView {
            session: turn_summary,
            nodes: TreeNodeView::for_session(&turn_session, T0 + 15_000),
        }],
    };

    let mut infra = Workspace::new("personal-infra", "/Users/x/personal-infra", T0);
    infra.id = WorkspaceId::from_stored("ws_personal_infra");
    let third_workspace = WorkspaceTreeView {
        workspace: WorkspaceSummary::from_workspace(&infra, &[]),
        checkouts: Vec::new(),
        write_lease: None,
        sessions: Vec::new(),
    };

    UnifiedHierarchy {
        snapshot: HierarchySnapshot {
            revision: 23,
            tree_state: TreeSurfaceState {
                surface_id: "window-snapshot".into(),
                selected: Some(HierarchyKey::process(claude_id.clone())),
                expanded: vec![
                    HierarchyKey::workspace(workspace_id.clone()),
                    HierarchyKey::session(session_id.clone()),
                    HierarchyKey::process(claude_id),
                    HierarchyKey::process(tests_id.clone()),
                    HierarchyKey::workspace(turn_workspace.id.clone()),
                    HierarchyKey::session(turn_session.id.clone()),
                ],
            },
            workspaces: vec![first_workspace, second_workspace, third_workspace],
        },
        session_id,
        reviewer_id,
        workspace_id,
        checkout_id,
        lease,
    }
}

/// A window in the state the product exists for: one session blocked on a permission,
/// others working, one failed, with three panes on screen.
fn busy_desk() -> Fixture {
    let (layout, panes) = three_pane_layout();
    let hierarchy = unified_hierarchy(&layout, &panes);
    let reviewer_preview = hierarchy
        .snapshot
        .workspaces
        .iter()
        .flat_map(|workspace| &workspace.sessions)
        .flat_map(|session| &session.nodes)
        .find(|node| node.node_id == hierarchy.reviewer_id)
        .and_then(|node| node.activity_preview.clone())
        .expect("Reviewer preview");
    let mut reviewer_history = vec![reviewer_preview.clone()];
    reviewer_history.push(ActivityPreview {
        raw_source_sequence: Some(41),
        normalized_text: "Checking state transitions and edge cases".into(),
        updated_ms: T0 + 10_000,
        ..reviewer_preview.clone()
    });
    reviewer_history.push(ActivityPreview {
        raw_source_sequence: Some(40),
        normalized_text: "Opened src/ai/climb_system.gd".into(),
        source: PreviewSource::RelevantAction,
        updated_ms: T0 + 8_000,
        ..reviewer_preview
    });
    let mut preview_history = HashMap::new();
    preview_history.insert(hierarchy.reviewer_id.clone(), reviewer_history);
    let mut grids = BTreeMap::new();
    let mut titles = BTreeMap::new();
    grids.insert(panes[0].clone(), agent_screen());
    titles.insert(panes[0].clone(), "claude · agent · sonnet".to_string());
    grids.insert(
        panes[1].clone(),
        screen(
            &[
                "src/ai/climb_system.gd",
                "  198  func _on_jump():",
                "  199    var new_state = _calculate_state()",
                "+ 200    if new_state == STATE_WALL or STATE_CEILING:",
                "  201      _transition_to(new_state)",
            ],
            20,
            46,
        ),
    );
    titles.insert(panes[1].clone(), "fang · files".to_string());
    grids.insert(
        panes[2].clone(),
        screen(&["~/space-troopers on climb $ "], 20, 46),
    );
    titles.insert(panes[2].clone(), "zsh".to_string());

    Fixture {
        hierarchy: Some(hierarchy.snapshot),
        sessions: Vec::new(),
        selected: Some(hierarchy.session_id),
        layout: Some(layout),
        grids,
        titles,
        focused: Some(panes[0].clone()),
        permission: Some(permission(Risk::Medium, false)),
        queue: vec![
            queue_item("Fix climbing bugs", AwaitingReason::Permission, true, false),
            queue_item("Dockerize tests", AwaitingReason::Input, true, false),
        ],
        connection: Some(connected()),
        preview_history,
        ..Fixture::default()
    }
}

fn restored_desk() -> Fixture {
    let mut fixture = busy_desk();
    fixture.permission = None;
    fixture.queue.clear();
    let session_id = fixture.selected.clone().expect("selected Session");
    let snapshot = fixture.hierarchy.as_ref().expect("Hierarchy");
    let session = snapshot
        .workspaces
        .iter()
        .flat_map(|workspace| &workspace.sessions)
        .find(|session| session.session.id == session_id)
        .expect("selected Session in Hierarchy");
    let panes = session
        .nodes
        .iter()
        .flat_map(|node| {
            node.pane_bindings
                .iter()
                .map(move |binding| PaneRestoreOutcome {
                    pane_id: binding.pane_id.clone(),
                    node_id: node.node_id.clone(),
                    lifecycle: Lifecycle::Lost,
                    can_relaunch: true,
                    command: Some(node.command.clone()),
                    // Every pane in this fixture is an agent or a named command, so all of
                    // them would use the Session's checkout write authority.
                    auto_start: false,
                    needs_checkout_write: true,
                })
        })
        .collect();
    let mut recovery_lease = snapshot
        .workspaces
        .iter()
        .find_map(|workspace| workspace.write_lease.clone())
        .expect("main-checkout lease");
    recovery_lease.state = LeaseState::RecoveryRequired;
    let stopped = DisplayState::Stopped;
    if let Some(snapshot) = fixture.hierarchy.as_mut() {
        for workspace in &mut snapshot.workspaces {
            workspace.workspace.sessions_needing_user = 0;
            workspace.workspace.badge_count = 0;
            if let Some(lease) = workspace.write_lease.as_mut() {
                lease.state = LeaseState::RecoveryRequired;
            }
            for session in &mut workspace.sessions {
                session.session.display_state = stopped;
                session.session.state_label = stopped.label().into();
                session.session.severity = stopped.severity();
                session.session.needs_user = false;
                session.session.badge_count = 0;
                session.session.subagent_count = 0;
                session.session.running_count = 0;
                for node in &mut session.nodes {
                    node.lifecycle = Lifecycle::Lost;
                    node.display_state = stopped;
                    node.state_label = stopped.label().into();
                    node.severity = stopped.severity();
                    node.needs_user = false;
                    node.interaction_pending = false;
                }
            }
        }
    }
    fixture.restore = Some(SessionRestoreView {
        session_id,
        state: RestoreState::LayoutOnly,
        panes,
    });
    fixture.recovery_lease = Some(recovery_lease);
    fixture.grids.clear();
    fixture
}

#[test]
fn a_busy_desk_with_a_pending_permission() {
    let mut h = harness(busy_desk());
    h.run();
    h.snapshot("busy_desk");
}

#[test]
fn a_restored_layout_explains_that_nothing_was_restarted_and_offers_recovery() {
    let mut h = harness(restored_desk());
    h.run();
    h.snapshot("restored_layout");

    h.state_mut().actions.clear();
    h.query_by_label("Confirm write access")
        .expect("recovery has an explicit write-access confirmation")
        .click();
    h.run_steps(1);
    assert!(matches!(
        h.state().actions.as_slice(),
        [ViewAction::ReclaimWorkspaceWriteLease { .. }]
    ));
}

/// Coming back to a Session is one decision, not one per pane.
///
/// Reported as unusable, and it was: every stopped pane showed a "Start pane" button in the
/// middle of itself, so a four-pane Session read as four separate decisions. There *was* a
/// "Start all" — small, in the bottom status bar, at the other end of the window from the panes
/// it acts on. The collective action now sits beside the per-pane one, where the hand already is,
/// and one click asks for every pane.
#[test]
fn coming_back_to_a_session_starts_every_pane_in_one_click() {
    // No write confirmation pending, so every stopped pane is one this click can start. The
    // case where one is *not* is asserted at the end.
    let mut fixture = restored_desk();
    fixture.recovery_lease = None;
    let mut h = harness(fixture);
    h.run();
    h.run();

    // The offer names how many it covers, so the click is not a guess.
    let labels = button_labels(&h);
    let collective = labels
        .iter()
        .find(|label| label.starts_with("Start all "))
        .unwrap_or_else(|| {
            panic!("the pane's own offer must include the collective one: {labels:?}")
        });
    let collective = collective.clone();

    h.state_mut().actions.clear();
    h.query_by_label(&collective)
        .expect("the collective offer is a real button")
        .click();
    h.run_steps(1);

    let asked: Vec<&ViewAction> = h
        .state()
        .actions
        .iter()
        .filter(|action| matches!(action, ViewAction::RelaunchNode { .. }))
        .collect();
    assert!(
        asked.len() > 1,
        "one click must ask for every stopped pane, got {asked:?}"
    );
    // Every one of them is a different pane: asking twice for the same node would start one
    // process and look like it started two.
    let mut nodes: Vec<String> = asked
        .iter()
        .filter_map(|action| match action {
            ViewAction::RelaunchNode { node_id, .. } => Some(node_id.to_string()),
            _ => None,
        })
        .collect();
    let before = nodes.len();
    nodes.sort();
    nodes.dedup();
    assert_eq!(
        nodes.len(),
        before,
        "the same pane must not be asked for twice"
    );

    // With a write confirmation pending, the offer covers only what it can really start — a
    // pane that would use the checkout is held back by the confirmation, so a button claiming
    // to start it would be lying about a number the user can count on screen.
    let mut h = harness(restored_desk());
    h.run();
    h.run();
    let pending = button_labels(&h);
    let claimed: Vec<&String> = pending
        .iter()
        .filter(|label| label.starts_with("Start all "))
        .collect();
    assert!(
        claimed.len() <= 1,
        "the collective offer appears at most once: {claimed:?}"
    );
    if let Some(label) = claimed.first() {
        assert!(
            !label.contains(&format!(
                "{} panes",
                pending.iter().filter(|l| *l == "Start pane").count() + 1
            )),
            "the number must not count panes the confirmation is holding back: {label:?}"
        );
    }
}

/// Pointing at one worker among many, and getting the layout back.
///
/// An agent managing four subagents runs all of them inside one pane, so the tree could list them
/// and not show them: finding the one you cared about meant reading output from four. Clicking a
/// subagent now maximises the pane it runs in, and clicking what owns it — its agent, its Session
/// — puts the layout back.
#[test]
fn clicking_a_subagent_shows_its_pane_and_clicking_its_owner_restores_the_layout() {
    let fixture = busy_desk();
    let mut h = harness(fixture);
    h.run();
    h.run();

    // A subagent, which has no pane of its own.
    let rows = tree_row_labels(&h);
    let subagent = rows
        .iter()
        .find(|label| label.contains("Reviewer"))
        .unwrap_or_else(|| panic!("the fixture has a subagent row: {rows:?}"))
        .clone();

    h.state_mut().actions.clear();
    h.get_by_label_contains(&subagent).click();
    h.run_steps(1);
    let zoomed: Vec<&ViewAction> = h
        .state()
        .actions
        .iter()
        .filter(|action| matches!(action, ViewAction::ZoomPane { .. }))
        .collect();
    assert_eq!(
        zoomed.len(),
        1,
        "clicking a subagent asks for exactly one pane to be shown: {:?}",
        h.state().actions
    );

    // The layout the daemon answers with, so the window's next decision is made against the
    // state the daemon reports rather than against a guess.
    let pane = match zoomed[0] {
        ViewAction::ZoomPane { pane_id, .. } => pane_id.clone(),
        _ => unreachable!(),
    };
    let mut layout = h.state().fixture.layout.clone().expect("a layout");
    assert!(layout.toggle_zoom(&pane), "the pane the click named exists");
    assert_eq!(layout.zoomed.as_ref(), Some(&pane));
    h.state_mut().fixture.layout = Some(layout);
    h.run();
    h.run();

    // Clicking the same subagent again does *not* toggle it back off. `zoom_pane` toggles, so a
    // second click on a row already being shown would un-maximise the pane and the tree would
    // flicker instead of holding still.
    h.state_mut().actions.clear();
    h.get_by_label_contains(&subagent).click();
    h.run_steps(1);
    assert!(
        !h.state()
            .actions
            .iter()
            .any(|action| matches!(action, ViewAction::ZoomPane { .. })),
        "a pane already being shown must not be un-maximised by pointing at it again: {:?}",
        h.state().actions
    );

    // And clicking the agent that owns it puts the layout back.
    let owner = rows
        .iter()
        .find(|label| label.contains("Claude Code"))
        .expect("the fixture has the owning agent")
        .clone();
    h.state_mut().actions.clear();
    h.get_by_label_contains(&owner).click();
    h.run_steps(1);
    let restored: Vec<&ViewAction> = h
        .state()
        .actions
        .iter()
        .filter(|action| matches!(action, ViewAction::ZoomPane { .. }))
        .collect();
    assert_eq!(
        restored.len(),
        1,
        "clicking the owner asks for the maximised pane to be released: {:?}",
        h.state().actions
    );
    assert!(
        matches!(restored[0], ViewAction::ZoomPane { pane_id, .. } if *pane_id == pane),
        "and it names the pane that is maximised, which is what un-toggles it"
    );
}

/// The other half of the recovery rule, in the window: a pending write confirmation holds
/// back what would use the checkout, not the whole Session.
///
/// A pane that would only open the user's own shell keeps a clickable offer — which is
/// what the owner needs in order to go and stop the process being asked about — while a
/// pane that would run an agent says plainly what it is waiting for.
#[test]
fn a_restored_pane_that_writes_nothing_is_still_startable_while_write_access_is_pending() {
    let gated = harness(restored_desk());
    let mut gated = gated;
    gated.run();
    gated.run();
    assert!(
        gated
            .query_all_by_label("Confirm write access in the status bar first.")
            .count()
            > 0,
        "an agent pane must say what it is waiting for: {:?}",
        button_labels(&gated)
    );

    let mut fixture = restored_desk();
    let shell_pane = fixture
        .restore
        .as_mut()
        .map(|restore| {
            for outcome in &mut restore.panes {
                // The same panes, but as terminals: nothing Turn starts in them writes to
                // the shared checkout.
                outcome.needs_checkout_write = false;
                outcome.command = None;
            }
            restore.panes[0].node_id.clone()
        })
        .expect("the restored fixture offers panes");
    let mut h = harness(fixture);
    h.run();
    h.run();
    assert_eq!(
        h.query_all_by_label("Confirm write access in the status bar first.")
            .count(),
        0,
        "a terminal is not gated, so nothing may tell the user it is"
    );

    h.state_mut().actions.clear();
    h.query_all_by_label("Start pane")
        .next()
        .expect("the offer for a pane that writes nothing remains a real button")
        .click();
    h.run_steps(1);
    assert!(
        matches!(
            h.state().actions.as_slice(),
            [ViewAction::RelaunchNode { node_id, .. }] if node_id == &shell_pane
        ),
        "{:?}",
        h.state().actions
    );
}

/// Every accessible name in the window, for the tests that care that a control can be
/// found by a person who cannot see it.
fn button_labels(h: &Harness<'static, Window>) -> Vec<String> {
    h.query_all_by_role(egui::accesskit::Role::Button)
        .filter_map(|node| node.accesskit_node().label())
        .collect()
}

/// The rows the command palette is offering, as a screen reader would read them.
fn palette_rows(h: &Harness<'static, Window>) -> Vec<String> {
    h.query_all_by_role(egui::accesskit::Role::ListItem)
        .filter_map(|node| node.accesskit_node().label())
        .collect()
}

/// What a screen reader would read out of the tree, one string per visible row.
fn tree_row_labels(h: &Harness<'static, Window>) -> Vec<String> {
    h.query_all_by_role(egui::accesskit::Role::TreeItem)
        .filter_map(|node| node.accesskit_node().label())
        .collect()
}

/// Every piece of text the window is showing, as a screen reader would find it.
///
/// Used where the *wording* is the feature: a dialog that promises not to delete somebody's
/// checkout has to name it, and a test that only looked at the image could not tell whether
/// the sentence said so or merely looked as if it did.
fn all_text(h: &Harness<'static, Window>) -> Vec<String> {
    // Both, because a painted label reaches the tree as a `value` and a widget's name reaches
    // it as a `label`, and the dialog's sentences are the first kind.
    h.root()
        .children_recursive()
        .filter_map(|node| {
            let node = node.accesskit_node();
            node.value()
                .map(|value| value.to_string())
                .or_else(|| node.label())
        })
        .collect()
}

fn group_labels(h: &Harness<'static, Window>) -> Vec<String> {
    h.query_all_by_role(egui::accesskit::Role::Group)
        .filter_map(|node| node.accesskit_node().label())
        .collect()
}

/// The toolbar that took the place of the `RESTORED SAFELY` strip.
///
/// Worth a screenshot of its own because the thing that goes wrong with a row of controls
/// beside a right-aligned label is overlap, and because every button in it has to be
/// findable by name: an icon nobody can name is a control a screen-reader user does not
/// have.
#[test]
fn the_top_bar_carries_a_toolbar_of_named_actions_and_the_version() {
    let mut fixture = busy_desk();
    fixture.permission = None;
    fixture.queue.clear();
    let mut h = harness(fixture);
    h.run();
    h.run();

    let buttons = button_labels(&h);
    for label in [
        "New pane",
        "Layout",
        "New session",
        "New workspace",
        "Command palette",
        "Attention queue",
        "Keyboard shortcuts",
        "Settings",
    ] {
        assert!(
            buttons.iter().any(|found| found == label),
            "the toolbar must offer {label:?} by name; found {buttons:?}"
        );
    }
    // `Archived` was a third button beside two that create things. It is gone from the
    // Workspaces bar entirely.
    assert!(
        !buttons.iter().any(|found| found == "Archived"),
        "the archived filter must not be in the workspaces bar: {buttons:?}"
    );

    let groups = group_labels(&h);
    assert!(
        groups
            .iter()
            .any(|group| group.starts_with("Turn ") && group.contains("connected")),
        "the version and the connection are announced, not only painted: {groups:?}"
    );
    h.snapshot("chrome_toolbar");
}

/// The toolbar has to give way rather than draw over what is beside it. At 520 points
/// there is no room for eight buttons, and the version and the connection must both
/// survive.
#[test]
fn a_narrow_window_drops_toolbar_buttons_rather_than_overlapping_the_version() {
    let mut fixture = busy_desk();
    fixture.permission = None;
    fixture.queue.clear();
    let mut h = Harness::builder()
        .with_size(egui::vec2(520.0, 600.0))
        .build_ui_state(
            |ui, window: &mut Window| {
                let Window {
                    fixture,
                    state,
                    theme,
                    keymap,
                    actions,
                } = window;
                theme.install(ui.ctx());
                actions.extend(fixture.view().ui(ui, theme, keymap, state));
            },
            window(fixture),
        );
    h.run();
    h.run();

    let buttons = button_labels(&h);
    let toolbar_present = [
        "New pane",
        "Layout",
        "New session",
        "New workspace",
        "Command palette",
        "Attention queue",
        "Keyboard shortcuts",
        "Settings",
    ]
    .into_iter()
    .filter(|label| buttons.iter().any(|found| found == label))
    .count();
    assert!(
        toolbar_present < 8,
        "a 520-point window cannot hold the whole toolbar; it kept {toolbar_present}"
    );
    assert!(
        group_labels(&h)
            .iter()
            .any(|group| group.starts_with("Turn ")),
        "the connection and version must never be the thing that is dropped"
    );
    // The tree obeys the same rule. A 520-point window leaves its rows about 218 points
    // wide, which is not enough for a name, a status tag and a pair of controls — so the
    // controls give way, and the rows still say which Session they are.
    assert!(
        !buttons
            .iter()
            .any(|label| label.starts_with("Close session")
                || label.starts_with("Archive session")),
        "a tree this narrow must keep its names rather than its controls; found {buttons:?}"
    );
    assert!(
        tree_row_labels(&h)
            .iter()
            .any(|label| label.contains("Fix climbing bugs")),
        "and the name has to survive whole: {:?}",
        tree_row_labels(&h)
    );
}

/// `Command::ClosePane` existed from the first keymap with nothing on screen to invoke it.
/// This is the affordance, on the pane it closes, and it says in its own tooltip that the
/// process survives — which is the rule the daemon already enforces.
#[test]
fn closing_a_pane_is_a_control_on_that_panes_own_header() {
    let mut fixture = busy_desk();
    fixture.permission = None;
    fixture.queue.clear();
    // One pane zoomed, which is the case where a header has to fit a long title, the
    // `zoomed` tag and the close control on one line without any of them running into
    // another.
    if let Some(layout) = fixture.layout.as_mut() {
        layout.zoomed = layout.panes().first().map(|pane| pane.id.clone());
    }
    let panes: Vec<PaneId> = fixture
        .layout
        .as_ref()
        .expect("layout")
        .panes()
        .into_iter()
        .map(|pane| pane.id.clone())
        .collect();
    let mut h = harness(fixture);
    h.run();
    h.run();
    h.snapshot("pane_close_control");
    assert!(
        button_labels(&h)
            .iter()
            .any(|label| label == "Close pane claude · agent · sonnet"),
        "a zoomed pane keeps its close control beside the zoom tag"
    );

    // Un-zoomed, every pane has one, and each names the pane it belongs to.
    if let Some(layout) = h.state_mut().fixture.layout.as_mut() {
        layout.zoomed = None;
    }
    h.run();
    h.run();
    let buttons = button_labels(&h);
    for title in ["claude · agent · sonnet", "fang · files", "zsh"] {
        assert!(
            buttons
                .iter()
                .any(|label| label == &format!("Close pane {title}")),
            "every pane header names its own close control; found {buttons:?}"
        );
    }

    h.state_mut().actions.clear();
    h.query_by_label("Close pane zsh")
        .expect("the close control is a real button")
        .click();
    h.run_steps(1);
    assert_eq!(
        h.state().actions,
        vec![ViewAction::ClosePane {
            pane_id: panes[2].clone()
        }],
        "the control closes its own pane and nothing else"
    );
}

/// A three-pane desk with nothing blocked on the user, so the panes have the window to
/// themselves and a drag can be aimed at real geometry.
///
/// The three screens are deliberately different sizes. The rectangles a gesture is aimed
/// at are read back out of the accessibility tree, and two panes that describe themselves
/// identically would make "the pane under the pointer" ambiguous in the test rather than in
/// the window.
fn relocation_desk() -> Fixture {
    let mut fixture = busy_desk();
    fixture.permission = None;
    fixture.queue.clear();
    let panes = relocation_panes(&fixture);
    fixture.grids.insert(panes[0].clone(), agent_screen());
    fixture.grids.insert(
        panes[1].clone(),
        screen(
            &[
                "src/ai/climb_system.gd",
                "  198  func _on_jump():",
                "  199    var new_state = _calculate_state()",
                "+ 200    if new_state == STATE_WALL or STATE_CEILING:",
                "  201      _transition_to(new_state)",
            ],
            24,
            46,
        ),
    );
    fixture.grids.insert(
        panes[2].clone(),
        screen(&["~/space-troopers on climb $ "], 12, 44),
    );
    fixture
}

fn relocation_panes(fixture: &Fixture) -> Vec<PaneId> {
    fixture
        .layout
        .as_ref()
        .expect("layout")
        .panes()
        .into_iter()
        .map(|pane| pane.id.clone())
        .collect()
}

/// The rectangle a pane was actually drawn at, reassembled from the accessibility tree.
///
/// A drop zone is a fraction of a pane's own width and height, so a test that guessed the
/// rectangle would be testing its guess. The header grip and the terminal body are both in
/// the tree, and together they are the whole pane.
fn drawn_pane(h: &mut Harness<'static, Window>, title: &str, terminal: &str) -> egui::Rect {
    let header = h
        .get_by_label_contains(&format!("{title} pane header"))
        .rect();
    let body = h.get_by_label_contains(terminal).rect();
    header.union(body)
}

/// The gesture people already know from every tiling editor: drag a pane by its header onto
/// another pane, and which of that pane's five regions the pointer is in decides the result.
/// It has to name both panes and the zone, and go through the daemon — the window must not
/// rearrange its own copy of the layout on the way.
#[test]
fn dropping_a_pane_on_an_edge_of_another_asks_to_land_on_that_side_of_it() {
    let fixture = relocation_desk();
    let panes = relocation_panes(&fixture);
    let untouched = fixture.layout.clone();
    let mut h = harness(fixture);
    h.run();
    h.run();

    let source = h.get_by_label_contains("zsh pane header").rect();
    let target = drawn_pane(&mut h, "claude · agent · sonnet", "Terminal, 40 rows");
    // Well inside the right-hand band, and vertically in the middle so it is that band and
    // not a corner.
    let on_the_right_edge = egui::pos2(
        target.max.x - turn_gui::panes::drop_edge_band(target.width()) / 2.0,
        target.center().y,
    );

    h.hover_at(source.center());
    h.run_steps(1);
    h.drag_at(source.center());
    h.run_steps(1);
    h.hover_at(on_the_right_edge);
    h.run_steps(1);
    h.state_mut().actions.clear();
    h.drop_at(on_the_right_edge);
    h.run_steps(1);

    assert_eq!(
        h.state().actions,
        vec![ViewAction::RelocatePane {
            moved: panes[2].clone(),
            target: panes[0].clone(),
            zone: DropZone::Right,
        }]
    );
    assert_eq!(
        h.state().fixture.layout,
        untouched,
        "the window asks the daemon to move the pane; it does not move its own copy"
    );
}

/// The middle of a pane is the one zone that changes no shape: the two panes exchange
/// places. It has to be easy to mean, which is what the band sizes are for.
#[test]
fn dropping_a_pane_on_the_middle_of_another_asks_to_exchange_the_two() {
    let fixture = relocation_desk();
    let panes = relocation_panes(&fixture);
    let mut h = harness(fixture);
    h.run();
    h.run();

    let source = h.get_by_label_contains("zsh pane header").rect();
    let target = drawn_pane(&mut h, "claude · agent · sonnet", "Terminal, 40 rows");

    h.hover_at(source.center());
    h.run_steps(1);
    h.drag_at(source.center());
    h.run_steps(1);
    h.hover_at(target.center());
    h.run_steps(1);
    h.state_mut().actions.clear();
    h.drop_at(target.center());
    h.run_steps(1);

    assert_eq!(
        h.state().actions,
        vec![ViewAction::RelocatePane {
            moved: panes[2].clone(),
            target: panes[0].clone(),
            zone: DropZone::Centre,
        }]
    );
}

/// What the user sees while dragging, aimed at an edge: the region the pane will occupy,
/// which is half of the target, with the side named in words. A rearrangement nobody can
/// predict before letting go is one they will not use.
#[test]
fn a_drag_aimed_at_a_panes_right_edge_shows_the_half_it_would_take() {
    let mut h = harness(relocation_desk());
    h.run();
    h.run();

    let source = h.get_by_label_contains("zsh pane header").rect();
    let target = drawn_pane(&mut h, "claude · agent · sonnet", "Terminal, 40 rows");
    let on_the_right_edge = egui::pos2(
        target.max.x - turn_gui::panes::drop_edge_band(target.width()) / 2.0,
        target.center().y,
    );

    h.hover_at(source.center());
    h.run_steps(1);
    h.drag_at(source.center());
    h.run_steps(1);
    h.hover_at(on_the_right_edge);
    h.run_steps(2);
    h.snapshot("pane_drop_zone_right_edge");
}

/// The same drag, moved to the middle of the same pane. Told apart from the edge without
/// dropping: a different shape, a different word.
#[test]
fn the_same_drag_over_the_middle_of_the_pane_shows_the_whole_of_it_instead() {
    let mut h = harness(relocation_desk());
    h.run();
    h.run();

    let source = h.get_by_label_contains("zsh pane header").rect();
    let target = drawn_pane(&mut h, "claude · agent · sonnet", "Terminal, 40 rows");

    h.hover_at(source.center());
    h.run_steps(1);
    h.drag_at(source.center());
    h.run_steps(1);
    h.hover_at(target.center());
    h.run_steps(2);
    h.snapshot("pane_drop_zone_centre");
}

/// Escape during a drag leaves the layout exactly as it was, even when the pointer is
/// released over a perfectly good target. A gesture people are afraid to start is one they
/// will not use.
#[test]
fn escape_during_a_drag_cancels_it_and_a_later_drop_moves_nothing() {
    let fixture = relocation_desk();
    let untouched = fixture.layout.clone();
    let mut h = harness(fixture);
    h.run();
    h.run();

    let source = h.get_by_label_contains("zsh pane header").rect();
    let target = drawn_pane(&mut h, "claude · agent · sonnet", "Terminal, 40 rows");

    h.hover_at(source.center());
    h.run_steps(1);
    h.drag_at(source.center());
    h.run_steps(1);
    h.hover_at(target.center());
    h.run_steps(1);
    h.key_press(egui::Key::Escape);
    h.run_steps(1);
    h.state_mut().actions.clear();
    h.drop_at(target.center());
    h.run_steps(1);

    assert!(
        h.state().actions.is_empty(),
        "a cancelled drag must ask for nothing; got {:?}",
        h.state().actions
    );
    assert_eq!(h.state().fixture.layout, untouched);
}

/// The Escape that cancels a drag is spent on the drag. A temporary pane is open — it is
/// not a sheet, so panes stay draggable behind it — and closing it is what Escape means
/// when nothing is being dragged. Cancelling a rearrangement must not also throw away what
/// the user was reading, and the *next* Escape must still work normally.
#[test]
fn escape_cancelling_a_drag_is_not_also_spent_closing_what_is_open_behind_it() {
    let mut fixture = relocation_desk();
    let snapshot = fixture.hierarchy.as_mut().expect("hierarchy");
    let reviewer = snapshot
        .workspaces
        .iter_mut()
        .flat_map(|workspace| &mut workspace.sessions)
        .flat_map(|session| &mut session.nodes)
        .find(|node| {
            node.agent
                .as_ref()
                .is_some_and(|agent| agent.name.display_name == "Reviewer")
        })
        .expect("Reviewer");
    let binding = PaneNodeBinding {
        pane_id: PaneId::from_stored("pane_reviewer_temporary"),
        session_id: reviewer.session_id.clone(),
        node_id: reviewer.node_id.clone(),
        temporary: true,
        surface_id: Some(snapshot.tree_state.surface_id.clone()),
        opened_ms: T0 + 15_000,
    };
    reviewer.pane_bindings.push(binding.clone());
    fixture.temporary_pane = Some(NodePaneView {
        binding,
        capability: NodePaneCapability::PreviewDetails,
    });
    let mut h = harness(fixture);
    h.run();
    h.run();

    // The left-hand end of the leftmost pane's header, which the temporary pane's panel
    // does not cover.
    let source = h
        .get_by_label_contains("claude · agent · sonnet pane header")
        .rect();
    let grip = egui::pos2(source.min.x + 12.0, source.center().y);
    h.hover_at(grip);
    h.run_steps(1);
    h.drag_at(grip);
    h.run_steps(1);
    h.hover_at(grip + egui::vec2(30.0, 90.0));
    h.run_steps(1);

    h.state_mut().actions.clear();
    h.key_press(egui::Key::Escape);
    h.run_steps(1);
    assert!(
        !h.state()
            .actions
            .iter()
            .any(|action| matches!(action, ViewAction::CloseTemporaryPane { .. })),
        "the drag's Escape closed the temporary pane as well; got {:?}",
        h.state().actions
    );

    // With no drag in progress, Escape goes back to meaning what it always meant.
    h.drop_at(grip + egui::vec2(30.0, 90.0));
    h.run_steps(1);
    h.state_mut().actions.clear();
    h.key_press(egui::Key::Escape);
    h.run_steps(1);
    assert!(
        h.state()
            .actions
            .iter()
            .any(|action| matches!(action, ViewAction::CloseTemporaryPane { .. })),
        "Escape must still close the temporary pane afterwards; got {:?}",
        h.state().actions
    );
}

/// A pane let go of over the sidebar, the toolbar or the status bar has not been dropped
/// anywhere. Nothing happens, and nothing is sent.
#[test]
fn dropping_a_pane_outside_every_pane_moves_nothing() {
    let fixture = relocation_desk();
    let untouched = fixture.layout.clone();
    let mut h = harness(fixture);
    h.run();
    h.run();

    let source = h.get_by_label_contains("zsh pane header").rect();
    // The workspace tree, which is a long way from any pane.
    let outside = egui::pos2(40.0, 400.0);

    h.hover_at(source.center());
    h.run_steps(1);
    h.drag_at(source.center());
    h.run_steps(1);
    h.hover_at(outside);
    h.run_steps(1);
    h.state_mut().actions.clear();
    h.drop_at(outside);
    h.run_steps(1);

    assert!(
        h.state().actions.is_empty(),
        "a drop with no target must ask for nothing; got {:?}",
        h.state().actions
    );
    assert_eq!(h.state().fixture.layout, untouched);
}

/// A drop onto a pane too narrow for the sentence. The failure to watch for here is text
/// painted past the edge of the region it describes, landing on the pane next door: the
/// label gives way to a single word, and then to nothing, while the highlighted shape — the
/// part that actually says where the pane lands — is unaffected.
#[test]
fn a_drop_onto_a_narrow_pane_shortens_its_label_rather_than_spilling_over_the_pane_beside_it() {
    let mut fixture = relocation_desk();
    let mut layout = fixture.layout.clone().expect("layout");
    // Five columns, so every pane is narrower than the sentence about the widest title.
    assert!(layout.apply_preset(turn_core::model::LayoutPreset::Columns));
    let mut last = layout.panes().last().expect("a pane").id.clone();
    for (index, name) in ["make", "logs"].iter().enumerate() {
        let pane = Pane::new(PaneKind::Shell).with_title(*name);
        let id = pane.id.clone();
        assert!(layout.split(&last, Direction::Horizontal, pane));
        fixture.titles.insert(id.clone(), (*name).to_string());
        // A different size per pane, so each one describes itself uniquely in the
        // accessibility tree the test reads its rectangle back out of.
        fixture.grids.insert(
            id.clone(),
            screen(&[&format!("$ {name}")], 16 + index as u16, 12),
        );
        last = id;
    }
    let panes: Vec<PaneId> = layout
        .panes()
        .into_iter()
        .map(|pane| pane.id.clone())
        .collect();
    fixture.layout = Some(layout);
    let untouched = fixture.layout.clone();

    let mut h = harness(fixture);
    h.run();
    h.run();

    let source = h.get_by_label_contains("zsh pane header").rect();
    // The pane with the longest title, so the sentence cannot possibly fit in it.
    let target = drawn_pane(&mut h, "claude · agent · sonnet", "Terminal, 40 rows");
    h.hover_at(source.center());
    h.run_steps(1);
    h.drag_at(source.center());
    h.run_steps(1);
    h.hover_at(target.center());
    h.run_steps(2);
    h.snapshot("pane_drop_zone_narrow");

    // The gesture still works, and still names the pane and the zone exactly.
    h.state_mut().actions.clear();
    h.drop_at(target.center());
    h.run_steps(1);
    assert_eq!(
        h.state().actions,
        vec![ViewAction::RelocatePane {
            moved: panes[2].clone(),
            target: panes[0].clone(),
            zone: DropZone::Centre,
        }]
    );
    assert_eq!(h.state().fixture.layout, untouched);
}

/// The layout the owner said was impossible to reach, in two pictures: three panes as a
/// tall column beside a stack, and the same three after one pane was relocated across the
/// window. The shape changed — which is the whole complaint — and the window drew the
/// layout the daemon sent back rather than one of its own.
#[test]
fn a_three_pane_layout_before_and_after_a_relocation_that_changes_its_orientation() {
    let before = relocation_desk();
    let panes = relocation_panes(&before);
    let mut h = harness(before);
    h.run();
    h.snapshot("relocation_before");

    // What the daemon answers with: the shell that was stacked on the right becomes the
    // bottom of the left-hand column, so the right-hand side is now one pane and the left
    // is two. No pane, and no process behind one, changed identity on the way.
    let mut relocated = h.state().fixture.layout.clone().expect("a layout");
    assert!(
        relocated.relocate(&panes[2], &panes[0], DropZone::Below),
        "the relocation the drag asks for has to be one the domain can do"
    );
    assert!(relocated.sizes_are_normalised());
    h.state_mut().fixture.layout = Some(relocated);
    h.run();
    h.run();
    h.snapshot("relocation_after");
}

/// Every row control of the same kind sits in the same column, whatever else its row carries.
///
/// The reported defect: with a Workspace expanded, the Session's controls and the Workspace's
/// did not line up, and neither did two Workspaces with each other. A column that moves from
/// row to row means the user has to look for the button that ends work instead of knowing
/// where it is — and a destructive control is the last one that should need looking for.
///
/// Measured rather than eyeballed: the buttons are found by name and their rectangles
/// compared, so the property is checked at whatever size and depth the tree happens to draw.
#[test]
fn a_row_control_of_the_same_kind_is_always_in_the_same_column() {
    let mut fixture = busy_desk();
    fixture.permission = None;
    fixture.queue.clear();
    // Expanded, which is the state the defect was reported in: a Workspace with its Sessions
    // under it, so rows of both kinds — and of different depths — are on screen together.
    let mut h = harness(fixture);
    h.run();
    h.run();

    let column_of = |h: &Harness<'static, Window>, label: &str| -> f32 {
        h.query_by_label(label)
            .unwrap_or_else(|| panic!("no control named {label:?}"))
            .rect()
            .max
            .x
    };

    // The rightmost column is the lifecycle one, on every row that has it.
    let mut closers = vec![
        column_of(&h, "Stop all sessions in space-troopers"),
        column_of(&h, "Stop all sessions in turn"),
    ];
    closers.push(column_of(&h, "Close session Fix climbing bugs"));
    let first = closers[0];
    for edge in &closers {
        assert!(
            (edge - first).abs() < 0.5,
            "the closing control must be in one column: {closers:?}"
        );
    }

    // And the column beside it, which a Session row and a Workspace row both carry.
    let archives = vec![
        column_of(&h, "Archive workspace space-troopers"),
        column_of(&h, "Archive session Fix climbing bugs"),
    ];
    assert!(
        (archives[0] - archives[1]).abs() < 0.5,
        "the archiving control must be in one column: {archives:?}"
    );
    assert!(
        archives[0] < first,
        "and it must be to the left of the destructive one"
    );

    // Every control occupies exactly the slot the row reserved for it, whatever glyph it
    // carries. A button sized by its own glyph is what made the columns drift: a wider icon
    // made a wider button, and every button after it moved.
    for label in [
        "Stop all sessions in space-troopers",
        "Archive workspace space-troopers",
        "New session in space-troopers",
        "Close session Fix climbing bugs",
        "Archive session Fix climbing bugs",
    ] {
        let rect = h
            .query_by_label(label)
            .unwrap_or_else(|| panic!("no control named {label:?}"))
            .rect();
        assert!(
            (rect.width() - turn_gui::icons::ROW_SIZE.x).abs() < 0.5
                && (rect.height() - turn_gui::icons::ROW_SIZE.y).abs() < 0.5,
            "{label} is {:?}, not the {:?} its slot reserved",
            rect.size(),
            turn_gui::icons::ROW_SIZE
        );
    }
}

/// A Session is created *in* a Workspace, and a global `+ Session` could not say which
/// one. The control lives on the Workspace's row, and it says which Workspace in its name.
/// Archiving and closing live beside it, because they are the same kind of thing — an act
/// on this row — and are told apart by their icon, their name and their tooltip.
#[test]
fn a_workspace_row_carries_creation_archiving_and_closing() {
    let mut fixture = busy_desk();
    fixture.permission = None;
    fixture.queue.clear();
    let workspace_id = fixture.hierarchy.as_ref().expect("hierarchy").workspaces[0]
        .workspace
        .id
        .clone();
    if let Some(snapshot) = fixture.hierarchy.as_mut() {
        // Collapsed, so all three Workspace rows and their controls are in one image, and
        // one of them archived — where the control is disabled rather than absent, because
        // a control that vanishes teaches nothing about why.
        snapshot.tree_state.expanded.clear();
        if let Some(last) = snapshot.workspaces.last_mut() {
            last.workspace.archived = true;
        }
    }
    // An archived row is only in the tree when the preference says so, and this image is
    // about what the row's controls do when it is.
    fixture.include_archived = true;
    let mut h = harness(fixture);
    h.run();
    h.run();
    h.snapshot("workspace_row_controls");

    let buttons = button_labels(&h);
    for workspace in ["space-troopers", "turn"] {
        for control in [
            format!("New session in {workspace}"),
            format!("Archive workspace {workspace}"),
            format!("Stop all sessions in {workspace}"),
        ] {
            assert!(
                buttons.iter().any(|label| label == &control),
                "every Workspace row offers {control:?}; found {buttons:?}"
            );
        }
    }
    // The archived one offers the way back rather than a second way out, and its name says
    // which Workspace it would restore.
    assert!(
        buttons
            .iter()
            .any(|label| label == "Restore workspace personal-infra"),
        "an archived Workspace's control is the one that undoes it; found {buttons:?}"
    );

    let selected_before = h.state().state.selected_tree.clone();
    h.query_by_label("New session in space-troopers")
        .expect("the per-workspace control is a real button")
        .click();
    h.run_steps(1);
    assert_eq!(
        h.state()
            .state
            .session_draft
            .as_ref()
            .map(|draft| draft.workspace_id.clone()),
        Some(workspace_id.clone()),
        "the draft is already pointed at the Workspace whose row was used"
    );
    assert_eq!(
        h.state().state.selected_tree,
        selected_before,
        "the control must not double as a click on the row underneath it"
    );

    // Closing asks; it does not close. Nothing leaves the window on this click.
    h.state_mut().state.session_draft = None;
    h.state_mut().actions.clear();
    h.run_steps(1);
    h.query_by_label("Stop all sessions in space-troopers")
        .expect("the destructive control is a real button")
        .click();
    h.run_steps(1);
    assert_eq!(
        h.state().actions,
        Vec::new(),
        "the control must not stop anything by itself"
    );
    assert!(
        matches!(
            h.state().state.lifecycle_confirmation,
            Some(LifecycleConfirmation::StopWorkspace { workspace_id: ref opened, .. })
                if opened == &workspace_id
        ),
        "it opens the confirmation for its own Workspace, got {:?}",
        h.state().state.lifecycle_confirmation
    );

    // Restoring an archived Workspace is a request with a flag, not a stop.
    h.state_mut().state.lifecycle_confirmation = None;
    h.state_mut().actions.clear();
    h.run_steps(1);
    h.query_by_label("Restore workspace personal-infra")
        .expect("the archived Workspace can be brought back")
        .click();
    h.run_steps(1);
    assert!(
        matches!(
            h.state().actions.as_slice(),
            [ViewAction::ArchiveWorkspace {
                archived: false,
                ..
            }]
        ),
        "got {:?}",
        h.state().actions
    );
}

/// The three states a row's archive control has to tell apart, in one image: a Session with
/// work running (archiving is refused, closing is offered), a Session with nothing running
/// (archiving is offered), and an archived one (only the way back).
fn tree_of_session_rows() -> Fixture {
    let mut fixture = busy_desk();
    fixture.permission = None;
    fixture.queue.clear();
    let snapshot = fixture.hierarchy.as_mut().expect("hierarchy");
    // Workspaces expanded, Sessions collapsed: this image is about the Session rows.
    snapshot
        .tree_state
        .expanded
        .retain(|key| matches!(key, HierarchyKey::Workspace { .. }));
    let second = &mut snapshot.workspaces[1];
    let mut idle = second.sessions[0].clone();
    // Nothing running, and the derived state says so: the daemon would never send `running`
    // with a count of zero, and a fixture that did would be teaching the row to lie.
    idle.session.running_count = 0;
    idle.session.display_state = DisplayState::Idle;
    idle.session.state_label = "idle".into();
    idle.session.needs_user = false;
    idle.session.badge_count = 0;
    idle.nodes.clear();
    idle.session.name = "Ship the release notes".into();
    idle.session.id = SessionId::from_stored("sess_releasenotes");

    let mut archived = idle.clone();
    archived.session.id = SessionId::from_stored("sess_lastmonth");
    archived.session.name = "Last month's spike".into();
    archived.session.status = turn_core::model::SessionStatus::Archived;
    second.sessions.push(idle);
    second.sessions.push(archived);
    second.workspace = WorkspaceSummary {
        session_count: second.sessions.len(),
        ..second.workspace.clone()
    };
    fixture
}

/// Every Session row carries the same pair, and the pair is the whole point: one of them
/// takes the row out of the way, the other stops the work, and they never look alike.
#[test]
fn a_session_row_offers_archiving_and_closing_as_different_acts() {
    let mut fixture = tree_of_session_rows();
    fixture.include_archived = true;
    let mut h = harness(fixture);
    h.run();
    h.run();
    h.snapshot("session_row_controls");

    let buttons = button_labels(&h);
    for control in [
        "Archive session Fix climbing bugs",
        "Close session Fix climbing bugs",
        "Archive session Ship the release notes",
        "Close session Ship the release notes",
        // The archived row offers the way back instead of a second way out.
        "Restore session Last month's spike",
    ] {
        assert!(
            buttons.iter().any(|label| label == control),
            "{control:?} must be a named control; found {buttons:?}"
        );
    }

    // Archiving is a request with a flag on it. Nothing in this path can stop a process.
    h.state_mut().actions.clear();
    h.query_by_label("Archive session Ship the release notes")
        .expect("a Session with nothing running can be archived")
        .click();
    h.run_steps(1);
    assert_eq!(
        h.state().actions,
        vec![ViewAction::ArchiveSession {
            session_id: SessionId::from_stored("sess_releasenotes"),
            archived: true,
        }],
        "archiving asks for exactly one thing, and it is not a termination"
    );
    assert!(
        h.state().state.lifecycle_confirmation.is_none(),
        "archiving needs no confirmation, because it destroys nothing"
    );

    // Closing asks first, and asks about the row it belongs to.
    h.state_mut().actions.clear();
    h.query_by_label("Close session Fix climbing bugs")
        .expect("the destructive control is a real button")
        .click();
    h.run_steps(1);
    assert_eq!(
        h.state().actions,
        Vec::new(),
        "the control must not stop anything by itself"
    );
    assert_eq!(
        h.state().state.lifecycle_confirmation,
        Some(LifecycleConfirmation::EndSession {
            session_id: SessionId::from_stored("sess_fixclimbing"),
            name: "Fix climbing bugs".into(),
            running_count: 6,
            escaped_count: 0,
        })
    );
}

/// An archived row is out of the way. Archiving is only believable if the row actually
/// leaves, so this is the half of the pair where the preference is off.
///
/// A test and an image each, rather than one test taking two: a harness collects its
/// snapshot results and writes them together, so two `snapshot` calls in one test wrote the
/// *same* frame to both files — and a pair of identical images would have shown the row
/// leaving whether it did or not.
#[test]
fn an_archived_session_is_out_of_the_tree_while_the_preference_is_off() {
    let mut h = harness(tree_of_session_rows());
    h.run();
    h.run();
    h.snapshot("archived_session_hidden");
    let hidden = tree_row_labels(&h);
    assert!(
        !hidden
            .iter()
            .any(|label| label.contains("Last month's spike")),
        "an archived Session must not be in the tree while the preference is off; got {hidden:?}"
    );
    assert!(
        hidden
            .iter()
            .any(|label| label.contains("Ship the release notes")),
        "and the Sessions that are not archived stay; got {hidden:?}"
    );
}

/// The other half: nothing was lost, and the preference in Settings brings it back saying
/// what it is.
#[test]
fn the_archived_preference_brings_the_row_back_and_the_row_says_it_is_archived() {
    let mut fixture = tree_of_session_rows();
    fixture.include_archived = true;
    let mut h = harness(fixture);
    h.run();
    h.run();
    h.snapshot("archived_session_shown");
    let shown = tree_row_labels(&h);
    let archived_row = shown
        .iter()
        .find(|label| label.contains("Last month's spike"))
        .expect("the preference brings the row back");
    assert!(
        archived_row.contains("archived"),
        "and the row says what it is: {archived_row:?}"
    );
}

/// Closing a Workspace reaches every Session in it, so the question says how many there are
/// and how many are working. "This workspace" is not a quantity.
#[test]
fn closing_a_workspace_says_how_many_sessions_it_would_stop() {
    let mut fixture = tree_of_session_rows();
    fixture.include_archived = true;
    let workspace = fixture.hierarchy.as_ref().expect("hierarchy").workspaces[1].clone();
    let mut h = harness(fixture);
    h.state_mut().state.lifecycle_confirmation =
        Some(LifecycleConfirmation::stop_workspace(&workspace));
    h.run();
    h.run();
    h.snapshot("workspace_close_confirmation");

    h.state_mut().actions.clear();
    h.query_by_label("Stop all sessions")
        .expect("the destructive action is a visible button")
        .click();
    h.run_steps(1);
    assert_eq!(
        h.state().actions,
        vec![ViewAction::CloseWorkspace {
            workspace_id: workspace.workspace.id.clone(),
            disposition: CloseDisposition::Terminate,
        }],
        "and only the accepted confirmation asks for the stop"
    );
}

/// The third verb, and the one the tree had no way to reach: getting rid of something for good.
///
/// What to look for in the image: the word "Delete", the count of what will be stopped, and —
/// the important one — the **path that is not being deleted**, spelled out. Somebody reading
/// "delete workspace" is asking a question about their code, and the answer has to be on
/// screen rather than implied. The way back out is named too: archiving keeps everything.
#[test]
fn deleting_a_workspace_names_the_directory_it_will_not_touch() {
    let mut fixture = tree_of_session_rows();
    fixture.include_archived = true;
    let workspace = fixture.hierarchy.as_ref().expect("hierarchy").workspaces[1].clone();
    let mut h = harness(fixture);
    h.state_mut().state.lifecycle_confirmation =
        Some(LifecycleConfirmation::delete_workspace(&workspace));
    h.run();
    h.run();
    h.snapshot("workspace_delete_confirmation");

    // The promise is checkable: the exact root is on screen, not a phrase about files.
    let shown = all_text(&h);
    assert!(
        shown
            .iter()
            .any(|line| line.contains(&workspace.workspace.root)),
        "the dialog must name the directory it leaves alone; got {shown:?}"
    );
    assert!(
        shown.iter().any(|line| line.contains("cannot be undone")),
        "and it must say that this one does not come back: {shown:?}"
    );

    h.state_mut().actions.clear();
    h.query_by_label("Delete workspace")
        .expect("the destructive action is a visible button")
        .click();
    h.run_steps(1);
    assert_eq!(
        h.state().actions,
        vec![ViewAction::DeleteWorkspace {
            workspace_id: workspace.workspace.id.clone(),
            disposition: CloseDisposition::Terminate,
        }],
        "and only the accepted confirmation asks for it"
    );
}

/// The Session half of the same act.
///
/// What to look for: the same shape as the Workspace one, one line shorter, and the sentence
/// that separates Turn's record from the user's work.
#[test]
fn deleting_a_session_says_what_goes_and_what_stays() {
    let mut fixture = tree_of_session_rows();
    fixture.include_archived = true;
    let session = fixture.hierarchy.as_ref().expect("hierarchy").workspaces[1].sessions[0]
        .session
        .clone();
    let mut h = harness(fixture);
    h.state_mut().state.lifecycle_confirmation =
        Some(LifecycleConfirmation::delete_session(&session));
    h.run();
    h.run();
    h.snapshot("session_delete_confirmation");

    let shown = all_text(&h);
    assert!(
        shown
            .iter()
            .any(|line| line.contains("files, branches and worktrees are not touched")),
        "the dialog must separate Turn's record from the user's work: {shown:?}"
    );

    h.state_mut().actions.clear();
    h.query_by_label("Delete session")
        .expect("the destructive action is a visible button")
        .click();
    h.run_steps(1);
    assert_eq!(
        h.state().actions,
        vec![ViewAction::DeleteSession {
            session_id: session.id.clone(),
            disposition: CloseDisposition::Terminate,
        }],
    );
}

/// A window whose main checkout is waiting to be confirmed. The decision is an authority
/// the user grants, so it stays a named button in the bottom bar rather than becoming a
/// sentence with no way to act on it.
#[test]
fn the_bottom_status_bar_keeps_a_pending_write_confirmation_actionable() {
    let mut fixture = busy_desk();
    fixture.permission = None;
    fixture.queue.clear();
    let mut lease = fixture
        .hierarchy
        .as_ref()
        .expect("hierarchy")
        .workspaces
        .iter()
        .find_map(|workspace| workspace.write_lease.clone())
        .expect("main-checkout lease");
    lease.state = LeaseState::RecoveryRequired;
    if let Some(snapshot) = fixture.hierarchy.as_mut() {
        for workspace in &mut snapshot.workspaces {
            if let Some(lease) = workspace.write_lease.as_mut() {
                lease.state = LeaseState::RecoveryRequired;
            }
        }
    }
    fixture.recovery_lease = Some(lease);
    let mut h = harness(fixture);
    h.run();
    h.run();
    h.snapshot("write_access_status_bar");

    assert!(
        group_labels(&h).iter().any(|group| {
            group.starts_with("Status:") && group.contains("confirm main-checkout write access")
        }),
        "the bottom bar says what is pending, in words: {:?}",
        group_labels(&h)
    );
    h.state_mut().actions.clear();
    h.query_by_label("Confirm write access")
        .expect("the authority decision remains a reachable button")
        .click();
    h.run_steps(1);
    assert!(matches!(
        h.state().actions.as_slice(),
        [ViewAction::ReclaimWorkspaceWriteLease { .. }]
    ));
}

/// The archived filter is a preference about what the list contains, not one of the
/// actions in the Workspaces bar. It moved to Settings and still works.
#[test]
fn the_archived_filter_is_a_setting_rather_than_a_button_in_the_workspaces_bar() {
    let mut h = harness(workspace_without_sessions());
    h.state_mut().state.settings_open = true;
    h.run();
    h.run();

    h.state_mut().actions.clear();
    h.query_by_label("Show archived Workspaces and Sessions")
        .expect("the archived preference is in Settings")
        .click();
    h.run_steps(1);
    assert_eq!(
        h.state().actions,
        vec![ViewAction::SetArchivedVisibility { include: true }]
    );
}

#[test]
fn ending_a_session_requires_confirmation_and_requests_process_termination() {
    let fixture = busy_desk();
    let session_id = fixture.selected.clone().expect("selected Session");
    let mut h = harness(fixture);
    h.state_mut().state.lifecycle_confirmation = Some(LifecycleConfirmation::EndSession {
        session_id: session_id.clone(),
        name: "Fix climbing bugs".into(),
        running_count: 6,
        escaped_count: 0,
    });
    h.run();
    h.snapshot("end_session_confirmation");

    h.state_mut().actions.clear();
    h.query_by_label("End session")
        .expect("the destructive action is a visible button")
        .click();
    h.run_steps(1);
    assert_eq!(
        h.state().actions,
        vec![ViewAction::CloseSession {
            session_id,
            disposition: CloseDisposition::Terminate,
        }]
    );
}

/// The same question when one of the processes cannot be stopped.
///
/// Turn used to refuse this outright, so there was no picture to record: a Session with a
/// survivor of a previous daemon in it could not be ended at all. It can now, and the
/// difference has to be legible in the dialog rather than only in a log — the extra line says
/// what will keep running after the button is pressed, and the button still works.
#[test]
fn a_session_with_an_unstoppable_process_says_what_ending_it_will_not_achieve() {
    let fixture = busy_desk();
    let session_id = fixture.selected.clone().expect("selected Session");
    let mut h = harness(fixture);
    h.state_mut().state.lifecycle_confirmation = Some(LifecycleConfirmation::EndSession {
        session_id: session_id.clone(),
        name: "Fix climbing bugs".into(),
        running_count: 6,
        escaped_count: 2,
    });
    h.run();
    h.snapshot("end_session_confirmation_with_survivors");

    h.state_mut().actions.clear();
    h.query_by_label("End session")
        .expect("a process Turn cannot reach does not disable the act")
        .click();
    h.run_steps(1);
    assert_eq!(
        h.state().actions,
        vec![ViewAction::CloseSession {
            session_id,
            disposition: CloseDisposition::Terminate,
        }]
    );
}

/// A link whose visible text names a different host than its target is asked about, and the
/// question quotes both halves.
///
/// The program in the pane chose the text *and* the target, which is the whole reason this
/// dialog exists — so neither is paraphrased and both are monospace, because a lookalike
/// character is exactly what a proportional font hides.
#[test]
fn a_link_that_does_not_go_where_it_says_is_asked_about_before_it_opens() {
    let mut fixture = busy_desk();
    fixture.link_confirmation = Some(turn_gui::terminal::links::LinkRequest {
        target: turn_gui::terminal::links::LinkTarget::Url(
            "https://evil.example/harvest?token=1".into(),
        ),
        display: "https://evil.example/harvest?token=1".into(),
        text: "https://github.com/theburrowhub/turn/pull/28".into(),
        warning: Some(
            turn_gui::terminal::links::LinkWarning::TextNamesAnotherHost {
                shown: "github.com".into(),
                target: "evil.example".into(),
            },
        ),
    });
    let mut h = harness(fixture);
    h.run();
    h.snapshot("link_confirmation");

    h.state_mut().actions.clear();
    h.query_by_label("Cancel")
        .expect("declining is always available")
        .click();
    h.run_steps(1);
    assert_eq!(
        h.state().actions,
        vec![ViewAction::DismissLink],
        "and it is the decline that is one keystroke away, not the open"
    );
}

/// A settings view with one value at each of three levels, so the picture shows the thing the
/// whole feature is about: which level a value came from and what resetting it would reveal.
fn layered_settings() -> turn_proto::SettingsView {
    use turn_core::settings::{Resolution, Scope, Sensitivity, Shadowed};
    use turn_proto::{SettingsControl, SettingsEntry, SettingsLevel};

    let entry = |key: &str,
                 title: &str,
                 description: &str,
                 control: SettingsControl,
                 accepts: &str,
                 resolution: Resolution,
                 settable_at: Vec<Scope>| SettingsEntry {
        area: turn_core::settings::Area::Appearance,
        area_title: turn_core::settings::Area::Appearance.title().to_string(),
        title: title.to_string(),
        description: description.to_string(),
        accepts: accepts.to_string(),
        control,
        settable_at,
        hidden: false,
        known: true,
        resolution: Resolution {
            key: key.to_string(),
            ..resolution
        },
    };
    let everywhere = vec![Scope::Global, Scope::Workspace, Scope::Session];
    turn_proto::SettingsView {
        session_id: Some(SessionId::from_stored("sess_fixclimbing")),
        levels: vec![
            SettingsLevel::global(),
            SettingsLevel {
                scope: Scope::Workspace,
                owner_id: "ws_spacetroopers".into(),
                label: "space-troopers".into(),
            },
            SettingsLevel {
                scope: Scope::Session,
                owner_id: "sess_fixclimbing".into(),
                label: "Fix climbing bugs".into(),
            },
        ],
        entries: vec![
            // Set at the level the sheet is writing to: this one offers a reset, and the
            // hover on it names the Workspace value that would come back.
            entry(
                "appearance.font_size",
                "Terminal font size",
                "Point size of the monospaced font in every pane.",
                SettingsControl::Integer { min: 6, max: 32 },
                "a whole number from 6 to 32",
                Resolution {
                    key: String::new(),
                    value: serde_json::json!(17),
                    origin: Some(Scope::Session),
                    shadowed: vec![Shadowed {
                        scope: Scope::Workspace,
                        value: serde_json::json!(13),
                    }],
                    sensitivity: Sensitivity::Plain,
                },
                everywhere.clone(),
            ),
            // Inherited: no reset, and the origin line says where it comes from.
            entry(
                "appearance.cursor",
                "Cursor",
                "The shape Turn draws when the program in the pane has not asked for one.",
                SettingsControl::Choice {
                    options: vec!["block".into(), "bar".into(), "underline".into()],
                },
                "one of block, bar, underline",
                Resolution {
                    key: String::new(),
                    value: serde_json::json!("bar"),
                    origin: Some(Scope::Workspace),
                    shadowed: Vec::new(),
                    sensitivity: Sensitivity::Plain,
                },
                everywhere.clone(),
            ),
            // Nobody set it: "Turn's default", which is distinguishable from a level having
            // set the same value and is why `origin` is an Option.
            entry(
                "appearance.ligatures",
                "Font ligatures",
                "Joins sequences like -> into one glyph.",
                SettingsControl::Toggle,
                "on or off",
                Resolution {
                    key: String::new(),
                    value: serde_json::json!(false),
                    origin: None,
                    shadowed: Vec::new(),
                    sensitivity: Sensitivity::Plain,
                },
                everywhere,
            ),
        ],
    }
}

/// The preferences sheet, with a value from each of the three levels.
///
/// What the picture has to show, and what a reviewer should check in it: every value says
/// where it came from, the level being written to is named once at the top rather than implied,
/// and "Reset" appears only on the value this level actually holds.
#[test]
fn the_settings_sheet_says_where_every_value_came_from() {
    let mut fixture = busy_desk();
    fixture.settings = Some(layered_settings());
    let mut h = harness(fixture);
    h.state_mut().state.settings_open = true;
    h.run();
    h.run();
    h.snapshot("settings_levels");
}

/// A change is written at the level the selector names, and nowhere else.
///
/// The failure this rules out is the one that makes a settings sheet dangerous: a user with a
/// Session selected adjusts a font size meaning "here", and it lands on their whole account.
/// The level is chosen once, explicitly, and every write quotes it back with the owner id.
#[test]
fn a_change_is_written_at_the_level_the_selector_names() {
    let mut fixture = busy_desk();
    fixture.settings = Some(layered_settings());
    let mut h = harness(fixture);
    h.state_mut().state.settings_open = true;
    // The Workspace level, chosen deliberately rather than left to default: the default is the
    // Session, and a test that used it could not tell the two apart.
    h.state_mut().state.settings_level = Some(turn_core::settings::Scope::Workspace);
    h.run();
    h.run();

    h.state_mut().actions.clear();
    // By role, not only by name: the preference's title is drawn beside its control as well,
    // so a name alone matches two nodes. The role is also the assertion that the control is
    // announced as a checkbox rather than as a button — a listener needs to know it has a
    // state, not just that it can be pressed.
    h.query_all_by_role(egui::accesskit::Role::CheckBox)
        .find(|node| node.accesskit_node().label().as_deref() == Some("Font ligatures"))
        .expect("the toggle is in the accessibility tree under its own name")
        .click();
    h.run_steps(1);
    assert_eq!(
        h.state().actions,
        vec![ViewAction::SetSetting {
            scope: turn_core::settings::Scope::Workspace,
            owner_id: "ws_spacetroopers".into(),
            key: "appearance.ligatures".into(),
            value: serde_json::json!(true),
        }],
        "the write names the level the selector showed, with that level's own owner id"
    );
}

/// Reset is offered only where there is something to remove.
///
/// A greyed-out reset on an inherited value teaches nothing, and one that silently did nothing
/// would be worse. So the control appears exactly when the chosen level is the level holding
/// the value — which means switching the selector changes which rows offer it.
#[test]
fn reset_is_offered_only_at_the_level_that_holds_the_value() {
    let mut fixture = busy_desk();
    fixture.settings = Some(layered_settings());
    let mut h = harness(fixture);
    h.state_mut().state.settings_open = true;

    // At the Global level nothing in this fixture is set, so nothing can be reset.
    h.state_mut().state.settings_level = Some(turn_core::settings::Scope::Global);
    h.run();
    h.run();
    assert!(
        h.query_by_label("Reset Terminal font size").is_none(),
        "no value here belongs to the Global level"
    );

    // At the Session level the font size is, so exactly that row offers it.
    h.state_mut().state.settings_level = Some(turn_core::settings::Scope::Session);
    h.run();
    h.run();
    h.state_mut().actions.clear();
    // Named per preference, not just "Reset": nine identical buttons are nine identical
    // announcements to anyone listening to the sheet rather than looking at it.
    h.query_by_label("Reset Terminal font size")
        .expect("the Session holds the font size")
        .click();
    h.run_steps(1);
    assert_eq!(
        h.state().actions,
        vec![ViewAction::ResetSetting {
            scope: turn_core::settings::Scope::Session,
            owner_id: "sess_fixclimbing".into(),
            key: "appearance.font_size".into(),
        }]
    );
}

/// The keyboard sheet, with a conflict the user made and can see.
///
/// The picture has to show the thing a shortcut editor exists to prevent: two commands on one
/// chord, where the second never fires and nothing else in Turn would say so. The row that
/// loses says which other command took its key, and the count is stated once above the list
/// because it is a fact about the set rather than about any row.
#[test]
fn the_keyboard_sheet_names_a_chord_bound_to_two_commands() {
    let mut h = harness(busy_desk());
    // The conflict, made the way a user would: a chord typed into a second command that
    // another one already had.
    let taken = turn_gui::keymap::Chord::parse("Mod+Shift+Q").expect("a chord");
    h.state_mut().keymap = Keymap::build(
        &Overrides::new()
            .bind(turn_gui::keymap::Command::ArchiveSession, taken)
            .bind(turn_gui::keymap::Command::ZoomPane, taken),
        Platform::MAC,
    );
    h.state_mut().state.shortcuts_open = true;
    h.run();
    h.run();
    h.snapshot("keyboard_conflict");
}

/// Typing a chord asks for the rebind, and an empty field asks to unbind.
#[test]
fn typing_a_chord_rebinds_the_command_it_belongs_to() {
    let mut h = harness(busy_desk());
    h.state_mut().state.shortcuts_open = true;
    h.run();
    h.run();

    // The chord as typed, put in the draft the field is editing. Typed character by character
    // through the harness would be the same assertion with a dependency on how egui delivers
    // text events.
    h.state_mut()
        .state
        .shortcut_drafts
        .insert("pane.zoom".into(), "Mod+Shift+J".into());
    h.query_all_by_role(egui::accesskit::Role::TextInput)
        .find(|node| node.accesskit_node().label().as_deref() == Some("Maximise pane (toggle)"))
        .expect("every command's chord is editable under its own name")
        .focus();
    h.run();
    h.state_mut().actions.clear();
    // Committed on Enter, never per keystroke: a write per character would be a round trip per
    // character, and "Mod+" on its own is not a chord anybody meant.
    h.key_press(egui::Key::Enter);
    h.run();
    assert!(
        h.state().actions.iter().any(|action| matches!(
            action,
            ViewAction::RebindCommand { command, chord }
                if command == "pane.zoom" && chord.contains("Mod+Shift+J")
        )),
        "got {:?}",
        h.state().actions
    );
}

#[test]
fn the_attention_queue_is_an_explicit_overlay_not_a_second_navigator() {
    let mut h = harness(busy_desk());
    h.state_mut().state.attention_panel_open = true;
    h.run();
    h.snapshot("attention_queue");
}

#[test]
fn an_empty_window_says_so_rather_than_looking_broken() {
    let mut h = harness(Fixture {
        // This is the actual first frame: without a daemon there is no fabricated
        // hierarchy response. The primary action remains visible but disabled at submit.
        hierarchy: None,
        connection: Some(ConnectionState::Disconnected {
            message: "no Turn daemon is listening. Your processes keep running; reconnecting"
                .into(),
            retrying: true,
        }),
        ..Fixture::default()
    });
    h.run();
    h.snapshot("empty");
}

#[test]
fn a_connected_empty_store_has_a_real_workspace_onboarding_action() {
    let mut h = harness(Fixture {
        hierarchy: Some(HierarchySnapshot::empty("window-snapshot", 1)),
        connection: Some(connected()),
        ..Fixture::default()
    });
    h.run();
    let labels: Vec<String> = h
        .query_all_by_role(egui::accesskit::Role::Button)
        .filter_map(|node| node.accesskit_node().label())
        .collect();
    assert!(labels.iter().any(|label| label == "Create workspace"));
    h.snapshot("empty_connected");
}

#[test]
fn the_workspace_form_is_a_visible_first_step_not_a_log_message() {
    let mut h = harness(Fixture {
        hierarchy: Some(HierarchySnapshot::empty("window-snapshot", 1)),
        connection: Some(connected()),
        ..Fixture::default()
    });
    h.state_mut().state.workspace_draft = Some(WorkspaceDraft {
        name: "turn".into(),
        root: "/Users/x/personal-workspace/turn".into(),
        name_is_derived: true,
        continue_to_session: true,
        request_name_focus: true,
        submitting: false,
        error: None,
    });
    h.run();
    h.snapshot("new_workspace");
}

#[test]
fn browsing_for_a_workspace_is_an_explicit_accessible_view_action() {
    let mut h = harness(Fixture {
        hierarchy: Some(HierarchySnapshot::empty("window-snapshot", 1)),
        connection: Some(connected()),
        ..Fixture::default()
    });
    h.state_mut().state.workspace_draft = Some(WorkspaceDraft {
        name: "turn".into(),
        root: "/Users/x/personal-workspace/turn".into(),
        name_is_derived: true,
        continue_to_session: true,
        request_name_focus: false,
        submitting: false,
        error: None,
    });
    h.run_steps(2);

    h.state_mut().actions.clear();
    h.query_by_label("Browse…")
        .expect("the folder chooser is a visible accessible button")
        .click();
    h.run_steps(1);

    assert_eq!(
        h.state().actions,
        vec![ViewAction::ChooseWorkspaceDirectory]
    );
}

#[test]
fn cmd_n_has_a_real_session_form_with_workspace_template_and_task() {
    let fixture = workspace_without_sessions();
    let workspace_id = fixture.workspaces[0].id.clone();
    let template_id = fixture.templates[0].id.clone();
    let mut h = harness(fixture);
    let mut draft = SessionDraft::new(workspace_id, Some(template_id));
    draft.name = "Fix startup onboarding".into();
    draft.task = "Make Cmd+N create a visible, selected Session.".into();
    h.state_mut().state.session_draft = Some(draft);
    h.run();
    h.snapshot("new_session");
}

#[test]
fn a_layout_preset_is_created_in_the_visual_row_and_column_editor() {
    let mut h = harness(workspace_without_sessions());
    h.state_mut().state.layout_draft = Some(LayoutTemplateDraft::two_shells(
        LayoutEditorOrigin::NewSession,
    ));
    h.run();
    h.snapshot("layout_editor");
}

/// The editor is where a layout is designed, so it has the same five-zone gesture — and it
/// applies the move to its own draft, since no daemon owns a template that does not exist
/// yet. Which makes cancelling load-bearing in a way it is not for a session pane: the drop
/// happens when the pointer is released, so an abandoned drag has to be forgotten or letting
/// go would still move the cell.
#[test]
fn escape_during_a_drag_in_the_layout_editor_leaves_the_draft_alone() {
    let mut h = harness(workspace_without_sessions());
    let mut draft = LayoutTemplateDraft::two_shells(LayoutEditorOrigin::NewSession);
    let cells: Vec<PaneId> = draft
        .layout
        .panes()
        .into_iter()
        .map(|pane| pane.id.clone())
        .collect();
    draft.dragged_pane = Some(cells[1].clone());
    let untouched = draft.layout.clone();
    h.state_mut().state.layout_draft = Some(draft);
    h.run();

    h.key_press(egui::Key::Escape);
    h.run_steps(1);
    let after = h
        .state()
        .state
        .layout_draft
        .clone()
        .expect("the sheet is still open");
    assert_eq!(
        after.dragged_pane, None,
        "the gesture must be forgotten, or releasing the pointer would still move the cell"
    );
    assert_eq!(after.layout, untouched, "and nothing may have moved yet");

    // Letting go afterwards, over the other cell, changes nothing.
    h.drop_at(egui::pos2(400.0, 300.0));
    h.run_steps(1);
    assert_eq!(
        h.state()
            .state
            .layout_draft
            .as_ref()
            .expect("the sheet is still open")
            .layout,
        untouched
    );
}

#[test]
fn settings_exposes_layout_presets_as_a_first_class_section() {
    let mut h = harness(workspace_without_sessions());
    h.state_mut().state.settings_open = true;
    h.run();
    h.snapshot("settings_layout_presets");
}

/// `Cmd+N` can beat the daemon's template response. When the choices arrive on a later
/// frame, the sheet must apply the same Workspace default -> first preset policy as an
/// immediately populated draft. There is no hidden preference for a legacy Coding preset.
#[test]
fn templates_arriving_after_the_session_sheet_select_the_first_available_preset() {
    let mut fixture = workspace_without_sessions();
    fixture.workspaces[0].default_template = None;
    let starter = TemplateSummary::from_template(&Template::two_shells(T0));
    let coding = TemplateSummary::from_template(&Template::coding(T0));
    let starter_id = starter.id.clone();
    fixture.templates = vec![starter, coding];
    let workspace_id = fixture.workspaces[0].id.clone();

    let mut h = harness(fixture);
    h.state_mut().state.session_draft = Some(SessionDraft::new(workspace_id, None));
    h.run_steps(1);

    assert_eq!(
        h.state()
            .state
            .session_draft
            .as_ref()
            .and_then(|draft| draft.template_id.as_ref()),
        Some(&starter_id)
    );
}

fn pane_writes(actions: &[ViewAction]) -> Vec<&[u8]> {
    actions
        .iter()
        .filter_map(|action| match action {
            ViewAction::Pane {
                action: PaneAction::Write(bytes),
                ..
            } => Some(bytes.as_slice()),
            _ => None,
        })
        .collect()
}

/// A modal is a keyboard lease, not just dark paint over a still-focused terminal.
/// This drives real egui Text, Paste and Key events through a focused pane and the
/// workspace sheet in the same frame — the ordering that previously leaked into PTY.
#[test]
fn workspace_onboarding_owns_keyboard_events_even_while_a_pane_remains_focused() {
    let mut h = harness(busy_desk());
    h.state_mut().state.workspace_draft = Some(WorkspaceDraft {
        name: "turn".into(),
        root: "/Users/x/personal-workspace/turn".into(),
        name_is_derived: true,
        continue_to_session: false,
        request_name_focus: true,
        submitting: false,
        error: None,
    });
    h.run_steps(2);

    assert!(
        h.query_all_by_role(egui::accesskit::Role::TextInput)
            .any(|node| node.is_focused()),
        "the first onboarding TextEdit must receive real egui focus"
    );
    let dialog = h
        .query_by_role(egui::accesskit::Role::Dialog)
        .expect("onboarding is exposed as a dialog, not an unrelated group of fields");
    assert!(dialog.accesskit_node().is_modal());
    assert_eq!(
        dialog.accesskit_node().label().as_deref(),
        Some("Create Workspace")
    );
    let field_labels: Vec<_> = h
        .query_all_by_role(egui::accesskit::Role::TextInput)
        .filter_map(|node| node.accesskit_node().label())
        .collect();
    assert!(field_labels.iter().any(|label| label == "Name"));
    assert!(field_labels.iter().any(|label| label == "Project folder"));
    assert!(h.query_by_label("Browse…").is_some());
    h.state_mut().actions.clear();
    h.event(egui::Event::Text("-typed".into()));
    h.event(egui::Event::Paste("-pasted".into()));
    h.key_press(egui::Key::ArrowLeft);
    h.run_steps(1);

    assert!(
        pane_writes(&h.state().actions).is_empty(),
        "workspace Text/Paste/Key leaked into the focused PTY: {:?}",
        pane_writes(&h.state().actions)
    );
    let draft = h.state().state.workspace_draft.as_ref().unwrap();
    assert!(draft.name.contains("-typed"));
    assert!(draft.name.contains("-pasted"));

    h.state_mut().actions.clear();
    h.key_press(egui::Key::Enter);
    h.run_steps(1);
    assert!(pane_writes(&h.state().actions).is_empty());
    assert!(h
        .state()
        .actions
        .iter()
        .any(|action| matches!(action, ViewAction::CreateWorkspace { .. })));
}

/// The dimmed background is inert, including controls that mutate ViewState directly
/// instead of returning a ViewAction. This reproduces a click on `+ Session` while the
/// Workspace sheet is already open and proves the click lands on the modal shield.
#[test]
fn onboarding_blocks_clicks_through_to_background_controls() {
    let mut h = harness(busy_desk());
    h.state_mut().state.workspace_draft = Some(WorkspaceDraft {
        name: "turn".into(),
        root: "/Users/x/personal-workspace/turn".into(),
        name_is_derived: true,
        continue_to_session: false,
        request_name_focus: true,
        submitting: false,
        error: None,
    });
    h.run_steps(2);

    h.query_by_label("New session in space-troopers")
        .expect("the background session control remains rendered")
        .click();
    h.run_steps(1);

    assert!(
        h.state().state.workspace_draft.is_some(),
        "the foreground Workspace sheet must remain open"
    );
    assert!(
        h.state().state.session_draft.is_none(),
        "the click must not activate the background New Session control"
    );
}

/// Session creation has the same keyboard boundary and its single-line name field
/// submits with Enter. The Enter is a real event and must become exactly a creation
/// intent, never terminal input.
#[test]
fn session_onboarding_captures_input_and_enter_submits_without_writing_to_the_pty() {
    let mut fixture = busy_desk();
    let workspace = fixture.hierarchy.as_ref().unwrap().workspaces[0]
        .workspace
        .clone();
    let template = TemplateSummary::from_template(&Template::coding(T0));
    let workspace_id = workspace.id.clone();
    let template_id = template.id.clone();
    fixture.workspaces = vec![workspace];
    fixture.templates = vec![template];

    let mut h = harness(fixture);
    let mut draft = SessionDraft::new(workspace_id.clone(), Some(template_id.clone()));
    draft.name = "Fix keyboard lease".into();
    h.state_mut().state.session_draft = Some(draft);
    h.run_steps(2);

    assert!(
        h.query_all_by_role(egui::accesskit::Role::TextInput)
            .any(|node| node.is_focused()),
        "the session name must receive real egui focus"
    );
    assert_eq!(
        h.query_by_role(egui::accesskit::Role::TextInput)
            .and_then(|node| node.accesskit_node().label()),
        Some("Session name (optional)".into())
    );
    assert_eq!(
        h.query_by_role(egui::accesskit::Role::MultilineTextInput)
            .and_then(|node| node.accesskit_node().label()),
        Some("Task note (optional)".into())
    );
    let combo_labels: Vec<_> = h
        .query_all_by_role(egui::accesskit::Role::ComboBox)
        .filter_map(|node| node.accesskit_node().label())
        .collect();
    assert!(combo_labels.iter().any(|label| label == "Workspace"));
    assert!(combo_labels.iter().any(|label| label == "Layout preset"));
    h.state_mut().actions.clear();
    h.event(egui::Event::Text(" safely".into()));
    h.event(egui::Event::Paste(" now".into()));
    h.key_press(egui::Key::ArrowLeft);
    h.run_steps(1);
    assert!(
        pane_writes(&h.state().actions).is_empty(),
        "session Text/Paste/Key leaked into the focused PTY: {:?}",
        pane_writes(&h.state().actions)
    );

    h.state_mut().actions.clear();
    h.key_press(egui::Key::Enter);
    h.run_steps(1);
    assert!(
        pane_writes(&h.state().actions).is_empty(),
        "the submit Enter leaked into the focused PTY"
    );
    assert!(h.state().actions.iter().any(|action| matches!(
        action,
        ViewAction::CreateSessionFromTemplate {
            workspace_id: created_workspace,
            template_id: created_template,
            name,
            ..
        } if created_workspace == &workspace_id
            && created_template == &template_id
            && name.contains("Fix keyboard lease")
    )));
}

/// Enter keeps its ordinary editing meaning in the optional multiline task. Only the
/// single-line name owns Enter as submit, so writing a multi-paragraph task cannot
/// accidentally launch an agent halfway through the note.
#[test]
fn enter_in_the_session_task_inserts_a_newline_instead_of_submitting() {
    let mut fixture = busy_desk();
    let workspace = fixture.hierarchy.as_ref().unwrap().workspaces[0]
        .workspace
        .clone();
    let template = TemplateSummary::from_template(&Template::coding(T0));
    let workspace_id = workspace.id.clone();
    let template_id = template.id.clone();
    fixture.workspaces = vec![workspace];
    fixture.templates = vec![template];

    let mut h = harness(fixture);
    let mut draft = SessionDraft::new(workspace_id, Some(template_id));
    draft.name = "Fix keyboard lease".into();
    draft.task = "First paragraph".into();
    h.state_mut().state.session_draft = Some(draft);
    h.run_steps(2);

    let task = h
        .query_by_role(egui::accesskit::Role::MultilineTextInput)
        .expect("the task note is a multiline text input");
    task.focus();
    h.run_steps(2);
    h.state_mut().actions.clear();
    h.key_press(egui::Key::Enter);
    h.run_steps(1);

    assert!(pane_writes(&h.state().actions).is_empty());
    assert!(!h
        .state()
        .actions
        .iter()
        .any(|action| matches!(action, ViewAction::CreateSessionFromTemplate { .. })));
    assert!(
        h.state()
            .state
            .session_draft
            .as_ref()
            .unwrap()
            .task
            .contains('\n'),
        "Enter in the task note must remain a newline"
    );
}

/// The strongest form of the banner, which is the one most worth reviewing as an image:
/// a destructive command, the directory it would run in, and no way to approve it here.
#[test]
fn a_high_risk_permission_shows_the_command_and_the_directory() {
    let mut fixture = busy_desk();
    fixture.permission = Some(PendingPermission {
        summary: "Run `rm -rf build && git clean -fdx`".into(),
        command: Some("rm -rf build && git clean -fdx".into()),
        risk: Risk::High,
        blocked_secs: 214,
        ..permission(Risk::High, false)
    });
    let mut h = harness(fixture);
    h.run();
    h.snapshot("high_risk_permission");
}

/// A demand a heuristic inferred has to look like a guess, both in the banner and in the
/// queue. This is the snapshot that proves it does.
#[test]
fn an_inferred_permission_and_an_inferred_relationship_are_drawn_as_guesses() {
    let mut fixture = busy_desk();
    fixture.permission = Some(permission(Risk::Medium, true));
    let reviewer = fixture
        .hierarchy
        .as_mut()
        .expect("hierarchy")
        .workspaces
        .iter_mut()
        .flat_map(|workspace| &mut workspace.sessions)
        .flat_map(|session| &mut session.nodes)
        .find(|node| {
            node.agent
                .as_ref()
                .is_some_and(|agent| agent.name.display_name == "Reviewer")
        })
        .expect("Reviewer node");
    reviewer.relationship.confidence = Confidence::InferredHigh;
    reviewer.relationship_is_provisional = true;
    fixture.queue = vec![
        queue_item("Reviewer", AwaitingReason::Input, true, true),
        queue_item("Fix climbing bugs", AwaitingReason::Permission, true, false),
        queue_item(
            "Draft release notes",
            AwaitingReason::Question,
            false,
            false,
        ),
    ];
    let mut h = harness(fixture);
    h.run();
    h.snapshot("inferred_relationship");
}

/// Thirty real Session branches exercise density in the one persistent hierarchy.
#[test]
fn thirty_sessions_stay_legible_in_the_unified_tree() {
    let mut fixture = busy_desk();
    fixture.permission = None;
    fixture.queue.clear();
    let snapshot = fixture.hierarchy.as_mut().expect("hierarchy");
    let workspace = &mut snapshot.workspaces[0];
    let workspace_id = workspace.workspace.id.clone();
    let mut sessions = Vec::new();
    for index in 0..30 {
        let mut session = Session::new(
            workspace_id.clone(),
            format!("Task {index:02} on a longish branch name"),
            "/Users/x/personal-workspace/space-troopers",
            fixture.layout.clone().expect("layout"),
            T0 + index as i64,
        );
        session.id = SessionId::from_stored(format!("sess_scale_{index:02}"));
        session.mode = if index == 0 {
            SessionMode::MainCheckout
        } else if index % 2 == 0 {
            session.read_only_enforced = true;
            SessionMode::ReadOnly
        } else {
            session.worktree_path = Some(format!("/Users/x/worktrees/task-{index:02}"));
            session.checkout_id = CheckoutId::from_stored(format!("checkout_task_{index:02}"));
            SessionMode::IsolatedWorktree
        };
        let mut agent = ProcessNode::agent(
            session.id.clone(),
            "claude",
            session.cwd.clone(),
            T0 + index as i64,
        );
        agent.id = NodeId::from_stored(format!("agent_scale_{index:02}"));
        agent.lifecycle = if index % 9 == 4 {
            Lifecycle::Exited { code: 1 }
        } else {
            Lifecycle::Alive
        };
        agent.turn = Some(match index % 9 {
            1 => Turn::AwaitingUser {
                reason: AwaitingReason::Permission,
            },
            2 => Turn::Done,
            3 => Turn::TaskDone,
            5 => Turn::AwaitingUser {
                reason: AwaitingReason::Question,
            },
            _ => Turn::Active,
        });
        agent.agent.as_mut().expect("agent detail").name =
            AgentName::declared(format!("Agent {index:02}"));
        add_preview(
            &mut agent,
            &format!("Working on task {index:02}…"),
            PreviewSource::SemanticEvent,
            Confidence::Integrated,
        );
        session.tree.insert(agent);
        let badge = usize::from(session.needs_user());
        let summary = SessionSummary::from_session(&session, badge, index % 7 == 0, T0 + 15_000);
        sessions.push(SessionTreeView {
            session: summary,
            nodes: TreeNodeView::for_session(&session, T0 + 15_000),
        });
    }
    workspace.workspace.session_count = sessions.len();
    workspace.workspace.sessions_needing_user = sessions
        .iter()
        .filter(|session| session.session.needs_user)
        .count();
    workspace.workspace.badge_count = workspace.workspace.sessions_needing_user;
    workspace.sessions = sessions;
    let selected = workspace.sessions[1].session.id.clone();
    snapshot.tree_state.selected = Some(HierarchyKey::session(selected.clone()));
    snapshot.tree_state.expanded = vec![HierarchyKey::workspace(workspace_id)];
    fixture.selected = Some(selected);
    let mut h = harness(fixture);
    h.run();
    h.snapshot("thirty_sessions");
}

/// A full-screen application in the alternate screen: the case a terminal has to get
/// right, and the one where a pane that scrolled under the program would be wrong.
#[test]
fn a_full_screen_application_fills_its_pane() {
    let (layout, panes) = three_pane_layout();
    let mut tui = screen(
        &[
            "┌ Status ─────────────────┐┌ Log ──────────────────────┐",
            "│ ● On branch climb       ││ 3f2a1c  fix grip physics  │",
            "│   modified: physics.rs  ││ 9b04de  add ledge tests   │",
            "│   modified: climb.rs    ││ 71ce88  wip               │",
            "│                         ││                           │",
            "└─────────────────────────┘└───────────────────────────┘",
            " [1] Status [2] Branches [3] Log        ? for help",
        ],
        40,
        56,
    );
    tui.alternate_screen = true;
    tui.modes.application_cursor = true;
    tui.modes.mouse = turn_proto::MouseMode::ButtonMotion;
    tui.cursor = Some((1, 3));
    // The selected row of a TUI arrives as a background, not as text.
    for col in 1..25u16 {
        if let Some(cell) = tui.cell_mut(2, col) {
            cell.bg = Some(Rgb::new(0x2a, 0x3a, 0x50));
        }
    }

    let mut grids = BTreeMap::new();
    let mut titles = BTreeMap::new();
    grids.insert(panes[0].clone(), tui);
    titles.insert(panes[0].clone(), "lazygit".to_string());
    grids.insert(panes[1].clone(), screen(&["idle"], 20, 46));
    grids.insert(panes[2].clone(), screen(&["~/space-troopers $ "], 20, 46));

    let mut fixture = busy_desk();
    fixture.grids = grids;
    fixture.titles = titles;
    fixture.layout = Some(layout);
    fixture.focused = Some(panes[0].clone());
    fixture.permission = None;
    fixture.queue = Vec::new();
    let snapshot = fixture.hierarchy.as_mut().expect("hierarchy");
    let nodes = &mut snapshot.workspaces[0].sessions[0].nodes;
    for node in nodes.iter_mut() {
        node.pane_bindings.clear();
    }
    let fang = nodes
        .iter_mut()
        .find(|node| node.kind == NodeKind::Tui)
        .expect("TUI node");
    fang.title = "lazygit".into();
    fang.activity_preview = Some(ActivityPreview {
        node_id: fang.node_id.clone(),
        raw_source_sequence: Some(80),
        normalized_text: "On branch climb · 2 modified files".into(),
        source: PreviewSource::StableScreenLine,
        confidence: Confidence::Integrated,
        stable: true,
        contains_sensitive_data: false,
        redacted: false,
        updated_ms: T0 + 15_000,
    });
    fang.pane_bindings.push(PaneNodeBinding {
        pane_id: panes[0].clone(),
        session_id: fang.session_id.clone(),
        node_id: fang.node_id.clone(),
        temporary: false,
        surface_id: None,
        opened_ms: T0 + 15_000,
    });
    snapshot.tree_state.selected = Some(HierarchyKey::process(fang.node_id.clone()));
    let mut h = harness(fixture);
    h.run();
    h.snapshot("alternate_screen");
}

/// A pane showing history, and saying that Turn's record does not reach all the way back.
/// The honesty is the point: the alternative is a user scrolling up into a lie.
#[test]
fn a_scrolled_pane_says_where_its_record_begins() {
    let (layout, panes) = three_pane_layout();
    let mut grids = BTreeMap::new();
    let mut history = screen(
        &[
            "   Compiling turn-proto v0.1.0",
            "   Compiling turn-core v0.1.0",
            "   Compiling turnd v0.1.0",
            "error[E0599]: no method named `set_size`",
        ],
        40,
        56,
    );
    history.scrollback_offset = 1_240;
    history.scrollback_len = 5_000;
    history.cursor = None;
    grids.insert(panes[0].clone(), history);
    grids.insert(panes[1].clone(), screen(&["idle"], 20, 46));
    grids.insert(panes[2].clone(), screen(&["$ "], 20, 46));

    let mut fixture = busy_desk();
    fixture.grids = grids;
    fixture.titles = BTreeMap::new();
    fixture.layout = Some(layout);
    fixture.focused = Some(panes[0].clone());
    fixture.scrolled = vec![panes[0].clone()];
    fixture.incomplete_history = vec![panes[0].clone()];
    fixture.permission = None;
    fixture.queue = Vec::new();
    let mut h = harness(fixture);
    h.run();
    h.snapshot("scrolled_history");
}

#[test]
fn quick_preview_is_semantic_and_does_not_replace_the_layout() {
    let fixture = busy_desk();
    let reviewer_id = fixture
        .hierarchy
        .as_ref()
        .expect("hierarchy")
        .workspaces
        .iter()
        .flat_map(|workspace| &workspace.sessions)
        .flat_map(|session| &session.nodes)
        .find(|node| {
            node.agent
                .as_ref()
                .is_some_and(|agent| agent.name.display_name == "Reviewer")
        })
        .map(|node| node.node_id.clone())
        .expect("Reviewer");
    let expected_layout = fixture.layout.clone();
    let mut h = harness(fixture);
    h.state_mut().state.quick_preview = Some(HierarchyKey::process(reviewer_id));
    h.run();
    assert_eq!(
        h.state().fixture.layout,
        expected_layout,
        "quick preview must not mutate the saved layout"
    );
    h.snapshot("quick_preview");
}

#[test]
fn a_temporary_reviewer_pane_is_visually_distinct_from_the_saved_layout() {
    let mut fixture = busy_desk();
    let snapshot = fixture.hierarchy.as_mut().expect("hierarchy");
    let reviewer = snapshot
        .workspaces
        .iter_mut()
        .flat_map(|workspace| &mut workspace.sessions)
        .flat_map(|session| &mut session.nodes)
        .find(|node| {
            node.agent
                .as_ref()
                .is_some_and(|agent| agent.name.display_name == "Reviewer")
        })
        .expect("Reviewer");
    let binding = PaneNodeBinding {
        pane_id: PaneId::from_stored("pane_reviewer_temporary"),
        session_id: reviewer.session_id.clone(),
        node_id: reviewer.node_id.clone(),
        temporary: true,
        surface_id: Some(snapshot.tree_state.surface_id.clone()),
        opened_ms: T0 + 15_000,
    };
    reviewer.pane_bindings.push(binding.clone());
    fixture.temporary_previews = fixture
        .preview_history
        .get(&reviewer.node_id)
        .cloned()
        .unwrap_or_default();
    fixture.temporary_pane = Some(NodePaneView {
        binding,
        capability: NodePaneCapability::PreviewDetails,
    });
    let expected_layout = fixture.layout.clone();
    let mut h = harness(fixture);
    h.run();
    assert_eq!(
        h.state().fixture.layout,
        expected_layout,
        "temporary panes live outside the saved layout"
    );
    h.run();
    assert!(
        h.query_all_by_role(egui::accesskit::Role::Group)
            .filter_map(|node| node.accesskit_node().label())
            .any(|label| {
                label.contains("Temporary pane for Reviewer")
                    && label.contains("closing keeps the process alive")
            }),
        "assistive technology must distinguish a temporary view from process lifetime"
    );
    h.snapshot("temporary_reviewer_pane");
}

#[test]
fn a_write_lease_conflict_offers_only_explicit_safe_alternatives() {
    let mut fixture = busy_desk();
    let hierarchy = unified_hierarchy(
        fixture.layout.as_ref().expect("layout"),
        &fixture
            .layout
            .as_ref()
            .expect("layout")
            .panes()
            .into_iter()
            .map(|pane| pane.id.clone())
            .collect::<Vec<_>>(),
    );
    fixture.write_conflict = Some(ProtoErrorContext::WorkspaceWriteLeaseConflict {
        workspace_id: hierarchy.workspace_id,
        checkout_id: hierarchy.checkout_id,
        requesting_session_id: Some(SessionId::from_stored("sess_second_writer")),
        lease: Box::new(hierarchy.lease),
        owner: Box::new(WriteLeaseOwnerView {
            session_id: hierarchy.session_id,
            session_name: "Fix climbing bugs".into(),
            mode: SessionMode::MainCheckout,
            cwd: "/Users/x/personal-workspace/space-troopers".into(),
            branch: Some("fix/climbing-bugs".into()),
            last_activity_ms: T0 + 12_000,
        }),
        alternatives: vec![
            SessionConflictAlternative::FocusOwner,
            SessionConflictAlternative::CreateReadOnly,
            SessionConflictAlternative::CreateIsolatedWorktree,
            SessionConflictAlternative::Cancel,
        ],
    });
    let mut h = harness(fixture);
    h.run();
    h.run();
    let buttons: Vec<String> = h
        .query_all_by_role(egui::accesskit::Role::Button)
        .filter_map(|node| node.accesskit_node().label())
        .collect();
    for alternative in [
        "Focus existing session",
        "Open read-only session",
        "Create isolated worktree",
        "Cancel",
    ] {
        assert!(
            buttons.iter().any(|label| label == alternative),
            "missing typed conflict alternative {alternative:?}; found {buttons:?}"
        );
    }
    h.snapshot("write_lease_conflict");
}

#[test]
fn the_command_palette_lists_commands_with_their_shortcuts() {
    let mut h = harness(busy_desk());
    h.state_mut().state.palette.open();
    h.state_mut().state.palette.set_query("pane");
    // The text cursor blinks, so this one is stepped rather than run to quiescence.
    h.run_steps(3);
    h.snapshot("palette");
}

/// The keyboard half of the row controls, in the place a user goes looking for a command
/// they cannot see. All four are here, each with the chord that runs it — which is also how
/// the palette teaches the pairing: the Workspace act is the Session chord plus Option.
#[test]
fn the_palette_offers_every_way_to_close_or_archive_with_its_chord() {
    let mut h = harness(busy_desk());
    h.state_mut().state.palette.open();
    h.state_mut().state.palette.set_query("archive");
    h.run_steps(3);
    h.snapshot("palette_archive");

    let rows = palette_rows(&h);
    for wanted in [
        "Archive session — take it out of the tree, stop nothing — Session — Shift+Cmd+Y",
        "Archive workspace — take it out of the tree, stop nothing — Workspace — Opt+Shift+Cmd+Y",
    ] {
        assert!(
            rows.iter().any(|row| row == wanted),
            "the palette must offer {wanted:?}; found {rows:?}"
        );
    }

    h.state_mut().state.palette.set_query("close");
    h.run_steps(3);
    let rows = palette_rows(&h);
    for wanted in [
        "End session — stop its processes and take its row out of the tree — Session — Shift+Cmd+K",
        "Stop all sessions in workspace — the Workspace itself stays in the tree — Workspace — Opt+Shift+Cmd+K",
    ] {
        assert!(
            rows.iter().any(|row| row == wanted),
            "the palette must offer {wanted:?}; found {rows:?}"
        );
    }
}

/// The same criterion, measured on the **real application** rather than on its view.
///
/// `build_eframe` runs `TurnApp` itself: the transport thread, the keymap, the repaint
/// plan. `run` steps until nothing asks for another frame, so a window that repainted
/// continuously would run to the step limit and this would fail. The socket does not
/// exist, which is the case that matters — a window waiting for a daemon must be the
/// cheapest thing on the desk, not a spin.
#[test]
fn the_real_application_settles_while_it_waits_for_a_daemon() {
    let mut h = Harness::builder()
        .with_size(egui::vec2(1280.0, 760.0))
        .build_eframe(|cc| {
            turn_gui::app::TurnApp::new(
                &cc.egui_ctx,
                std::path::PathBuf::from("/tmp/turn-no-such-daemon-for-snapshots.sock"),
                Keymap::build(&Overrides::new(), Platform::MAC),
            )
        });

    let steps = h.run();
    assert!(
        steps <= 6,
        "the application took {steps} frames to settle; something is repainting in a loop"
    );
    assert!(
        h.state().repaint_plan(turn_core::now_ms()).is_idle(),
        "a window with no daemon and nothing open must ask for no frames at all"
    );

    // And it is drawing the honest thing while it waits, rather than a blank window.
    h.run();
    assert!(
        h.query_by_role(egui::accesskit::Role::List).is_some(),
        "the window is composed even with nothing to show"
    );
}

/// Regression for the first-run dead end that prompted the onboarding repair. This
/// drives the real native application with a real macOS Command+N event; testing the
/// keymap and the form separately would allow the wiring between them to break again.
#[test]
fn command_n_opens_workspace_onboarding_in_the_real_application() {
    let mut h = Harness::builder()
        .with_size(egui::vec2(1280.0, 760.0))
        .build_eframe(|cc| {
            turn_gui::app::TurnApp::new(
                &cc.egui_ctx,
                std::path::PathBuf::from("/tmp/turn-no-such-daemon-for-command-n.sock"),
                Keymap::build(&Overrides::new(), Platform::MAC),
            )
        });
    h.run();

    h.key_press_modifiers(
        egui::Modifiers {
            command: true,
            mac_cmd: true,
            ..egui::Modifiers::NONE
        },
        egui::Key::N,
    );
    h.run_steps(2);

    assert!(
        h.query_by_label("CREATE WORKSPACE").is_some(),
        "Command+N must produce visible onboarding, not a log-only notice"
    );
    assert!(
        h.query_by_label("Create and continue").is_some(),
        "first-run onboarding must continue into the first Session"
    );
}

/// The product's most explicit performance criterion, measured rather than asserted.
///
/// `Harness::run` steps until nothing asks for another frame. An idle window settles in a
/// couple of passes; one that repainted continuously would run to the step limit and this
/// would fail. The window under test has three panes, thirty sessions and a queue — the
/// busy case, not a blank one.
#[test]
fn an_idle_window_settles_instead_of_repainting_in_a_loop() {
    let mut h = harness(busy_desk());
    let steps = h.run();
    assert!(
        steps <= 6,
        "an idle window took {steps} frames to settle; something is repainting in a loop"
    );

    // And it stays settled: another run from the same state costs the same handful of
    // frames rather than growing.
    let again = h.run();
    assert!(again <= 6, "a second pass took {again} frames");

    // The window is not asking to be woken immediately either — which is what
    // `request_repaint_after(ZERO)` would look like, and what a continuous repaint is.
    let delay = h
        .output()
        .viewport_output
        .values()
        .map(|viewport| viewport.repaint_delay)
        .min()
        .unwrap_or(std::time::Duration::MAX);
    assert!(
        delay > std::time::Duration::ZERO,
        "the window asked for an immediate repaint with nothing to draw"
    );
}

/// The accessibility tree is not a nice-to-have here: a terminal UI drawn on a GPU has no
/// DOM, so if this is empty the window does not exist for a screen reader.
///
/// `kittest` drives the same AccessKit tree a screen reader would read, so this asserts
/// the real thing rather than a parallel description of it.
///
/// The hierarchy is a real AccessKit `Tree` with `TreeItem` descendants. This guards the
/// product correction itself: falling back to the removed flat `SessionRow` list would
/// make this test fail even if a PNG happened to look plausible.
#[test]
fn every_hierarchy_level_is_a_reachable_tree_item() {
    let mut h = harness(busy_desk());
    // Two frames: egui builds the AccessKit tree from the previous frame's widgets, so a
    // single pass has nothing in it yet.
    h.run();
    h.run();

    let tree = h
        .query_by_role(egui::accesskit::Role::Tree)
        .expect("the unified workspace hierarchy is an accessibility tree");
    assert!(
        tree.accesskit_node()
            .label()
            .is_some_and(|label| label.contains("Workspaces, sessions and processes")),
        "the tree describes its unified contents"
    );
    assert_eq!(
        h.query_all_by_role(egui::accesskit::Role::ListItem).count(),
        0,
        "the normative fixture must not silently render the removed flat SessionRow list"
    );

    let rows: Vec<(String, Option<usize>)> = h
        .query_all_by_role(egui::accesskit::Role::TreeItem)
        .filter_map(|node| {
            node.accesskit_node()
                .label()
                .map(|label| (label, node.accesskit_node().level()))
        })
        .collect();
    assert!(
        rows.len() >= 12,
        "workspace, sessions, agents, tools and child processes must be tree rows; found {rows:?}"
    );

    for (fragment, level) in [
        ("Workspace space-troopers — 1 sessions", 1),
        (
            "Session Fix climbing bugs — mode MAIN — YOUR TURN — 1 attention demand",
            2,
        ),
        ("AGENT Claude Code — PERMISSION", 3),
        ("SUBAGENT Reviewer — running", 4),
        ("SUBAGENT Tests — running", 4),
        ("TESTS Jest worker — running", 5),
        ("SHELL Shell — running", 3),
        ("TUI Fang (files) — running", 3),
    ] {
        assert!(
            rows.iter()
                .any(|(label, actual_level)| label.contains(fragment)
                    && *actual_level == Some(level)),
            "expected hierarchy row {fragment:?} at level {level}; found {rows:?}"
        );
    }

    assert!(
        rows.iter()
            .any(|(label, _)| label.contains("Reviewer")
                && label.contains("Reviewing climb_system.gd")),
        "stable semantic previews must be audible: {rows:?}"
    );

    let selected: Vec<String> = h
        .query_all_by_role(egui::accesskit::Role::TreeItem)
        .filter(|node| node.accesskit_node().is_selected() == Some(true))
        .filter_map(|node| node.accesskit_node().label())
        .collect();
    assert!(
        selected.iter().any(|label| label.contains("Claude Code")),
        "selection is independent and belongs to the selected AgentNode; found {selected:?}"
    );
}

/// The pane itself has to be in the tree too, with its contents readable — a terminal
/// that announced only its geometry would be useless.
#[test]
fn a_terminal_pane_offers_its_screen_to_a_screen_reader() {
    let mut h = harness(busy_desk());
    h.run();
    h.run();

    let panes: Vec<(Option<String>, Option<String>)> = h
        .query_all_by_role(egui::accesskit::Role::Terminal)
        .map(|node| (node.accesskit_node().label(), node.value()))
        .collect();
    assert_eq!(
        panes.len(),
        3,
        "one node per pane on screen; found {panes:?}"
    );

    let agent = panes
        .iter()
        .find(|(_, value)| {
            value
                .as_deref()
                .is_some_and(|text| text.contains("Do you want to allow this?"))
        })
        .expect("the agent's screen must be readable, not only visible");
    let label = agent.0.as_deref().unwrap_or_default();
    assert!(
        label.contains("rows by") && label.contains("columns"),
        "a pane must say its shape: {label}"
    );
    assert!(
        label.contains("focused"),
        "and which one has the keyboard: {label}"
    );
}

/// A window with nothing in it still has to be navigable, and must not claim to have rows
/// it does not have.
#[test]
fn an_empty_window_has_an_empty_unified_tree() {
    let mut h = harness(Fixture {
        hierarchy: Some(HierarchySnapshot::empty("window-snapshot", 1)),
        connection: Some(connected()),
        ..Fixture::default()
    });
    h.run();
    h.run();
    assert_eq!(
        h.query_all_by_role(egui::accesskit::Role::TreeItem).count(),
        0
    );
    let tree = h
        .query_by_role(egui::accesskit::Role::Tree)
        .expect("the unified tree exists even when it is empty");
    assert!(
        tree.accesskit_node()
            .label()
            .is_some_and(|label| label.contains('0')),
        "the tree must say how many rows it has: {:?}",
        tree.accesskit_node().label()
    );
}

/// Attention remains a logical queue and is not rendered as a second persistent navigator.
#[test]
fn attention_is_exposed_without_reintroducing_a_second_navigation_list() {
    let mut fixture = busy_desk();
    // A snoozed demand first, so "next" is not simply "the top row".
    fixture.queue = vec![
        queue_item(
            "Draft release notes",
            AwaitingReason::Question,
            false,
            false,
        ),
        queue_item("Fix climbing bugs", AwaitingReason::Permission, true, false),
    ];
    let mut h = harness(fixture);
    h.run();
    h.run();

    let rows: Vec<String> = h
        .query_all_by_role(egui::accesskit::Role::TreeItem)
        .filter_map(|node| node.accesskit_node().label())
        .collect();
    assert!(
        rows.iter()
            .any(|label| label.contains("Claude Code") && label.contains("PERMISSION")),
        "the exact AgentNode needing attention remains explicit: {rows:?}"
    );
    assert_eq!(
        h.query_all_by_role(egui::accesskit::Role::ListItem).count(),
        0,
        "the logical queue must not reappear as a persistent navigation list"
    );
}

/// The window must draw at any size somebody can drag it to, including one too small for
/// its own chrome. A panic here is a crash on a window resize.
#[test]
fn the_window_survives_being_dragged_smaller_than_its_own_chrome() {
    for size in [
        egui::vec2(1280.0, 760.0),
        egui::vec2(700.0, 400.0),
        egui::vec2(320.0, 200.0),
        egui::vec2(80.0, 60.0),
    ] {
        let mut h = Harness::builder().with_size(size).build_ui_state(
            |ui, window: &mut Window| {
                let Window {
                    fixture,
                    state,
                    theme,
                    keymap,
                    ..
                } = window;
                theme.install(ui.ctx());
                fixture.view().ui(ui, theme, keymap, state);
            },
            window(busy_desk()),
        );
        h.run_steps(2);
    }
}

/// The size in cells a pane reports has to match what is painted, or a program lays itself
/// out for a width Turn never drew and truncates its own file names. Measured through the
/// same font the window uses, because a cell taken from anywhere else is the defect.
#[test]
fn a_panes_reported_size_matches_the_cells_it_can_paint() {
    let context = egui::Context::default();
    let theme = Theme::dark();
    let mut measured = None;
    turn_gui::frames::run(&context, |ui| {
        measured = theme.cell_size(ui);
    });
    let cell = measured.expect("the bundled monospace face can be measured");

    // A pane of exactly a hundred columns and thirty rows of that cell.
    let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, cell * egui::vec2(100.0, 30.0));
    let size = turn_gui::panes::size_in_cells(rect, cell);
    assert_eq!(size, PtySize::new(30, 100));

    // And the pane paints exactly the rows it claims to have.
    let grid = Grid::blank(size.rows, size.cols);
    let rows = turn_gui::terminal::visible_rows(&grid, rect.min, cell, rect);
    assert_eq!(rows, 0..30);

    // The invented cell reported a size neither the drawing nor the font agreed with.
    assert_ne!(cell, egui::vec2(8.0, 17.0));
}

// ---------------------------------------------------------------------------------------
// The terminal grid itself.
//
// These render one pane, without the window's chrome, because what they are about is the
// lattice: whether a border meets the border above it, whether a table's columns line up,
// whether an emoji shifts the rest of its row. A whole-window snapshot answers none of
// those questions — the panes in it are too small to see a one-pixel seam.
// ---------------------------------------------------------------------------------------

/// The cell the window will actually use, measured the way the window measures it.
fn measured_cell(theme: &Theme) -> egui::Vec2 {
    let context = egui::Context::default();
    let mut cell = None;
    turn_gui::frames::run(&context, |ui| {
        cell = theme.cell_size(ui);
    });
    cell.expect("the bundled monospace face can be measured")
}

/// Renders one pane, sized to exactly the grid it holds, and returns the harness so the
/// caller can snapshot it or read its pixels.
fn pane_harness(grid: Grid, focused: bool) -> Harness<'static, ()> {
    selected_pane_harness(grid, focused, None)
}

/// The same, with a selection painted over it.
fn selected_pane_harness(
    grid: Grid,
    focused: bool,
    selection: Option<Selection>,
) -> Harness<'static, ()> {
    let theme = Theme::dark();
    let cell = measured_cell(&theme);
    let size = egui::vec2(cell.x * f32::from(grid.cols), cell.y * f32::from(grid.rows));
    let mut harness = Harness::builder().with_size(size).build_ui(move |ui| {
        theme.install(ui.ctx());
        turn_gui::terminal::paint(
            ui,
            &theme,
            egui::Rect::from_min_size(egui::Pos2::ZERO, size),
            &grid,
            selection.as_ref(),
            turn_gui::terminal::PaneOptions {
                focused,
                now_ms: cursor_on(),
                ..Default::default()
            },
        );
    });
    harness.run();
    harness
}

/// Fills a grid from lines of text, leaving the rest blank.
fn grid_of(lines: &[&str], rows: u16, cols: u16) -> Grid {
    let mut grid = screen(lines, rows, cols);
    // No cursor: these images are about the lattice, and a blinking block over the first
    // corner is a hole in the exact place a reviewer looks first.
    grid.cursor = None;
    grid
}

/// A frame with every join in it. If any arm stops short of its cell edge, this image shows
/// it as a broken corner; the report's screenshot showed exactly that, drawn from the
/// font's own glyphs.
#[test]
fn a_box_drawn_frame_joins_at_every_corner_and_tee() {
    let grid = grid_of(
        &[
            "┌──────────────┬─────────────────────────────┐",
            "│ light        │ ┏━━━━━━━━━━━━━━━━━━━━━━━━━┓ │",
            "│ frame        │ ┃ heavy frame inside it   ┃ │",
            "├──────────────┼─╂─────────────────────────┨ │",
            "│ a tee ┬ and  │ ┃ ├─┼─┤ mixed ╀ joins ╂   ┃ │",
            "│ a cross ┼ in │ ┗━━━━━━━━━━━━━━━━━━━━━━━━━┛ │",
            "│ running text │                             │",
            "└──────────────┴─────────────────────────────┘",
            "╭──── rounded ─────╮ ╔═══ double ═══╦═══════╗",
            "│ ╌╌╌╌ dashed ╌╌╌╌ │ ║ two strokes  ║ meet  ║",
            "╰──────────────────╯ ╚═══════════════╩═══════╝",
            "─ │ ┼ ┴ ┬ ├ ┤ ┌ ┐ └ ┘ ━ ┃ ╋ ╱ ╲ ╳ · ▏▎▍▌▋▊▉█",
        ],
        12,
        46,
    );
    pane_harness(grid, false).snapshot("terminal_box_frame");
}

/// A table: the case where a column that drifts by a fraction of a cell is unmissable,
/// because the numbers stop lining up with their heading.
#[test]
fn a_table_keeps_its_columns_aligned_down_the_pane() {
    let mut grid = grid_of(
        &[
            "┏━━━━━━━━━━━━━━━━━━━━━┳━━━━━━━━┳━━━━━━━━━━┳━━━━━━━━━┓",
            "┃ crate               ┃  tests ┃     time ┃ result  ┃",
            "┡━━━━━━━━━━━━━━━━━━━━━╇━━━━━━━━╇━━━━━━━━━━╇━━━━━━━━━┩",
            "│ turn-core           │    412 │   0.31 s │ ok      │",
            "│ turn-proto          │    288 │   0.18 s │ ok      │",
            "│ turn-gui            │    193 │   4.02 s │ ok      │",
            "│ turnd               │    301 │   1.77 s │ ok      │",
            "├─────────────────────┼────────┼──────────┼─────────┤",
            "│ total               │  1 194 │   6.28 s │ ok      │",
            "└─────────────────────┴────────┴──────────┴─────────┘",
        ],
        10,
        53,
    );
    // The heading in bold, and the totals row in the theme's own attention colour, so the
    // image also proves a coloured run still lands on the grid.
    for col in 0..53u16 {
        if let Some(cell) = grid.cell_mut(1, col) {
            cell.attrs = CellAttrs::default().with(CellAttrs::BOLD);
        }
        if let Some(cell) = grid.cell_mut(8, col) {
            cell.fg = Some(Rgb::new(0xe8, 0xa8, 0x3a));
        }
    }
    pane_harness(grid, false).snapshot("terminal_table");
}

/// Wide glyphs. Each takes two columns, and the rules above and below say whether the rest
/// of the row moved: if a wide cell were painted one column wide, every pipe after it would
/// step left.
#[test]
fn a_wide_glyph_takes_two_columns_without_shifting_its_row() {
    let mut grid = grid_of(
        &[
            "┌────┬────┬────┬────┐",
            "│    │    │    │    │",
            "├────┼────┼────┼────┤",
            "│ ab │ cd │ ef │ gh │",
            "└────┴────┴────┴────┘",
            "0123456789 123456789",
        ],
        6,
        21,
    );
    // Two emoji and two ideographs in the cells of row 1, each with its WIDE_TRAILER to the
    // right, which paints background only. The bundled faces have no CJK coverage, so the
    // ideographs come out as the missing-glyph box — a font question, not a layout one: the
    // box still occupies its two columns and the rules below stay where they were.
    for (row, col, glyph) in [
        (1u16, 2u16, "🔥"),
        (1, 7, "🔒"),
        (1, 12, "中"),
        (1, 17, "文"),
    ] {
        assert!(grid.set_wide(row, col, glyph), "no room for {glyph}");
    }
    pane_harness(grid, false).snapshot("terminal_wide_glyphs");
}

/// The screenshot from the report, rebuilt: a file browser with nested frames, a highlighted
/// row, a scrollbar and a right-hand info panel. The file names are the tell — the report
/// showed them truncated to `.g...` and `po...` because the program had been told a width
/// Turn never drew.
#[test]
fn a_file_browser_pane_shows_its_frames_columns_and_names_whole() {
    let mut grid = grid_of(
        &[
            "╭─ ~/personal-workspace/turn ────────────────╮╭─ ARCHITECTURE.md ──────────────╮",
            "│ ▸ crates/                                  ││ # Turn                         │",
            "│   ├── turn-core/                           ││                                │",
            "│   ├── turn-gui/                            ││ One window, one daemon, and    │",
            "│   │   ├── src/terminal/boxdraw.rs          ││ a store that survives a        │",
            "│   │   ├── src/terminal/geometry.rs         ││ restart.                       │",
            "│   │   └── src/theme.rs                     ││                                │",
            "│   └── turnd/                               ││ ## The pane                    │",
            "│ ▸ docs/                                    ││                                │",
            "│   ARCHITECTURE.md                          ││ A pane hosts the user's        │",
            "│   CONTRIBUTING.md                          ││ shell. An agent runs in it.    │",
            "│   Cargo.toml                               ││                                │",
            "│   Makefile                                 ││ ┌ metrics ───────────────────┐ │",
            "│   README.md                                ││ │ cell     8 x 15 px         │ │",
            "│   rust-toolchain.toml                      ││ │ advance  7.82666 pt        │ │",
            "│                                            ││ │ rows     41 x 100          │ │",
            "│                                            ││ └────────────────────────────┘ │",
            "╰────────────────────────────────────────────╯╰────────────────────────────────╯",
            " 15 entries · ▓▓▓▓▓▓▒▒▒▒▒▒▒▒ 34%                j/k move · enter open · q quit  ",
        ],
        19,
        80,
    );
    // The highlighted row arrives as a background, the way a TUI sends it: the whole row,
    // wall to wall, which is where a gap between cells would be unmissable.
    for col in 1..44u16 {
        if let Some(cell) = grid.cell_mut(9, col) {
            cell.bg = Some(Rgb::new(0x2a, 0x3a, 0x50));
        }
    }
    // The directory rows in the running colour, dimmed status at the bottom.
    for row in [1u16, 8] {
        for col in 1..44u16 {
            if let Some(cell) = grid.cell_mut(row, col) {
                cell.fg = Some(Rgb::new(0x6a, 0x9e, 0xd8));
                cell.attrs = CellAttrs::default().with(CellAttrs::BOLD);
            }
        }
    }
    for col in 0..80u16 {
        if let Some(cell) = grid.cell_mut(18, col) {
            cell.attrs = CellAttrs::default().with(CellAttrs::DIM);
        }
    }
    pane_harness(grid, true).snapshot("terminal_file_browser");
}

/// The pixels, not the recording. A snapshot only says the image matches the last one
/// recorded; this says the line is a line, all the way across. It fails on the font's own
/// glyphs, whose strokes stop short of the cell box and leave a gap at every boundary.
#[test]
fn a_drawn_rule_is_continuous_in_the_pixels_it_paints() {
    let cols = 24u16;
    let rows = 6u16;
    // A full-width rule and a full-height rule, so both directions are one run of cells with
    // nothing else in them. The corners of a frame are covered by the unit tests, which can
    // see the geometry rather than guess at it from pixels.
    let horizontal = "─".repeat(cols as usize);
    let mut lines = vec![horizontal.as_str()];
    lines.extend(std::iter::repeat_n("│", rows as usize - 1));
    let grid = grid_of(&lines, rows, cols);

    let mut harness = pane_harness(grid, false);
    let image = harness.render().expect("the pane renders");
    let lit = |x: u32, y: u32| -> bool {
        let pixel = image.get_pixel(x, y);
        u32::from(pixel.0[0]) + u32::from(pixel.0[1]) + u32::from(pixel.0[2]) > 150
    };
    let (width, height) = (image.width(), image.height());

    let rule_row = (0..height / 4)
        .find(|y| lit(width / 2, *y))
        .expect("a horizontal rule in the first row of cells");
    let gaps: Vec<u32> = (0..width).filter(|x| !lit(*x, rule_row)).collect();
    assert!(
        gaps.is_empty(),
        "the horizontal rule has gaps at {gaps:?} of {width} pixels"
    );

    let rule_col = (0..width / 8)
        .find(|x| lit(*x, height / 2))
        .expect("a vertical rule in the first column of cells");
    // From the second row of cells down: the first row holds the horizontal rule instead.
    let second_row = measured_cell(&Theme::dark()).y as u32;
    let gaps: Vec<u32> = (second_row..height)
        .filter(|y| !lit(rule_col, *y))
        .collect();
    assert!(
        gaps.is_empty(),
        "the vertical rule has gaps at {gaps:?} of the {height} pixels below {second_row}"
    );
}

/// "Loose and doubled" measured. The bundled face draws `│` as a 1.17-point stroke at a
/// fractional offset, so the GPU spreads it over two pixel columns at 85% and 62% of the
/// foreground: a soft grey double line. Drawn by Turn it is one column at full strength.
///
/// The property is scale-independent — no partially covered pixel anywhere in a rule — which
/// is what "crisp" means and what the report was looking at.
#[test]
fn a_rule_is_one_crisp_column_of_pixels_rather_than_a_soft_smear() {
    let rows = 8u16;
    let cols = 4u16;
    let lines = vec!["│"; rows as usize];
    let grid = grid_of(&lines, rows, cols);

    let mut harness = pane_harness(grid, false);
    let image = harness.render().expect("the pane renders");
    let value = |x: u32, y: u32| -> u32 {
        let pixel = image.get_pixel(x, y);
        u32::from(pixel.0[0]) + u32::from(pixel.0[1]) + u32::from(pixel.0[2])
    };
    let cell = measured_cell(&Theme::dark());
    let background = value(image.width() - 1, image.height() / 2);
    let middle = image.height() / 2;
    let full = (0..cell.x as u32)
        .map(|x| value(x, middle))
        .max()
        .expect("a rule somewhere in the first cell");
    assert!(
        full > background + 300,
        "the rule is not being painted at all: {full} against a background of {background}"
    );

    let mut columns = Vec::new();
    for x in 0..cell.x as u32 {
        let painted = value(x, middle);
        if painted <= background + 20 {
            continue;
        }
        assert_eq!(
            painted, full,
            "the pixel column {x} of the rule is only partly covered ({painted} of {full}), \
             which is the soft doubled line the font produces"
        );
        columns.push(x);
    }
    assert_eq!(
        columns.len(),
        1,
        "at this size a light rule is one pixel wide; it covered {columns:?}"
    );

    // And it is the same pixel on every row, all the way down: no seam, no drift.
    for y in 0..image.height() {
        assert_eq!(
            value(columns[0], y),
            full,
            "the rule is interrupted at the pixel row {y}"
        );
    }
}

/// The other half of the same property: text sits on the same lattice as the borders, so a
/// rule drawn after different amounts of text is one straight line rather than the doubled,
/// drifting pipes in the report. This is the test the old renderer could not pass: it drew a
/// row as one string, and 19 columns of the font's advance are 3.3 pixels short of 19 cells.
#[test]
fn a_rule_after_text_stays_on_the_pixel_column_the_grid_gives_it() {
    let cols = 40u16;
    let rows = 12u16;
    let column_of_the_rule = 19u16;
    let mut lines = Vec::new();
    for row in 0..rows {
        // Text of a different length on every row, so a renderer that let the font decide
        // would put each row's rule in a slightly different place.
        let label = format!("row {row} of {rows}");
        lines.push(format!("│{label:<18}│{:>19}", row * 7));
    }
    let borrowed: Vec<&str> = lines.iter().map(String::as_str).collect();
    let grid = grid_of(&borrowed, rows, cols);

    let mut harness = pane_harness(grid, false);
    let image = harness.render().expect("the pane renders");
    let lit = |x: u32, y: u32| -> bool {
        let pixel = image.get_pixel(x, y);
        u32::from(pixel.0[0]) + u32::from(pixel.0[1]) + u32::from(pixel.0[2]) > 150
    };
    let cell = measured_cell(&Theme::dark());
    let from = (cell.x * f32::from(column_of_the_rule)) as u32;
    let to = (cell.x * f32::from(column_of_the_rule + 1)) as u32;

    let mut columns = std::collections::BTreeSet::new();
    for y in 0..image.height() {
        let found: Vec<u32> = (from..to).filter(|x| lit(*x, y)).collect();
        assert!(
            !found.is_empty(),
            "the rule is missing from the pixel row {y}: it must be inside cell \
             {column_of_the_rule}, pixels {from}..{to}"
        );
        columns.extend(found);
    }
    assert_eq!(
        columns.len(),
        1,
        "every row's rule must be on the same pixel column, not spread over {columns:?}"
    );
}

// ---------------------------------------------------------------------------------------
// Selection and the pane's own menu. These are the images that say whether a selection
// looks like a selection: the highlight is the only feedback a drag gives, and a menu that
// greys an item without saying why is the defect the menu exists to remove.
// ---------------------------------------------------------------------------------------

/// A double-click on a compiler error. The whole of `src/main.rs:42` is highlighted —
/// including the colon and the line number, which a word class borrowed from a text editor
/// would have split at three separate places.
#[test]
fn a_double_clicked_path_is_highlighted_whole() {
    let grid = grid_of(
        &[
            "error[E0425]: cannot find value `paint` in this scope",
            "  --> src/main.rs:42:17",
            "   |",
            "42 |     let cell = paint(ui, &theme);",
            "   |                ^^^^^ not found in this scope",
        ],
        5,
        54,
    );
    // The cell the pointer was over: inside the path, on the `main` of `src/main.rs:42`.
    let selection = Selection::word(&grid, CellPos::new(1, 12), SelectionKind::Linear);
    assert_eq!(
        selection.text(&grid),
        "src/main.rs:42:17",
        "the image is only worth recording if the selection is the one being claimed"
    );
    selected_pane_harness(grid, true, Some(selection)).snapshot("terminal_word_selection");
}

/// A rectangle over one column of a table. The highlight is a block, not a run of lines:
/// the two columns either side of it are untouched on every row.
#[test]
fn a_rectangular_selection_takes_one_column_out_of_a_table() {
    let mut grid = grid_of(
        &[
            "CONTAINER ID   IMAGE            STATUS         PORTS",
            "9f2b1c4d8e7a   turn/daemon      Up 3 hours     8080/tcp",
            "3c7d5e9f1a2b   turn/gateway     Up 3 hours     9000/tcp",
            "b8e4f6a2c9d1   postgres:17      Up 2 days      5432/tcp",
            "5a1d3f7b9e2c   redis:7          Up 2 days      6379/tcp",
        ],
        5,
        56,
    );
    for col in 0..56u16 {
        if let Some(cell) = grid.cell_mut(0, col) {
            cell.attrs = CellAttrs::default().with(CellAttrs::BOLD);
        }
    }
    // The PORTS column starts at 47 and is eight cells wide.
    let mut selection = Selection::new(CellPos::new(1, 47), SelectionKind::Block);
    selection.extend_to(CellPos::new(4, 55));
    assert_eq!(
        selection.text(&grid),
        "8080/tcp\n9000/tcp\n5432/tcp\n6379/tcp",
        "a linear selection could not produce this, which is why the block kind exists"
    );
    selected_pane_harness(grid, true, Some(selection)).snapshot("terminal_block_selection");
}

/// A selection across a line the terminal broke at the margin. The highlight runs to the
/// end of the first row and continues on the second, because it is one line — and the text
/// it copies has no newline in the middle of the path.
#[test]
fn a_selection_over_a_hard_wrapped_line_covers_both_of_its_rows() {
    // The first row is exactly as wide as the pane, which is what a row that wrapped looks
    // like: the terminal broke it because it ran out of columns.
    let mut grid = grid_of(
        &[
            "$ cargo build --manifest-path /Users/xy/personal-w",
            "orkspace/turn/crates/turn-gui/Cargo.toml --release",
            "   Compiling turn-gui v0.1.0",
            "    Finished `dev` profile in 4.02s",
        ],
        4,
        50,
    );
    // The first row wrapped into the second: the program printed one long command line.
    assert!(grid.set_row_wrapped(0, true));
    // A triple-click anywhere in it takes the logical line, both rows of it.
    let selection = Selection::line(&grid, CellPos::new(1, 10));
    assert_eq!(
        selection.text(&grid),
        "$ cargo build --manifest-path /Users/xy/personal-workspace/turn/crates/turn-gui/Cargo.toml \
         --release",
        "the wrap must not become a newline"
    );
    selected_pane_harness(grid, true, Some(selection)).snapshot("terminal_wrapped_selection");
}

/// The menu, open, with half of it unavailable — and every unavailable item saying why.
///
/// This is the image the whole module exists for: "Copy" greyed with *nothing is selected*
/// under it teaches the user what to do, where a "Copy" that had quietly disappeared would
/// leave them wondering whether the terminal can copy at all.
#[test]
fn the_pane_menu_explains_every_item_it_cannot_offer() {
    let grid = screen(&["$ cargo test", "running 3 tests"], 24, 60);
    let shortcuts = PaneShortcuts::from_keymap(&Keymap::build(&Overrides::new(), Platform::MAC));
    let context = PaneContext {
        close_unavailable: Some("this is the only pane in the session".into()),
        ..PaneContext::default()
    };
    let items = PaneMenu {
        grid: &grid,
        at: CellPos::new(0, 3),
        selection: None,
        context: &context,
        shortcuts: &shortcuts,
        links: None,
    }
    .items();

    // The image is worth recording only if it is showing the state being claimed.
    let unavailable: Vec<PaneCommand> = items
        .iter()
        .filter(|item| !item.enabled())
        .map(|item| item.command)
        .collect();
    assert_eq!(
        unavailable,
        vec![
            PaneCommand::Copy,
            PaneCommand::ClearBuffer,
            PaneCommand::SearchSelection,
            PaneCommand::OpenLink,
            PaneCommand::ClosePane,
        ]
    );
    for item in &items {
        assert!(item.shortcut.is_some(), "{} teaches no chord", item.label());
    }

    menu_harness(items).snapshot("terminal_pane_menu");
}

/// Renders a menu's items in a panel of their own, which is the only way to get a
/// reviewable image of a menu: a popup belongs to a frame that has already ended.
fn menu_harness(items: Vec<MenuItem>) -> Harness<'static, ()> {
    let theme = Theme::dark();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(420.0, 470.0))
        .build_ui(move |ui| {
            theme.install(ui.ctx());
            ui.painter()
                .rect_filled(ui.max_rect(), 0.0, theme.background);
            egui::Frame::menu(ui.style()).show(ui, |ui| {
                ui.set_width(392.0);
                let _ = turn_gui::terminal::menu::show_items(ui, &theme, &items);
            });
        });
    harness.run();
    harness
}

// ---------------------------------------------------------------------------------------
// Searching the scrollback, and the scrollback itself.
//
// These render one pane with its own interaction state, because what they are about is the
// pane: where a match is highlighted, which one is current, what the bar says when nothing
// matched, and where the position indicator sits when the view is a long way back. A
// whole-window image would show all of that four pixels tall.
// ---------------------------------------------------------------------------------------

/// Renders one pane through `show_pane`, with the interaction state the caller set up, and
/// returns the harness so it can be snapshotted.
///
/// Sized to the grid plus a margin, so the search bar has somewhere to sit and the
/// indicator's track is against a real edge.
fn interactive_pane(
    grid: Grid,
    state: PaneInteraction,
    options: PaneOptions,
) -> Harness<'static, PaneInteraction> {
    let theme = Theme::dark();
    let cell = measured_cell(&theme);
    let size = egui::vec2(cell.x * f32::from(grid.cols), cell.y * f32::from(grid.rows));
    let mut harness = Harness::builder().with_size(size).build_ui_state(
        move |ui, state: &mut PaneInteraction| {
            theme.install(ui.ctx());
            let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
            let _ = turn_gui::terminal::show_pane(
                ui,
                state,
                turn_gui::terminal::PaneInput {
                    theme: &theme,
                    rect,
                    grid: &grid,
                    options,
                    id: ui.id().with("pane-under-test"),
                    chrome: None,
                },
            );
        },
        state,
    );
    // Twice: the first frame opens the bar and asks for the keyboard, the second draws it
    // with the focus it was given.
    harness.run();
    harness.run();
    harness
}

/// A build log with an error in it, searched. Several matches are highlighted and the one
/// the user is on is a different colour — a search where every hit looks the same is one
/// where "next" appears to do nothing.
#[test]
fn a_search_highlights_every_match_and_distinguishes_the_current_one() {
    let grid = grid_of(
        &[
            "   Compiling turn-proto v0.1.0 (/Users/x/turn/crates/turn-proto)",
            "   Compiling turn-pty v0.1.0 (/Users/x/turn/crates/turn-pty)",
            "error[E0599]: no method named `set_size` found for struct `Screen`",
            "   --> crates/turn-pty/src/buffer.rs:248:29",
            "    |",
            "248 |         self.parser.screen_mut().set_size(size.rows, size.cols);",
            "    |                                  ^^^^^^^^ method not found",
            "",
            "error[E0308]: mismatched types in crates/turnd/src/core/screens.rs",
            "   --> crates/turnd/src/core/screens.rs:47:31",
            "",
            "error: could not compile `turn-pty` (lib) due to 2 previous errors",
            "warning: build failed, waiting for other jobs to finish...",
            "~/turn on main $ ",
        ],
        14,
        66,
    );

    // The matches the daemon would return for this screen, produced by the daemon's own
    // engine rather than by hand, so the highlights are the ones a real search produces.
    let query = turn_proto::search::SearchQuery::literal("error");
    let outcome = turn_proto::search::search_grid(&grid, &query).expect("a valid query");
    // Four: the two `error[E…]` lines, and both halves of "error: could not compile … due to
    // 2 previous errors".
    assert_eq!(outcome.count(), 4, "{:?}", outcome.matches);

    let mut state = PaneInteraction::default();
    state.search.open_with("error", 0, T0);
    let _ = state.search.take_intents();
    state.search.receive(&query, outcome);
    // The second match: stepping to it is what makes one of the three the current one.
    assert!(state.search.next_match());
    assert!(state.search.next_match());
    assert_eq!(state.search.status(), "2 of 4");
    let _ = state.search.take_intents();

    let highlights = state.search.highlights(&grid);
    assert_eq!(highlights.len(), 4, "{highlights:?}");
    assert_eq!(
        highlights.iter().filter(|h| h.current).count(),
        1,
        "exactly one match is the current one"
    );

    interactive_pane(
        grid,
        state,
        PaneOptions {
            focused: true,
            accepts_input: true,
            now_ms: cursor_on(),
            ..Default::default()
        },
    )
    .snapshot("terminal_search_matches");
}

/// A search that found nothing says so, in the one loud colour, rather than leaving the user
/// to wonder whether it ran.
#[test]
fn a_search_with_no_matches_says_so_in_the_bar() {
    let grid = grid_of(
        &[
            "~/turn on main $ cargo test --workspace",
            "    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.31s",
            "     Running unittests src/lib.rs (target/debug/deps/turn_proto-9c1f)",
            "",
            "running 241 tests",
            "test result: ok. 241 passed; 0 failed; 0 ignored; 0 measured",
            "~/turn on main $ ",
        ],
        14,
        66,
    );

    let query = turn_proto::search::SearchQuery::literal("segfault");
    let outcome = turn_proto::search::search_grid(&grid, &query).expect("a valid query");
    assert!(outcome.is_empty());

    let mut state = PaneInteraction::default();
    state.search.open_with("segfault", 0, T0);
    let _ = state.search.take_intents();
    state.search.receive(&query, outcome);
    assert_eq!(state.search.status(), "no matches");
    assert!(state.search.found_nothing());

    interactive_pane(
        grid,
        state,
        PaneOptions {
            focused: true,
            accepts_input: true,
            now_ms: cursor_on(),
            ..Default::default()
        },
    )
    .snapshot("terminal_search_no_matches");
}

/// The behaviour that decides whether people trust a terminal: output arriving while the
/// user is reading history must leave what they are reading exactly where it is.
///
/// Driven through a real `PaneFeed` — attach, scroll back, then let six more lines arrive —
/// so the image is of the view the feed actually produced and the assertions are about the
/// same view.
#[test]
fn new_output_while_scrolled_back_leaves_the_view_where_it_was() {
    use turn_gui::terminal::feed::PaneFeed;
    use turn_proto::{PaneAttachment, PaneStream, PtySize, ScreenUpdate, TerminalBytes};

    let rows = 14u16;
    let cols = 66u16;
    let line = |index: usize| {
        format!("[{index:04}] compiling turn-proto v0.1.0 — one more line of a long build")
    };

    // The daemon's screen: a build that has already scrolled forty lines past.
    let mut daemon = Grid::blank(rows, cols);
    for row in 0..rows {
        let text = line(40 + usize::from(row));
        for (col, ch) in text.chars().enumerate().take(usize::from(cols)) {
            if let Some(cell) = daemon.cell_mut(row, col as u16) {
                cell.text = ch.to_string();
            }
        }
    }
    daemon.scrollback_len = 40;
    daemon.cursor = Some((rows - 1, 0));

    let mut feed = PaneFeed::attach(&PaneAttachment {
        session_id: SessionId::from_stored("sess_scroll01"),
        pane_id: PaneId::from_stored("pane_scroll01"),
        node_id: None,
        stream: PaneStream::Cells,
        screen: Some(Box::new(daemon.clone())),
        replay: TerminalBytes::new(Vec::new()),
        size: PtySize::new(rows, cols),
        scrollback_truncated: false,
        bytes_seen: 4_096,
        next_seq: 1,
    });

    // The window has never seen those forty rows, so it fetches them the way it would from
    // the daemon: a screen-shaped window at the offset it is showing.
    assert!(feed.scroll_by(30));
    assert_eq!(feed.take_history_request(), Some(30));
    let mut window = Grid::blank(rows, cols);
    for row in 0..rows {
        let text = line(10 + usize::from(row));
        for (col, ch) in text.chars().enumerate().take(usize::from(cols)) {
            if let Some(cell) = window.cell_mut(row, col as u16) {
                cell.text = ch.to_string();
            }
        }
    }
    window.scrollback_offset = 30;
    window.scrollback_len = 40;
    window.cursor = None;
    feed.receive_history(&window);
    let reading = feed.grid().row_text(0);
    assert_eq!(reading, line(10), "the view starts at line ten");

    // Six more lines arrive while the user reads.
    for step in 0..6u64 {
        let mut next = Grid::blank(rows, cols);
        for row in 1..rows {
            next.set_row(row - 1, daemon.row(row));
        }
        let text = line(54 + step as usize);
        for (col, ch) in text.chars().enumerate().take(usize::from(cols)) {
            if let Some(cell) = next.cell_mut(rows - 1, col as u16) {
                cell.text = ch.to_string();
            }
        }
        next.scrollback_len = daemon.scrollback_len + 1;
        next.cursor = Some((rows - 1, 0));
        feed.apply(1 + step, &ScreenUpdate::between(&daemon, &next))
            .expect("the update applies");
        daemon = next;
    }

    assert!(feed.is_scrolled(), "the view was not yanked to the bottom");
    assert_eq!(feed.offset(), 36, "the offset grew by what scrolled off");
    assert_eq!(
        feed.grid().row_text(0),
        reading,
        "and the line the user was reading is still the line on screen"
    );

    let view = feed.grid().clone();
    assert_eq!(view.scrollback_len, 46);
    interactive_pane(
        view,
        PaneInteraction::default(),
        PaneOptions {
            focused: true,
            accepts_input: true,
            now_ms: cursor_on(),
            scrolled: true,
            history_complete: true,
        },
    )
    .snapshot("terminal_scrolled_new_output");
}

/// The position indicator: a thumb against the right edge saying how far through a long
/// record the viewport is, next to a marker that says the same thing in words.
///
/// A count of rows on its own tells the user how far they have come and nothing about how
/// much is left, which is the half of the sentence people notice is missing.
#[test]
fn a_deeply_scrolled_pane_shows_where_it_is_in_the_record() {
    let mut grid = grid_of(
        &[
            "   Compiling turn-core v0.1.0 (/Users/x/turn/crates/turn-core)",
            "   Compiling turn-proto v0.1.0 (/Users/x/turn/crates/turn-proto)",
            "   Compiling turn-store v0.1.0 (/Users/x/turn/crates/turn-store)",
            "   Compiling turn-pty v0.1.0 (/Users/x/turn/crates/turn-pty)",
            "warning: unused variable: `rows`",
            "   --> crates/turn-pty/src/buffer.rs:248:29",
            "    |",
            "248 |         self.parser.screen_mut().set_size(rows, cols);",
            "    |                                           ^^^^ help: `_rows`",
            "    |",
            "   Compiling turn-agents v0.1.0 (/Users/x/turn/crates/turn-agents)",
            "   Compiling turnd v0.1.0 (/Users/x/turn/crates/turnd)",
            "   Compiling turn-gui v0.1.0 (/Users/x/turn/crates/turn-gui)",
            "    Finished `dev` profile [unoptimized + debuginfo] target(s) in 41.02s",
        ],
        14,
        66,
    );
    // A long record, a long way back: the thumb belongs near the top of its track.
    grid.scrollback_offset = 4_600;
    grid.scrollback_len = 5_000;

    let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(520.0, 240.0));
    let options = PaneOptions {
        focused: true,
        accepts_input: true,
        now_ms: cursor_on(),
        scrolled: true,
        history_complete: false,
    };
    let (track, thumb) =
        turn_gui::terminal::scroll_indicator(rect, &grid, options).expect("a position to show");
    assert!(
        thumb.center().y < track.center().y,
        "four hundred rows from the top of five thousand belongs in the top half"
    );
    assert!(
        turn_gui::terminal::scroll_marker_label(4_600, 5_000, false).contains("4600 of 5000"),
        "got {}",
        turn_gui::terminal::scroll_marker_label(4_600, 5_000, false)
    );

    interactive_pane(grid, PaneInteraction::default(), options)
        .snapshot("terminal_scroll_position");
}

// ---------------------------------------------------------------------------------------
// Links.
// ---------------------------------------------------------------------------------------

/// A resolver that knows one path, so a link over a compiler error can be rendered without
/// the image depending on what happens to exist on the machine running the test.
struct KnownPaths(&'static str);

impl turn_gui::terminal::links::PathResolver for KnownPaths {
    fn resolve(&mut self, candidate: &str) -> Option<std::path::PathBuf> {
        (candidate == self.0).then(|| std::path::PathBuf::from("/repo").join(candidate))
    }
}

/// Renders a pane with the link under `pointer` decorated, exactly as a hover draws it.
fn link_harness(grid: Grid, pointer: CellPos, resolves: &'static str) -> Harness<'static, ()> {
    let theme = Theme::dark();
    let cell = measured_cell(&theme);
    let size = egui::vec2(cell.x * f32::from(grid.cols), cell.y * f32::from(grid.rows));
    let mut harness = Harness::builder().with_size(size).build_ui(move |ui| {
        theme.install(ui.ctx());
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
        let options = PaneOptions {
            now_ms: cursor_on(),
            ..Default::default()
        };
        turn_gui::terminal::paint(ui, &theme, rect, &grid, None, options);

        let map = turn_gui::terminal::links::LinkMap::find(&grid, &mut KnownPaths(resolves));
        let link = map
            .at(pointer.row, pointer.col)
            .expect("there is a link under the pointer");
        turn_gui::terminal::paint_link_underline(ui, &theme, rect, cell, link, options);
        // The pointer sits in the middle of the cell, which is where it would be.
        let at = egui::pos2(
            (f32::from(pointer.col) + 0.5) * cell.x,
            (f32::from(pointer.row) + 0.5) * cell.y,
        );
        turn_gui::terminal::paint_link_target(ui, &theme, egui::Id::new("link-snapshot"), at, link);
    });
    harness.run();
    harness
}

/// What the user sees when the pointer rests on a URL: the link underlined where it is, and
/// the whole target next to the pointer with the gesture that opens it.
#[test]
fn a_hovered_url_is_underlined_and_its_whole_target_is_shown() {
    let grid = grid_of(
        &[
            "$ cargo test --workspace",
            "opened https://github.com/TheBurrowHub/turn/pull/42 for the fix",
            "",
            "error[E0308]: mismatched types",
            "  --> src/main.rs:42:8",
            "",
            "serving docs on localhost:3000",
        ],
        10,
        66,
    );
    link_harness(grid, CellPos::new(1, 20), "src/main.rs").snapshot("terminal_link_hover");
}

/// The phishing shape: an OSC 8 hyperlink whose text says one host and whose destination is
/// another. The hover names both and says which one the click would reach.
#[test]
fn a_link_whose_text_names_another_host_shows_the_disagreement() {
    let mut grid = grid_of(
        &[
            "The agent opened a pull request:",
            "",
            "  https://github.com/TheBurrowHub/turn/pull/42",
            "",
            "Review it before merging.",
        ],
        8,
        84,
    );
    assert!(grid.set_row_meta(
        2,
        turn_proto::cells::RowMeta {
            wrapped: false,
            links: vec![turn_proto::cells::RowLink::new(
                2,
                47,
                "https://evil.example/steal?token=1",
            )],
        }
    ));
    link_harness(grid, CellPos::new(2, 4), "").snapshot("terminal_link_disguised");
}

// ---------------------------------------------------------------------------------------
// Inline images.
//
// These are the one feature where a snapshot really is the only evidence. Every other
// property of a picture — which cells it occupies, what happens when the screen scrolls,
// whether an over-large payload is refused — can be asserted on a grid. Whether the pixels
// come out the right way up, in the right cells, at the right shape, cannot.
//
// So each of these drives the **whole path**: a real escape sequence is fed to the daemon's
// own terminal buffer, the grid that comes out is what the window paints, and the payload
// the window uploads is the one the daemon decoded.
// ---------------------------------------------------------------------------------------

/// A picture whose orientation is unmistakable: four quadrants, and a white border.
///
/// Chosen so a mis-tiled, flipped or mirrored picture is obvious at a glance rather than
/// plausible. Red is top-left, green top-right, blue bottom-left, yellow bottom-right.
fn quadrant_rgba(width: u32, height: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        for x in 0..width {
            let border = x < 2 || y < 2 || x + 2 >= width || y + 2 >= height;
            let colour = if border {
                [255, 255, 255, 255]
            } else {
                match (x * 2 < width, y * 2 < height) {
                    (true, true) => [220, 40, 40, 255],
                    (false, true) => [40, 190, 60, 255],
                    (true, false) => [50, 90, 230, 255],
                    (false, false) => [235, 200, 40, 255],
                }
            };
            out.extend_from_slice(&colour);
        }
    }
    out
}

/// The Kitty sequence that transmits and places a raw RGBA picture.
///
/// Raw RGBA rather than a PNG so the test needs no encoder, and Kitty rather than iTerm2 so
/// the cell box is stated in cells and the snapshot is not at the mercy of the nominal cell
/// size the daemon assumes for a pixel request.
fn kitty_rgba(width: u32, height: u32, cols: u16, rows: u16) -> Vec<u8> {
    let payload = turn_proto::encode_base64(&quadrant_rgba(width, height));
    format!("\x1b_Ga=T,f=32,s={width},v={height},c={cols},r={rows};{payload}\x1b\\").into_bytes()
}

/// Feeds a script to the daemon's terminal buffer and returns what a client would receive:
/// the grid, and the payloads for the pictures on it.
fn daemon_pane(rows: u16, cols: u16, script: &[&[u8]]) -> (Grid, Vec<turn_proto::ImagePayload>) {
    let mut buffer = turn_pty::TerminalBuffer::new(turn_pty::ScreenSize::new(rows, cols));
    for chunk in script {
        buffer.write(chunk);
    }
    let mut grid = buffer.grid();
    // No cursor: these images are about the picture, and a blinking block is a hole in one
    // of the cells a reviewer looks at.
    grid.cursor = None;
    let payloads = grid
        .images
        .iter()
        .filter_map(|image| buffer.image_payload(image.id).cloned())
        .collect();
    (grid, payloads)
}

/// Renders one pane with its pictures uploaded, the way the window does.
fn image_pane_harness(
    grid: Grid,
    payloads: Vec<turn_proto::ImagePayload>,
    selection: Option<Selection>,
) -> Harness<'static, ()> {
    let theme = Theme::dark();
    let cell = measured_cell(&theme);
    let size = egui::vec2(cell.x * f32::from(grid.cols), cell.y * f32::from(grid.rows));
    let mut cache = turn_gui::terminal::images::ImageCache::default();
    let mut harness = Harness::builder().with_size(size).build_ui(move |ui| {
        theme.install(ui.ctx());
        for payload in &payloads {
            cache.insert(ui.ctx(), payload);
        }
        turn_gui::terminal::paint_with_images(
            ui,
            &theme,
            egui::Rect::from_min_size(egui::Pos2::ZERO, size),
            &grid,
            turn_gui::terminal::Decoration::selected(selection.as_ref()),
            turn_gui::terminal::PaneOptions {
                now_ms: cursor_on(),
                ..Default::default()
            },
            Some(&mut cache),
        );
    });
    harness.run();
    harness
}

/// A picture among ordinary output, which is what `imgcat` in a shell looks like.
///
/// What to look for: the four-quadrant block sits in the cells between the prompt line above
/// it and the line below, red top-left and yellow bottom-right, its white border unbroken and
/// square on the cell grid. The text around it is undisturbed.
#[test]
fn an_image_sits_inline_among_the_output_around_it() {
    let (grid, payloads) = daemon_pane(
        14,
        56,
        &[
            b"~/turn on main $ kitten icat plot.png\r\n",
            &kitty_rgba(240, 160, 30, 8),
            b"\r\n~/turn on main $ ",
        ],
    );
    assert_eq!(grid.images.len(), 1, "the picture reached the grid");
    assert_eq!(payloads.len(), 1, "and its pixels came with it");
    let mut harness = image_pane_harness(grid, payloads, None);
    harness.snapshot("terminal_inline_image");
}

/// Text around a picture: before it, after it, and below it.
///
/// What to look for: `chart:` to the left of the block on its first row, `<- last run`
/// immediately to the right of the block's **bottom** row — where both iTerm2 and Kitty leave
/// the cursor after drawing — and the sentence below running the full width of the pane. The
/// picture claims exactly its own columns and nothing on any row shifts.
#[test]
fn text_flows_around_an_image_on_the_same_line() {
    let (grid, payloads) = daemon_pane(
        10,
        56,
        &[
            b"chart: ",
            &kitty_rgba(180, 120, 16, 4),
            b" <- last run\r\n",
            b"the line below runs the whole width of the pane, undisturbed\r\n",
        ],
    );
    assert!(grid.row_text(0).starts_with("chart:"));
    assert!(
        grid.row_text(3).contains("<- last run"),
        "the text after a four-row picture belongs beside its bottom row: {:?}",
        grid.row_text(3)
    );
    assert!(grid.row_text(4).starts_with("the line below"));
    let mut harness = image_pane_harness(grid, payloads, None);
    harness.snapshot("terminal_image_with_text_around_it");
}

/// A picture the screen has scrolled halfway out of.
///
/// What to look for: the *bottom* part of the four-quadrant block only — blue and yellow,
/// with the white border along the bottom and sides but **no top edge** — sitting at the top
/// of the pane above the lines that pushed it up. This is the case the tile coordinates in
/// the marker exist for: the picture's own first row is gone, so every surviving row has to
/// say which part of the picture it is.
#[test]
fn an_image_scrolled_partly_off_the_top_shows_the_part_that_is_left() {
    let mut script: Vec<Vec<u8>> = vec![kitty_rgba(200, 200, 20, 10)];
    // Enough output to push the top half of the picture off a ten-row pane.
    for line in 0..9 {
        script.push(format!("output line {line}\r\n").into_bytes());
    }
    let borrowed: Vec<&[u8]> = script.iter().map(|chunk| chunk.as_slice()).collect();
    let (grid, payloads) = daemon_pane(10, 48, &borrowed);

    // The picture is still on screen, and the top row of it carries a tile from the middle
    // of the image rather than its first.
    let first_tile = (0..grid.cols)
        .find_map(|col| {
            grid.cell(0, col)
                .and_then(turn_proto::cells::Cell::image_tile)
        })
        .expect("the picture still has cells on the top row");
    assert!(
        first_tile.dy > 0,
        "the picture's own first row must have scrolled away, got {first_tile:?}"
    );
    let mut harness = image_pane_harness(grid, payloads, None);
    harness.snapshot("terminal_image_scrolled_off_the_top");
}

/// A payload Turn refuses, and what the user is told about it.
///
/// What to look for: no picture, the command and the prompt on consecutive lines with nothing
/// wedged between them, and `[turn: image not shown — payload over 8 MB]` across the bottom of
/// the pane in Turn's own strip, with a dismiss button at its right.
///
/// The two halves of that are both the point. A picture that silently did not appear is a bug
/// report nobody can write — so the sentence is shown. And the program's screen is the
/// program's: the sentence used to be written into it at the cursor, which cut a line of real
/// output in half and shifted every row below it, so the sentence is *not* shown there.
#[test]
fn a_refused_payload_tells_the_user_why_nothing_appeared() {
    // Nine mebibytes of base64, over the eight-mebibyte payload limit.
    let mut sequence = Vec::from(b"\x1b]1337;File=inline=1:".as_slice());
    sequence.extend(std::iter::repeat_n(b'A', 13 * 1024 * 1024));
    sequence.push(0x07);
    let (grid, payloads) = daemon_pane(
        8,
        62,
        &[
            b"~/turn on main $ imgcat enormous.png\r\n",
            &sequence,
            b"~/turn on main $ ",
        ],
    );
    assert!(payloads.is_empty(), "nothing was decoded");
    assert!(!grid.has_images(), "and nothing was placed");
    // The pane says so, beside the cells.
    let notice = turn_gui::terminal::notices::summary(&grid.notices);
    assert!(
        notice.contains("payload over 8 MB"),
        "the pane must say why: {:?}",
        grid.notices
    );
    // And the program's own output is exactly what the program wrote. This is the regression:
    // the notice landed at the cursor, between the command and the prompt.
    let text = grid.text();
    assert!(
        !text.contains("image not shown"),
        "Turn's sentence must not be in the program's screen: {text:?}"
    );
    assert!(
        text.starts_with("~/turn on main $ imgcat enormous.png\n~/turn on main $"),
        "the command and the prompt must be consecutive: {text:?}"
    );
    // Through `show_pane` rather than the painter, because the strip is chrome: a harness that
    // only painted cells would render a snapshot of a refusal with the refusal missing.
    let mut harness = interactive_pane(
        grid,
        PaneInteraction::default(),
        PaneOptions {
            now_ms: cursor_on(),
            ..Default::default()
        },
    );
    harness.snapshot("terminal_image_refused");
}

/// A selection dragged across a picture.
///
/// What to look for: the picture tinted with the selection colour over the columns inside the
/// selection and untinted outside it, and the words on either side highlighted as usual. A
/// selection that stopped at a picture would be the one thing on screen it did not touch.
#[test]
fn a_selection_over_an_image_highlights_it_like_anything_else() {
    let (grid, payloads) =
        daemon_pane(6, 44, &[b"pick: ", &kitty_rgba(160, 80, 16, 3), b" ok\r\n"]);
    let mut selection = Selection::new(
        turn_gui::terminal::selection::CellPos::new(0, 2),
        turn_gui::terminal::selection::SelectionKind::Linear,
    );
    selection.extend_to(turn_gui::terminal::selection::CellPos::new(0, 16));
    let mut harness = image_pane_harness(grid, payloads, Some(selection));
    harness.snapshot("terminal_image_selected");
}

/// A picture whose pixels have not arrived yet.
///
/// What to look for: a framed rectangle in the cells the picture will occupy — filled with
/// the raised panel colour and outlined on all four sides — with the text around it in place.
/// This is what a pane looks like for the frame or two between a screen arriving and the
/// payload being fetched, and it is deliberately visible.
#[test]
fn an_image_whose_pixels_have_not_arrived_shows_a_frame_rather_than_a_hole() {
    let (grid, _payloads) = daemon_pane(
        8,
        44,
        &[b"waiting: ", &kitty_rgba(160, 120, 12, 5), b" done\r\n"],
    );
    // Deliberately no payloads: the window has the screen and not the pixels.
    let mut harness = image_pane_harness(grid, Vec::new(), None);
    harness.snapshot("terminal_image_placeholder");
}

/// Where the glyph actually lands inside a row control.
///
/// The reported defect, and the one the column test above cannot see: the boxes were the right
/// size and in the right places, and the *icons inside them* were not centred. `egui`'s
/// `Button` takes its alignment from the layout of the `Ui` it is added to and offers no knob
/// of its own, so a button added to a plain region is laid out against that region's alignment
/// — left, in a top-down `Ui`. Three buttons of three different glyph widths then each sat at
/// a different offset inside its own box.
///
/// Measured from the pixels: the button is drawn alone in a known rectangle, and the ink is
/// weighed against the rectangle's centre. An eyeballed screenshot is how this survived twice.
#[test]
fn the_glyph_of_a_row_control_is_centred_in_its_box() {
    // Every glyph the rows use, because the failure was per-glyph: a wide icon and a narrow one
    // were wrong by different amounts, which is what made a row look ragged.
    for (name, glyph) in [
        ("close", turn_gui::icons::CLOSE),
        ("archive", turn_gui::icons::ARCHIVE),
        ("new-session", turn_gui::icons::FILE_PLUS),
        ("power", turn_gui::icons::POWER),
    ] {
        let offset = row_button_ink_offset(glyph);
        assert!(
            offset.x.abs() <= 1.0,
            "the {name} glyph is {:.2}pt off centre horizontally in its box",
            offset.x
        );
        assert!(
            offset.y.abs() <= 1.0,
            "the {name} glyph is {:.2}pt off centre vertically in its box",
            offset.y
        );
    }
}

/// Renders one row control alone and returns how far the ink's centre is from the box's, in
/// points. Positive x is to the right.
fn row_button_ink_offset(glyph: &'static str) -> egui::Vec2 {
    // A canvas larger than the button, so ink that escaped the box is still measured rather
    // than clipped away — a glyph drawn outside its own frame is exactly what is being checked.
    let pad = 6.0;
    let size = turn_gui::icons::ROW_SIZE + egui::Vec2::splat(pad * 2.0);
    let theme = Theme::dark();
    let mut harness = Harness::builder()
        .with_size(size)
        .with_pixels_per_point(4.0)
        .build_ui(move |ui| {
            theme.install(ui.ctx());
            let at = egui::Rect::from_min_size(egui::pos2(pad, pad), turn_gui::icons::ROW_SIZE);
            turn_gui::icons::row_button(ui, at, glyph, "measured", "measured", None, true);
        });
    harness.run();
    let image = harness.render().expect("the harness must render");
    let scale = image.width() as f32 / size.x;

    // The ink: anything brighter than the panel it is drawn on. The frame is not drawn for an
    // idle control, so what is left is the glyph.
    let (mut sum_x, mut sum_y, mut weight) = (0.0f64, 0.0f64, 0.0f64);
    for y in 0..image.height() {
        for x in 0..image.width() {
            let pixel = image.get_pixel(x, y);
            let luma =
                f64::from(pixel[0]) * 0.3 + f64::from(pixel[1]) * 0.6 + f64::from(pixel[2]) * 0.1;
            // Well above the panel and the border, well below the glyph's own grey.
            if luma > 60.0 {
                sum_x += f64::from(x) * luma;
                sum_y += f64::from(y) * luma;
                weight += luma;
            }
        }
    }
    assert!(weight > 0.0, "the control drew no glyph at all");
    let centre = egui::pos2(
        (sum_x / weight) as f32 / scale,
        (sum_y / weight) as f32 / scale,
    );
    let box_centre =
        egui::Rect::from_min_size(egui::pos2(pad, pad), turn_gui::icons::ROW_SIZE).center();
    centre - box_centre
}
