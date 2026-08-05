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
    ActivityPreview, AgentName, Direction, Layout, LeaseState, NodeKind, Pane, PaneKind,
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
use turn_gui::terminal::PaneAction;
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
    restore: Option<SessionRestoreView>,
    recovery_lease: Option<WorkspaceWriteLease>,
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
            include_archived: false,
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

#[test]
fn ending_a_session_requires_confirmation_and_requests_process_termination() {
    let fixture = busy_desk();
    let session_id = fixture.selected.clone().expect("selected Session");
    let mut h = harness(fixture);
    h.state_mut().state.lifecycle_confirmation = Some(LifecycleConfirmation::EndSession {
        session_id: session_id.clone(),
        name: "Fix climbing bugs".into(),
        running_count: 6,
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

    h.query_by_label("+ Session")
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

/// The size in cells a pane reports has to match what is painted, or every wrapped line
/// ends a column off the edge. Checked here rather than only in the geometry unit test,
/// because this is the composition that decides it.
#[test]
fn a_panes_reported_size_matches_the_cells_it_can_paint() {
    let theme = Theme::dark();
    let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(8.0 * 100.0, 17.0 * 30.0));
    let size = turn_gui::panes::size_in_cells(rect, theme.cell_size);
    assert_eq!(size, PtySize::new(30, 100));

    // And the pane paints exactly the rows it claims to have.
    let grid = Grid::blank(size.rows, size.cols);
    let rows = turn_gui::terminal::visible_rows(&grid, rect.min, theme.cell_size, rect);
    assert_eq!(rows, 0..30);
}
