//! Reproducible performance envelope for the v0.1 desk.
//!
//! This is a production-shaped, deterministic workload rather than a microbenchmark:
//! thirty Workspaces, thirty Sessions, four relevant Processes per Session, stable
//! previews on every Process, simultaneous attention, and a live 40x120 terminal.
//! Wall-clock numbers are printed for profiling; generous latency ceilings catch
//! order-of-magnitude regressions while deterministic shape/capacity tests carry the
//! strict CI guarantees.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use turn_core::event::Confidence;
use turn_core::ids::{NodeId, PaneId, SessionId, WorkspaceId};
use turn_core::model::{
    ActivityPreview, Layout, NodeKind, Pane, PaneKind, PaneNodeBinding, PreviewSource, ProcessNode,
    Relation, Session, TreeVisibilityMode, Workspace,
};
use turn_core::state::{AwaitingReason, Lifecycle, Turn};
use turn_gui::keymap::{Keymap, Overrides, Platform};
use turn_gui::terminal::feed::{PaneFeed, MAX_HISTORY_ROWS};
use turn_gui::terminal::images::MAX_CACHE_BYTES;
use turn_gui::theme::Theme;
use turn_gui::transport::{
    INBOUND_MESSAGE_CAPACITY, OUTBOUND_INTENT_CAPACITY, PENDING_REQUEST_CAPACITY,
};
use turn_gui::view::{PaneContent, TurnView, ViewState};
use turn_proto::cells::Grid;
use turn_proto::{
    HierarchyKey, HierarchySnapshot, PtySize, Request, ScreenUpdate, SessionSummary,
    SessionTreeView, TerminalBytes, TreeNodeView, TreeSurfaceState, WorkspaceSummary,
    WorkspaceTreeView,
};
use turn_pty::buffer::DEFAULT_BYTE_CAPACITY;
use turn_pty::images::MAX_STORE_BYTES;
use turn_pty::JournalConfig;

const NOW: i64 = 1_700_000_000_000;
const WORKSPACES: usize = 30;
const SESSIONS: usize = 30;
const PROCESSES_PER_SESSION: usize = 4;
const PROCESS_COUNT: usize = SESSIONS * PROCESSES_PER_SESSION;
const OUTPUT_UPDATES: usize = 1_024;
const _: () = assert!(PROCESS_COUNT > 100);

struct SessionSurface {
    id: SessionId,
    pane_id: PaneId,
    layout: Layout,
    grid: Grid,
}

struct Envelope {
    hierarchy: HierarchySnapshot,
    sessions: Vec<SessionSurface>,
}

impl Envelope {
    fn build() -> Self {
        let mut branches = Vec::with_capacity(WORKSPACES);
        let mut surfaces = Vec::with_capacity(SESSIONS);
        let mut expanded = Vec::with_capacity(WORKSPACES + SESSIONS * 2);

        for index in 0..WORKSPACES {
            let mut workspace = Workspace::new(
                format!("workspace-{index:02}"),
                format!("/tmp/turn-envelope/workspace-{index:02}"),
                NOW,
            );
            workspace.id = WorkspaceId::from_stored(format!("ws_envelope_{index:02}"));

            let mut pane = Pane::new(PaneKind::Agent).with_command("claude");
            pane.id = PaneId::from_stored(format!("pane_envelope_{index:02}"));
            let layout = Layout::single(pane.clone());
            let mut session = Session::new(
                workspace.id.clone(),
                format!("session-{index:02}"),
                workspace.root.clone(),
                layout.clone(),
                NOW + index as i64,
            );
            session.id = SessionId::from_stored(format!("sess_envelope_{index:02}"));
            session.last_activity_ms = NOW + index as i64;

            let mut root =
                ProcessNode::agent(session.id.clone(), "claude", session.cwd.clone(), NOW);
            root.id = NodeId::from_stored(format!("agent_envelope_{index:02}"));
            root.lifecycle = Lifecycle::Alive;
            root.turn = Some(if index % 5 == 0 {
                Turn::AwaitingUser {
                    reason: AwaitingReason::Permission,
                }
            } else {
                Turn::Active
            });
            root.interaction_pending = index % 5 == 0;
            add_preview(&mut root, format!("Working on task {index:02}"));
            let root_id = session.tree.insert(root);

            for child in 0..(PROCESSES_PER_SESSION - 1) {
                let kind = match child {
                    0 => NodeKind::TestRunner,
                    1 => NodeKind::Build,
                    _ => NodeKind::Background,
                };
                let mut process = ProcessNode::process(
                    session.id.clone(),
                    kind,
                    format!("worker-{index:02}-{child}"),
                    session.cwd.clone(),
                    NOW,
                );
                process.id = NodeId::from_stored(format!("proc_envelope_{index:02}_{child}"));
                process.lifecycle = Lifecycle::Alive;
                process.link_to(root_id.clone(), Relation::Confirmed);
                add_preview(
                    &mut process,
                    format!("Progress {index:02}/{child}: stable preview"),
                );
                session.tree.insert(process);
            }

            let binding = PaneNodeBinding {
                pane_id: pane.id.clone(),
                session_id: session.id.clone(),
                node_id: root_id.clone(),
                temporary: false,
                surface_id: None,
                opened_ms: NOW,
            };
            let nodes =
                TreeNodeView::for_session_with_panes(&session, &[binding], &HashMap::new(), NOW);
            let badge_count = usize::from(index % 5 == 0);
            let summary = SessionSummary::from_session(&session, badge_count, false, NOW);
            let workspace_summary =
                WorkspaceSummary::from_workspace(&workspace, std::slice::from_ref(&summary));

            expanded.push(HierarchyKey::workspace(workspace.id.clone()));
            expanded.push(HierarchyKey::session(session.id.clone()));
            expanded.push(HierarchyKey::process(root_id));
            branches.push(WorkspaceTreeView {
                workspace: workspace_summary,
                checkouts: Vec::new(),
                write_lease: None,
                sessions: vec![SessionTreeView {
                    session: summary,
                    nodes,
                }],
            });
            surfaces.push(SessionSurface {
                id: session.id,
                pane_id: pane.id,
                layout,
                grid: busy_grid(index),
            });
        }

        let selected = HierarchyKey::session(surfaces[0].id.clone());
        Self {
            hierarchy: HierarchySnapshot {
                revision: 1,
                tree_state: TreeSurfaceState {
                    surface_id: "performance-envelope".into(),
                    selected: Some(selected),
                    expanded,
                    ..TreeSurfaceState::empty("performance-envelope")
                },
                workspaces: branches,
            },
            sessions: surfaces,
        }
    }
}

fn add_preview(node: &mut ProcessNode, text: String) {
    node.activity_preview = Some(ActivityPreview {
        node_id: node.id.clone(),
        raw_source_sequence: Some(42),
        normalized_text: text,
        source: PreviewSource::SemanticEvent,
        confidence: Confidence::Explicit,
        stable: true,
        contains_sensitive_data: false,
        redacted: false,
        updated_ms: NOW,
    });
}

fn busy_grid(seed: usize) -> Grid {
    let mut grid = Grid::blank(40, 120);
    write_row(
        &mut grid,
        0,
        &format!("session {seed:02}: compiling target, 1,024 updates queued"),
    );
    grid.cursor = Some((0, 48));
    grid
}

fn write_row(grid: &mut Grid, row: u16, text: &str) {
    for col in 0..grid.cols {
        if let Some(cell) = grid.cell_mut(row, col) {
            *cell = turn_proto::cells::Cell::blank();
        }
    }
    for (col, character) in text.chars().enumerate().take(grid.cols as usize) {
        if let Some(cell) = grid.cell_mut(row, col as u16) {
            cell.text = character.to_string();
        }
    }
}

struct Window {
    envelope: Envelope,
    active: usize,
    state: ViewState,
    theme: Theme,
    keymap: Keymap,
}

impl Window {
    fn new() -> Self {
        let envelope = Envelope::build();
        let mut state = ViewState::default();
        state.hierarchy = Some(envelope.hierarchy.clone());
        state.tree_visibility = TreeVisibilityMode::Expanded;
        state.selected_tree = envelope.hierarchy.tree_state.selected.clone();
        Self {
            envelope,
            active: 0,
            state,
            theme: Theme::dark(),
            keymap: Keymap::build(&Overrides::new(), Platform::MAC),
        }
    }
}

fn harness() -> Harness<'static, Window> {
    Harness::builder()
        .with_size(egui::vec2(1280.0, 760.0))
        .build_ui_state(
            |ui, window| {
                window.theme.install(ui.ctx());
                let active = &window.envelope.sessions[window.active];
                let panes = vec![PaneContent {
                    pane_id: active.pane_id.clone(),
                    title: "claude · performance envelope".into(),
                    grid: &active.grid,
                    focused: true,
                    scrolled: false,
                    history_complete: true,
                }];
                let view = TurnView {
                    workspaces: &[],
                    templates: &[],
                    sessions: Vec::new(),
                    selected: Some(active.id.clone()),
                    layout: Some(active.layout.clone()),
                    panes,
                    temporary_pane: None,
                    inspector: None,
                    restore: None,
                    recovery_lease: None,
                    unreachable_processes: 0,
                    relaunching: Vec::new(),
                    reclaiming_workspaces: Vec::new(),
                    reclaiming_write_access: false,
                    permission: None,
                    queue: Vec::new(),
                    connection: None,
                    include_archived: false,
                    notice: None,
                    write_conflict: None,
                    link_confirmation: None,
                    settings: None,
                    policy: None,
                    now_ms: NOW,
                };
                let _ = view.ui(ui, &window.theme, &window.keymap, &mut window.state);
            },
            Window::new(),
        )
}

fn percentile(samples: &mut [Duration], percentile: usize) -> Duration {
    samples.sort_unstable();
    let index = (samples.len() - 1) * percentile / 100;
    samples[index]
}

#[cfg(unix)]
fn process_usage() -> (Duration, u64) {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    // SAFETY: `getrusage` initialises the pointed-to `rusage` on success, and the
    // pointer remains valid for the duration of the call.
    let status = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
    assert_eq!(status, 0, "getrusage failed");
    // SAFETY: success above guarantees the value was initialised.
    let usage = unsafe { usage.assume_init() };
    let timeval = |value: libc::timeval| {
        Duration::from_secs(value.tv_sec.max(0) as u64)
            + Duration::from_micros(value.tv_usec.max(0) as u64)
    };
    let cpu = timeval(usage.ru_utime) + timeval(usage.ru_stime);
    #[cfg(target_os = "macos")]
    let rss_bytes = usage.ru_maxrss.max(0) as u64;
    #[cfg(not(target_os = "macos"))]
    let rss_bytes = (usage.ru_maxrss.max(0) as u64).saturating_mul(1024);
    (cpu, rss_bytes)
}

#[cfg(not(unix))]
fn process_usage() -> (Duration, u64) {
    (Duration::ZERO, 0)
}

#[test]
fn memory_disk_and_gui_queues_remain_bounded_at_the_envelope() {
    let terminal_processes = SESSIONS;
    let raw_memory_bytes = terminal_processes * DEFAULT_BYTE_CAPACITY;
    let image_memory_bytes = terminal_processes * MAX_STORE_BYTES + MAX_CACHE_BYTES;
    let journal = JournalConfig::default();
    let disk_bytes =
        terminal_processes * (journal.max_journal_bytes as usize + journal.max_checkpoint_bytes);

    assert_eq!(raw_memory_bytes, 60 * 1024 * 1024);
    assert_eq!(image_memory_bytes, 492 * 1024 * 1024);
    assert!(raw_memory_bytes + image_memory_bytes <= 600 * 1024 * 1024);
    assert_eq!(disk_bytes, 360 * 1024 * 1024);
    assert_eq!(MAX_HISTORY_ROWS, 5_000);
    assert_eq!(INBOUND_MESSAGE_CAPACITY, 64);
    assert_eq!(OUTBOUND_INTENT_CAPACITY, 256);
    assert_eq!(PENDING_REQUEST_CAPACITY, 512);

    eprintln!(
        "turn-performance terminal_raw_memory_cap_mib={} terminal_image_memory_cap_mib={} terminal_disk_cap_mib={} inbound_messages={} outbound_intents={} pending_requests={} semantic_processes={}",
        raw_memory_bytes / (1024 * 1024),
        image_memory_bytes / (1024 * 1024),
        disk_bytes / (1024 * 1024),
        INBOUND_MESSAGE_CAPACITY,
        OUTBOUND_INTENT_CAPACITY,
        PENDING_REQUEST_CAPACITY,
        PROCESS_COUNT - terminal_processes
    );
}

#[test]
fn thirty_sessions_and_one_hundred_processes_switch_within_the_frame_budget() {
    let mut harness = harness();
    let initial_frames = harness.run();
    assert!(
        initial_frames <= 6,
        "initial desk needed {initial_frames} frames"
    );

    let hierarchy = &harness.state().envelope.hierarchy;
    assert_eq!(hierarchy.workspaces.len(), WORKSPACES);
    assert_eq!(
        hierarchy
            .workspaces
            .iter()
            .map(|workspace| workspace.sessions.len())
            .sum::<usize>(),
        SESSIONS
    );
    assert_eq!(
        hierarchy
            .workspaces
            .iter()
            .flat_map(|workspace| &workspace.sessions)
            .map(|session| session.nodes.len())
            .sum::<usize>(),
        PROCESS_COUNT
    );

    let retained_projection_bytes = serde_json::to_vec(hierarchy).unwrap().len();
    assert!(
        retained_projection_bytes < 1_500_000,
        "the compact 30/120 hierarchy grew to {retained_projection_bytes} bytes"
    );

    let (cpu_before, _) = process_usage();
    let mut switch_times = Vec::with_capacity(SESSIONS);
    let mut worst_frames = 0;
    for index in 0..SESSIONS {
        let selected = harness.state().envelope.sessions[index].id.clone();
        harness.state_mut().active = index;
        harness.state_mut().state.selected_tree = Some(HierarchyKey::session(selected));
        let started = Instant::now();
        let frames = harness.run();
        switch_times.push(started.elapsed());
        worst_frames = worst_frames.max(frames);
    }
    let (cpu_after, peak_rss_bytes) = process_usage();
    let switch_cpu = cpu_after.saturating_sub(cpu_before);
    let p95 = percentile(&mut switch_times, 95);
    eprintln!(
        "turn-performance frame_switch_p95_us={} switch_cpu_ms={} worst_settle_frames={} hierarchy_bytes={} peak_rss_mib={} workspaces={} sessions={} processes={}",
        p95.as_micros(),
        switch_cpu.as_millis(),
        worst_frames,
        retained_projection_bytes,
        peak_rss_bytes / (1024 * 1024),
        WORKSPACES,
        SESSIONS,
        PROCESS_COUNT
    );
    assert!(
        worst_frames <= 6,
        "a Session switch needed {worst_frames} frames"
    );
    assert!(
        p95 < Duration::from_millis(50),
        "Session switch p95 was {p95:?}; the debug CI budget is 50ms"
    );
    assert!(
        switch_cpu < Duration::from_secs(4),
        "thirty Session switches consumed {switch_cpu:?} of process CPU"
    );
}

#[test]
fn terminal_input_enqueue_stays_non_blocking() {
    let (sender, mut receiver) = tokio::sync::mpsc::channel::<Request>(OUTBOUND_INTENT_CAPACITY);
    let session_id = SessionId::from_stored("sess_input_envelope");
    let node_id = NodeId::from_stored("node_input_envelope");
    let mut samples = Vec::with_capacity(OUTPUT_UPDATES);
    for _ in 0..OUTPUT_UPDATES {
        let request = Request::WritePty {
            session_id: session_id.clone(),
            node_id: node_id.clone(),
            data: TerminalBytes::new(b"x".to_vec()),
        };
        let started = Instant::now();
        sender.try_send(request).unwrap();
        samples.push(started.elapsed());
        receiver.try_recv().unwrap();
    }
    let p95 = percentile(&mut samples, 95);
    eprintln!(
        "turn-performance input_enqueue_p95_us={} samples={} queue_capacity={}",
        p95.as_micros(),
        OUTPUT_UPDATES,
        OUTBOUND_INTENT_CAPACITY
    );
    assert!(
        p95 < Duration::from_millis(1),
        "terminal input enqueue p95 was {p95:?}"
    );
}

#[test]
fn noisy_output_keeps_updates_and_another_terminal_responsive() {
    let size = PtySize::new(40, 120);
    let mut source = Grid::blank(size.rows, size.cols);
    let mut updates = Vec::with_capacity(OUTPUT_UPDATES);
    for sequence in 0..OUTPUT_UPDATES {
        let mut next = source.clone();
        write_row(
            &mut next,
            (sequence % usize::from(size.rows)) as u16,
            &format!("build output {sequence:04}: {}", "x".repeat(80)),
        );
        updates.push(ScreenUpdate::between(&source, &next));
        source = next;
    }

    let mut noisy = PaneFeed::blank(size);
    let mut quiet = PaneFeed::blank(size);
    let mut apply_times = Vec::with_capacity(OUTPUT_UPDATES);
    let (cpu_before, _) = process_usage();
    let started = Instant::now();
    for (sequence, update) in updates.iter().enumerate() {
        let apply_started = Instant::now();
        noisy.apply(sequence as u64, update).unwrap();
        apply_times.push(apply_started.elapsed());
    }
    let total = started.elapsed();
    let (cpu_after, _) = process_usage();
    let output_cpu = cpu_after.saturating_sub(cpu_before);

    let mut quiet_grid = Grid::blank(size.rows, size.cols);
    write_row(&mut quiet_grid, 0, "other terminal still responds");
    let quiet_update = ScreenUpdate::between(&Grid::blank(size.rows, size.cols), &quiet_grid);
    let quiet_started = Instant::now();
    quiet.apply(0, &quiet_update).unwrap();
    let quiet_latency = quiet_started.elapsed();
    let p95 = percentile(&mut apply_times, 95);

    eprintln!(
        "turn-performance output_apply_p95_us={} output_total_ms={} output_cpu_ms={} quiet_terminal_us={} updates={} history_cap={}",
        p95.as_micros(),
        total.as_millis(),
        output_cpu.as_millis(),
        quiet_latency.as_micros(),
        OUTPUT_UPDATES,
        MAX_HISTORY_ROWS
    );
    assert!(
        p95 < Duration::from_millis(5),
        "screen update p95 was {p95:?}; the debug CI budget is 5ms"
    );
    assert!(
        total < Duration::from_secs(3),
        "{OUTPUT_UPDATES} output updates took {total:?}"
    );
    assert!(
        output_cpu < Duration::from_secs(3),
        "{OUTPUT_UPDATES} output updates consumed {output_cpu:?} of process CPU"
    );
    assert!(
        quiet_latency < Duration::from_millis(20),
        "a different terminal update took {quiet_latency:?}"
    );
    assert_eq!(quiet.grid().row_text(0), "other terminal still responds");
}
