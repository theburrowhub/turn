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
//! * `every_session_row_is_reachable_by_its_accessible_name` drives the same AccessKit
//!   tree a screen reader would read. A GPU-drawn terminal has no DOM, so this is the one
//!   requirement no snapshot can cover.

use std::collections::BTreeMap;

use egui_kittest::kittest::{NodeT, Queryable};
use egui_kittest::Harness;
use turn_core::event::Risk;
use turn_core::ids::{AttentionId, PaneId, SessionId};
use turn_core::model::{Direction, Layout, Pane, PaneKind};
use turn_core::state::{AwaitingReason, DisplayState};
use turn_proto::cells::{Cell, CellAttrs, Grid, Rgb};
use turn_proto::{PtySize, Welcome};

use turn_gui::keymap::{Keymap, Overrides, Platform};
use turn_gui::theme::Theme;
use turn_gui::transport::{ConnectionState, DaemonIdentity};
use turn_gui::view::{
    Overview, PaneContent, PendingPermission, QueueItem, SessionRow, TurnView, ViewState,
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
    overview_open: bool,
    /// One screen per session, for the overview's thumbnails.
    overview_screens: Vec<(SessionId, Grid)>,
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
        TurnView {
            sessions: self.sessions.clone(),
            selected: self.selected.clone(),
            layout: self.layout.clone(),
            panes,
            temporary_pane: None,
            overview_screens: self
                .overview_screens
                .iter()
                .map(|(session, grid)| (session.clone(), grid))
                .collect(),
            permission: self.permission.clone(),
            queue: self.queue.clone(),
            connection: self.connection.clone(),
            notice: self.notice.clone(),
            write_conflict: None,
            overview: Overview {
                open: self.overview_open,
            },
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
}

fn window(fixture: Fixture) -> Window {
    Window {
        fixture,
        state: ViewState::default(),
        theme: Theme::dark(),
        keymap: Keymap::build(&Overrides::new(), Platform::MAC),
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
                } = window;
                theme.install(ui.ctx());
                fixture.view().ui(ui, theme, keymap, state);
            },
            window(fixture),
        )
}

fn connected() -> ConnectionState {
    DaemonIdentity::new().observe(&Welcome::new(1, "0.1.0", 51234, T0))
}

fn session(name: &str, state: DisplayState) -> SessionRow {
    SessionRow {
        id: SessionId::from_stored(format!(
            "sess_{:0>11}",
            name.replace(' ', "").to_lowercase()
        )),
        name: name.into(),
        state,
        state_label: state.label().to_string(),
        detail: String::new(),
        badge: 0,
        provisional: false,
        depth: 0,
        muted: false,
    }
}

fn with_detail(mut row: SessionRow, detail: &str, badge: usize) -> SessionRow {
    row.detail = detail.into();
    row.badge = badge;
    row
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

/// A session's screen for the overview: each one shaped differently, because a thumbnail
/// earns its place only if a build, a diff and a prompt are tellable apart at that size.
fn overview_screen(index: usize) -> Grid {
    match index % 4 {
        0 => {
            let mut grid = Grid::blank(30, 100);
            for row in 0..28u16 {
                let line = format!("   Compiling crate-number-{row} v0.1.0 (/Users/x/turn)");
                for (col, ch) in line.chars().enumerate().take(100) {
                    if let Some(cell) = grid.cell_mut(row, col as u16) {
                        cell.text = ch.to_string();
                        if row % 7 == 0 {
                            cell.fg = Some(Rgb::new(0xe0, 0x5a, 0x5a));
                        }
                    }
                }
            }
            grid
        }
        1 => screen(&["~/turn on main $ "], 30, 100),
        2 => {
            let mut grid = Grid::blank(30, 100);
            for row in 0..24u16 {
                let line = if row % 3 == 0 {
                    "+    let grid = from_screen(parser.screen());"
                } else {
                    "-    let grid = parser.screen();"
                };
                for (col, ch) in line.chars().enumerate().take(100) {
                    if let Some(cell) = grid.cell_mut(row, col as u16) {
                        cell.text = ch.to_string();
                        cell.fg = Some(if row % 3 == 0 {
                            Rgb::new(0x6e, 0xb0, 0x7e)
                        } else {
                            Rgb::new(0xe0, 0x5a, 0x5a)
                        });
                    }
                }
            }
            grid
        }
        _ => {
            let mut grid = Grid::blank(30, 100);
            for row in 0..30u16 {
                for col in 0..100u16 {
                    if let Some(cell) = grid.cell_mut(row, col) {
                        cell.bg = Some(Rgb::new(0x1a, 0x1e, 0x24));
                        if row % 4 == 1 && col < 60 {
                            cell.text = "─".into();
                        }
                    }
                }
            }
            grid.alternate_screen = true;
            grid
        }
    }
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
        Pane::new(PaneKind::TestOutput).with_title("cargo test"),
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

/// A window in the state the product exists for: one session blocked on a permission,
/// others working, one failed, with three panes on screen.
fn busy_desk() -> Fixture {
    let (layout, panes) = three_pane_layout();
    let mut grids = BTreeMap::new();
    let mut titles = BTreeMap::new();
    grids.insert(panes[0].clone(), agent_screen());
    titles.insert(panes[0].clone(), "claude · agent · sonnet".to_string());
    grids.insert(
        panes[1].clone(),
        screen(
            &[
                "running 128 tests",
                "test physics::climbing::grips ... ok",
                "test physics::climbing::ledges ... FAILED",
            ],
            20,
            46,
        ),
    );
    titles.insert(panes[1].clone(), "cargo test -p physics".to_string());
    grids.insert(
        panes[2].clone(),
        screen(&["~/space-troopers on climb $ "], 20, 46),
    );
    titles.insert(panes[2].clone(), "zsh".to_string());

    let mut reviewer = session("Reviewer", DisplayState::Running);
    reviewer.depth = 1;
    reviewer.detail = "subagent".into();
    let mut inferred = session("npm run dev", DisplayState::Running);
    inferred.depth = 1;
    inferred.provisional = true;
    inferred.detail = "inferred".into();

    Fixture {
        sessions: vec![
            with_detail(
                session("Fix climbing bugs", DisplayState::NeedsPermission),
                "1 running · 3 panes",
                1,
            ),
            reviewer,
            inferred,
            with_detail(
                session("Improve targeting", DisplayState::CompletedTurn),
                "2 running",
                0,
            ),
            with_detail(
                session("Dockerize tests", DisplayState::Failed),
                "exit 1",
                0,
            ),
            with_detail(
                session("Draft release notes", DisplayState::Idle),
                "12m idle",
                0,
            ),
            with_detail(
                session("Port the supervisor", DisplayState::Running),
                "3 panes",
                0,
            ),
        ],
        selected: Some(SessionId::from_stored("sess_fixclimbingbugs")),
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
        ..Fixture::default()
    }
}

#[test]
fn a_busy_desk_with_a_pending_permission() {
    let mut h = harness(busy_desk());
    h.run();
    h.snapshot("busy_desk");
}

#[test]
fn an_empty_window_says_so_rather_than_looking_broken() {
    let mut h = harness(Fixture {
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
    fixture.queue = vec![
        queue_item("npm run dev", AwaitingReason::Input, true, true),
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

/// Thirty sessions is the desk the product is designed for, and the case where a sidebar
/// stops being legible. Worth an image.
#[test]
fn thirty_sessions_stay_legible_in_the_sidebar() {
    let states = [
        DisplayState::Running,
        DisplayState::NeedsPermission,
        DisplayState::CompletedTurn,
        DisplayState::Failed,
        DisplayState::Idle,
        DisplayState::WaitingForUser,
        DisplayState::Starting,
        DisplayState::Stopped,
        DisplayState::AskingQuestion,
        DisplayState::CompletedTask,
    ];
    let mut fixture = busy_desk();
    fixture.sessions = (0..30)
        .map(|index| {
            let state = states[index % states.len()];
            let mut row = with_detail(
                session(&format!("Task {index:02} on a longish branch name"), state),
                "2 running · 4 panes",
                if state.demands_user() { index % 4 } else { 0 },
            );
            row.muted = index % 7 == 0;
            row.provisional = index % 5 == 0;
            row
        })
        .collect();
    fixture.selected = fixture.sessions.get(1).map(|row| row.id.clone());
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

/// The overview: thirty postage stamps, which is what makes "what is my desk doing" a
/// glance rather than a scroll.
#[test]
fn the_session_overview_shows_a_picture_of_every_session() {
    let mut fixture = busy_desk();
    fixture.overview_open = true;
    fixture.sessions = (0..12)
        .map(|index| {
            with_detail(
                session(
                    &format!("Session {index:02}"),
                    if index % 3 == 0 {
                        DisplayState::NeedsPermission
                    } else {
                        DisplayState::Running
                    },
                ),
                "2 running",
                index % 3,
            )
        })
        .collect();
    fixture.selected = fixture.sessions.first().map(|row| row.id.clone());
    // A screen per session, so the overview shows what it is for rather than a grid of
    // empty tiles: a build scrolling, a diff, a prompt, a full-screen tool.
    fixture.overview_screens = fixture
        .sessions
        .iter()
        .enumerate()
        .map(|(index, row)| (row.id.clone(), overview_screen(index)))
        .collect();
    let mut h = harness(fixture);
    h.run();
    h.snapshot("overview");
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
/// The rows are queried by **role and label** rather than by label alone. That is the fix
/// for what made an earlier version of this test fail: a session's name legitimately
/// appears in more than one place — the sidebar row and the permission banner both name
/// "Fix climbing bugs" — so a query by label was ambiguous and panicked. The row carries
/// `Role::ListItem`, which is both unambiguous and the thing a screen reader needs in
/// order to announce it as one of a list.
#[test]
fn every_session_row_is_reachable_by_its_accessible_name() {
    let mut h = harness(busy_desk());
    // Two frames: egui builds the AccessKit tree from the previous frame's widgets, so a
    // single pass has nothing in it yet.
    h.run();
    h.run();

    let rows: Vec<String> = h
        .query_all_by_role(egui::accesskit::Role::ListItem)
        .filter_map(|node| node.accesskit_node().label())
        .collect();
    assert!(
        rows.len() >= 7,
        "every session must be a row in the tree; found {rows:?}"
    );

    // Each row's accessible name carries its state in words, not only in colour, so
    // querying for name-and-state together proves both reach the tree.
    for (name, state_word) in [
        ("Fix climbing bugs", "PERMISSION"),
        ("Dockerize tests", "failed"),
        ("Improve targeting", "turn done"),
        ("Draft release notes", "idle"),
    ] {
        let expected = format!("{name} — {state_word}");
        assert!(
            rows.iter().any(|label| label.contains(&expected)),
            "{name}'s accessible name must state its condition ({state_word}), or a \
             screen-reader user cannot tell what it is doing. Found {rows:?}"
        );
    }

    // A guess must be audible as a guess.
    assert!(
        rows.iter()
            .any(|label| label.contains("npm run dev") && label.contains("(inferred)")),
        "an inferred state must say so in words: {rows:?}"
    );

    // And the selected row is marked as selected, which is how a screen reader says
    // "this is the one you are looking at".
    let selected: Vec<String> = h
        .query_all_by_role(egui::accesskit::Role::ListItem)
        .filter(|node| node.accesskit_node().is_selected() == Some(true))
        .filter_map(|node| node.accesskit_node().label())
        .collect();
    assert!(
        selected
            .iter()
            .any(|label| label.contains("Fix climbing bugs")),
        "the selected session must be marked selected in the tree; found {selected:?}"
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
fn an_empty_window_has_a_list_with_nothing_in_it_rather_than_a_missing_list() {
    let mut h = harness(Fixture {
        connection: Some(connected()),
        ..Fixture::default()
    });
    h.run();
    h.run();
    assert_eq!(
        h.query_all_by_role(egui::accesskit::Role::ListItem).count(),
        0
    );
    let list = h
        .query_by_role(egui::accesskit::Role::List)
        .expect("the list exists even when it is empty");
    assert!(
        list.accesskit_node()
            .label()
            .is_some_and(|label| label.contains('0')),
        "the list must say how many rows it has: {:?}",
        list.accesskit_node().label()
    );
}

/// The queue's next item has to be identifiable from the tree, because "press the shortcut
/// and land on the right thing" is the product's core promise and a screen-reader user
/// gets no visual rule to look at.
#[test]
fn the_next_demand_in_the_queue_is_marked_as_next() {
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
        .query_all_by_role(egui::accesskit::Role::ListItem)
        .filter_map(|node| node.accesskit_node().label())
        .collect();
    assert!(
        rows.iter()
            .any(|label| label.starts_with("next: Fix climbing bugs")),
        "the first actionable demand is the next one, not the first row: {rows:?}"
    );
    assert!(
        rows.iter()
            .any(|label| label.contains("Draft release notes") && label.contains("snoozed")),
        "a snoozed demand is still listed, and says so: {rows:?}"
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
