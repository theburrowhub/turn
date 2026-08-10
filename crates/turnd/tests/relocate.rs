//! Moving a pane, through the socket, over a session with real processes in it.
//!
//! The product complaint these tests answer: a pane could be exchanged with another one
//! but never actually *moved*, so the shape of a layout was fixed when the session was
//! created. What matters here is not that a request is accepted — the domain tests cover
//! the tree surgery — but that a relocation crosses the whole system without touching
//! anything it must not touch: the same processes keep running under the same pids, every
//! attached client is told, and the new arrangement is still there after a restart.

mod common;

use common::*;
use turn_core::ids::PaneId;
use turn_core::model::{Direction, DropZone, LayoutNode, PaneKind};
use turn_proto::{ErrorCode, NewPane, Request, ServerEvent, SessionDetails};

/// A layout's shape written out by pane title, so an assertion reads like the
/// arrangement a user would be looking at.
///
/// `|` separates columns and `/` separates rows, and every split is bracketed — which
/// is what makes the difference between a real orientation change and a reordering of
/// the same split visible in the assertion itself.
fn shape(node: &LayoutNode) -> String {
    match node {
        LayoutNode::Leaf(pane) => pane
            .title
            .clone()
            .unwrap_or_else(|| pane.id.as_str().to_string()),
        LayoutNode::Split(split) => {
            let separator = match split.direction {
                Direction::Horizontal => " | ",
                Direction::Vertical => " / ",
            };
            let children: Vec<String> = split
                .children
                .iter()
                .map(|child| shape(&child.node))
                .collect();
            format!("({})", children.join(separator))
        }
    }
}

/// The pid of the process behind a pane, through the pane's node binding.
fn pid_behind(details: &SessionDetails, pane_id: &PaneId) -> u32 {
    let node_id = details
        .layout
        .get(pane_id)
        .unwrap_or_else(|| panic!("{pane_id} is not in this layout"))
        .node_id
        .clone()
        .unwrap_or_else(|| panic!("{pane_id} has no process behind it"));
    row(details, &node_id)
        .pid
        .unwrap_or_else(|| panic!("{node_id} is not running"))
}

fn pane_named(details: &SessionDetails, title: &str) -> PaneId {
    details
        .layout
        .panes()
        .into_iter()
        .find(|pane| pane.title.as_deref() == Some(title))
        .unwrap_or_else(|| panic!("no pane titled {title:?}"))
        .id
        .clone()
}

/// A session of three columns, each running a real process.
async fn three_columns(daemon: &TestDaemon, ui: &mut Client) -> SessionDetails {
    let root = daemon
        .data_dir()
        .join("workspaces")
        .join(turn_core::ids::WorkspaceId::new().as_str());
    std::fs::create_dir_all(&root).expect("a workspace root");
    let workspace = workspace_of(
        ui.ask(Request::CreateWorkspace {
            name: "relocation".to_string(),
            root: root.display().to_string(),
        })
        .await,
    );
    let session = session_of(
        ui.ask(Request::CreateSession {
            workspace_id: workspace.id,
            name: "three columns".to_string(),
            cwd: None,
            panes: Some(vec![pane("left"), pane("middle"), pane("right")]),
            note: None,
            tags: Vec::new(),
        })
        .await,
    );
    details_of(
        ui.ask(Request::GetSession {
            session_id: session.id,
        })
        .await,
    )
}

/// A pane holding a process that stays up until it is told to stop.
fn pane(title: &str) -> NewPane {
    let mut spec = NewPane::new(PaneKind::Terminal).with_command("cat");
    spec.title = Some(title.to_string());
    spec
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_relocated_pane_changes_the_layouts_orientation_and_survives_a_restart() {
    let daemon = TestDaemon::start_plain().await;
    let mut ui = daemon.connect().await;
    let mut observer = daemon.connect().await;
    let before = three_columns(&daemon, &mut ui).await;
    let session_id = before.summary.id.clone();

    assert_eq!(
        shape(&before.layout.root),
        "(left | middle | right)",
        "three panes start as one flat row"
    );
    let left = pane_named(&before, "left");
    let middle = pane_named(&before, "middle");
    let right = pane_named(&before, "right");
    let pids = [
        pid_behind(&before, &left),
        pid_behind(&before, &middle),
        pid_behind(&before, &right),
    ];
    assert!(
        pids.iter().all(|pid| pid_is_alive(*pid)),
        "all three panes must really be running something"
    );
    let active_before = before.layout.active.clone();
    assert!(active_before.is_some());

    // Right goes under Left. This is the move the old protocol could not express: the
    // two panes end up in a column, which no exchange of positions can produce.
    let after = layout_of(
        ui.ask(Request::RelocatePane {
            session_id: session_id.clone(),
            moved: right.clone(),
            target: left.clone(),
            zone: DropZone::Below,
        })
        .await,
    );
    assert_eq!(
        shape(&after.root),
        "((left / right) | middle)",
        "the moved pane became a row-sibling of its target"
    );
    assert!(after.sizes_are_normalised());
    assert_eq!(after.pane_count(), 3);
    assert_eq!(
        after.active, active_before,
        "moving a pane must not change which pane has focus"
    );

    // The other window is told, without having asked, and is told the same tree.
    let pushed = observer
        .wait_for("a layout change", |event| match event {
            ServerEvent::LayoutChanged {
                session_id: id,
                layout,
            } if id == &session_id => Some(layout.clone()),
            _ => None,
        })
        .await;
    assert_eq!(shape(&pushed.root), shape(&after.root));

    // Nothing was started or stopped: a pane moving is a view change, and the process
    // behind it never learns it happened.
    let details = details_of(
        ui.ask(Request::GetSession {
            session_id: session_id.clone(),
        })
        .await,
    );
    assert_eq!(
        [
            pid_behind(&details, &left),
            pid_behind(&details, &middle),
            pid_behind(&details, &right),
        ],
        pids,
        "a relocation must not restart anything"
    );
    assert!(pids.iter().all(|pid| pid_is_alive(*pid)));
    assert_eq!(
        details.tree.len(),
        before.tree.len(),
        "a relocation must not add or remove a process"
    );

    drop(ui);
    drop(observer);
    let daemon = daemon.restart().await;
    let mut ui = daemon.connect().await;

    let restored = details_of(
        ui.ask(Request::GetSession {
            session_id: session_id.clone(),
        })
        .await,
    );
    assert_eq!(
        shape(&restored.layout.root),
        "((left / right) | middle)",
        "the arrangement the user made was persisted, not the one the session was created with"
    );
    assert!(restored.layout.sizes_are_normalised());
    // The same panes, not new ones: pane identity is what client attachments and pty
    // ownership are keyed by, so a restore that minted fresh ids would lose the
    // relocation's whole point.
    assert_eq!(pane_named(&restored, "left"), left);
    assert_eq!(pane_named(&restored, "middle"), middle);
    assert_eq!(pane_named(&restored, "right"), right);

    // And the restored arrangement is still something that can be rearranged.
    let again = layout_of(
        ui.ask(Request::RelocatePane {
            session_id: session_id.clone(),
            moved: middle.clone(),
            target: right.clone(),
            zone: DropZone::Right,
        })
        .await,
    );
    assert_eq!(shape(&again.root), "(left / (right | middle))");
    assert!(again.sizes_are_normalised());

    daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_relocation_that_cannot_mean_anything_is_refused_and_moves_nothing() {
    let daemon = TestDaemon::start_plain().await;
    let mut ui = daemon.connect().await;
    let session = three_columns(&daemon, &mut ui).await;
    let session_id = session.summary.id.clone();
    let left = pane_named(&session, "left");
    let ghost = PaneId::from_stored("pane_notthere01");

    let error = ui
        .try_ask(Request::RelocatePane {
            session_id: session_id.clone(),
            moved: left.clone(),
            target: ghost.clone(),
            zone: DropZone::Right,
        })
        .await
        .expect_err("a pane cannot be moved next to one that does not exist");
    assert_eq!(error.code, ErrorCode::NotFound);

    let error = ui
        .try_ask(Request::RelocatePane {
            session_id: session_id.clone(),
            moved: ghost,
            target: left.clone(),
            zone: DropZone::Right,
        })
        .await
        .expect_err("a pane that does not exist cannot be moved");
    assert_eq!(error.code, ErrorCode::NotFound);

    let error = ui
        .try_ask(Request::RelocatePane {
            session_id: session_id.clone(),
            moved: left.clone(),
            target: left.clone(),
            zone: DropZone::Below,
        })
        .await
        .expect_err("a pane has no position relative to itself");
    assert_eq!(error.code, ErrorCode::Conflict);

    let unchanged = details_of(
        ui.ask(Request::GetSession {
            session_id: session_id.clone(),
        })
        .await,
    );
    assert_eq!(shape(&unchanged.layout.root), "(left | middle | right)");
    assert_eq!(unchanged.layout, session.layout);

    daemon.shutdown().await;
}

/// The older `swap_panes` still works, and it works by being a relocation, so the two
/// spellings cannot come to mean different things.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn swapping_panes_is_served_as_a_centre_relocation() {
    let daemon = TestDaemon::start_plain().await;
    let mut ui = daemon.connect().await;
    let session = three_columns(&daemon, &mut ui).await;
    let session_id = session.summary.id.clone();
    let left = pane_named(&session, "left");
    let right = pane_named(&session, "right");

    let swapped = layout_of(
        ui.ask(Request::SwapPanes {
            session_id: session_id.clone(),
            a: left.clone(),
            b: right.clone(),
        })
        .await,
    );
    assert_eq!(
        shape(&swapped.root),
        "(right | middle | left)",
        "an exchange in place leaves the geometry alone"
    );

    let relocated = layout_of(
        ui.ask(Request::RelocatePane {
            session_id: session_id.clone(),
            moved: right.clone(),
            target: left.clone(),
            zone: DropZone::Centre,
        })
        .await,
    );
    assert_eq!(
        shape(&relocated.root),
        "(left | middle | right)",
        "the centre zone undid the swap, because it is the same operation"
    );

    let error = ui
        .try_ask(Request::SwapPanes {
            session_id,
            a: left.clone(),
            b: left,
        })
        .await
        .expect_err("a pane cannot be exchanged with itself");
    assert_eq!(error.code, ErrorCode::Conflict);

    daemon.shutdown().await;
}
