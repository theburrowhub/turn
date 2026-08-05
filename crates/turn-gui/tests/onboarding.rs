//! First-run onboarding over the real application boundary.
//!
//! This deliberately stops one layer short of pixel-driven input: snapshots and app
//! tests own the `Cmd+N` binding and sheet widgets, while this test takes the exact
//! typed action produced by that sheet through `Desk -> DaemonLink -> Unix socket ->
//! turnd -> SQLite`, then feeds every answer back into the real Desk. Keeping the
//! daemon and project roots under one TempDir makes it impossible to mutate a user's
//! Turn database or checkout.

#![cfg(unix)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use turn_core::ids::{SessionId, WorkspaceId};
use turn_core::model::{LeaseState, PaneKind, SessionMode};
use turn_gui::desk::{Desk, Reaction};
use turn_gui::transport::{ConnectionState, DaemonLink};
use turn_gui::view::{TurnView, ViewAction};
use turn_proto::HierarchyKey;

const WAIT: Duration = Duration::from_secs(10);

/// A headless window: the same Desk and transport as `TurnApp`, with rendering omitted.
struct GuiHarness {
    desk: Desk,
    link: DaemonLink,
    observed: Vec<Reaction>,
}

impl GuiHarness {
    fn connect(socket: std::path::PathBuf) -> Self {
        Self {
            desk: Desk::new(),
            link: DaemonLink::spawn(socket, env!("CARGO_PKG_VERSION"), Arc::new(|| {})),
            observed: Vec::new(),
        }
    }

    fn view(&self) -> TurnView<'_> {
        self.desk.view(turn_core::now_ms())
    }

    /// Performs the I/O half of `TurnApp::perform`; non-I/O reactions are retained so
    /// the test can prove the creation lifecycle reached the window.
    fn route(&mut self, reactions: Vec<Reaction>) {
        for reaction in reactions {
            match reaction {
                Reaction::Send { ask, request } => self.link.send(ask, request),
                other => self.observed.push(other),
            }
        }
    }

    fn pump(&mut self) {
        for inbound in self.link.drain() {
            let reactions = self.desk.apply_inbound(inbound, turn_core::now_ms());
            self.route(reactions);
        }
    }

    fn act(&mut self, action: ViewAction) {
        let reactions = self.desk.apply_view_action(action, turn_core::now_ms());
        self.route(reactions);
    }

    async fn wait_for<T>(&mut self, description: &str, find: impl Fn(&Self) -> Option<T>) -> T {
        let deadline = Instant::now() + WAIT;
        loop {
            self.pump();
            if let Some(value) = find(self) {
                return value;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {description}; connection={:?}, workspaces={}, sessions={}, selected={:?}, notice={:?}",
                self.desk.connection(),
                self.view().workspaces.len(),
                self.desk.sessions().len(),
                self.desk.selected(),
                self.view().notice,
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn wait_until_ready(&mut self) {
        self.wait_for("the initial hierarchy and templates", |gui| {
            (matches!(gui.desk.connection(), ConnectionState::Connected { .. })
                && gui.desk.hierarchy().is_some()
                && !gui.view().templates.is_empty())
            .then_some(())
        })
        .await;
    }
}

fn created_workspace(gui: &GuiHarness) -> Option<WorkspaceId> {
    gui.observed.iter().find_map(|reaction| match reaction {
        Reaction::WorkspaceCreated {
            workspace_id,
            continue_to_session: true,
        } => Some(workspace_id.clone()),
        _ => None,
    })
}

fn created_session(gui: &GuiHarness) -> Option<SessionId> {
    gui.observed.iter().find_map(|reaction| match reaction {
        Reaction::SessionCreated { session_id } => Some(session_id.clone()),
        _ => None,
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workspace_then_two_shell_session_is_selected_leased_and_restored_from_disk() {
    let state = tempfile::tempdir().expect("an isolated daemon data directory");
    let project = state.path().join("project");
    std::fs::create_dir(&project).expect("an isolated workspace root");
    let canonical_state = std::fs::canonicalize(state.path()).expect("the canonical test root");
    let canonical_project = std::fs::canonicalize(&project).expect("the canonical project root");

    let daemon = turnd::start(turnd::Config::in_dir(state.path()))
        .await
        .expect("the isolated daemon starts");
    let socket_parent = std::fs::canonicalize(
        daemon
            .socket_path()
            .parent()
            .expect("the temporary socket has a parent"),
    )
    .expect("the socket parent is canonicalizable");
    assert_eq!(socket_parent, canonical_state);
    assert_eq!(daemon.data_dir(), canonical_state);

    let mut gui = GuiHarness::connect(daemon.socket_path().to_path_buf());
    gui.wait_until_ready().await;
    assert!(gui.view().workspaces.is_empty(), "this is a true first run");

    // This is the exact typed action emitted by the first-run Workspace sheet.
    gui.act(ViewAction::CreateWorkspace {
        name: "turn-e2e".into(),
        root: project.display().to_string(),
        continue_to_session: true,
    });
    let workspace_id = gui
        .wait_for("the WorkspaceCreated lifecycle reaction", created_workspace)
        .await;

    gui.wait_for("the new Workspace selection", |gui| {
        let hierarchy = gui.desk.hierarchy()?;
        (hierarchy.tree_state.selected == Some(HierarchyKey::workspace(workspace_id.clone())))
            .then_some(())
    })
    .await;

    let starter = gui
        .view()
        .templates
        .iter()
        .find(|template| template.name == "Two Shells")
        .cloned()
        .expect("turnd installs the portable starter template");
    assert!(
        starter.commands.is_empty(),
        "Two Shells must not hide an optional command: {:?}",
        starter.commands
    );

    // This is the exact typed action emitted by the New Session sheet after Cmd+N.
    gui.act(ViewAction::CreateSessionFromTemplate {
        workspace_id: workspace_id.clone(),
        template_id: starter.id,
        name: "Investigate safely".into(),
        task: Some("Prove first-run onboarding".into()),
    });
    let session_id = gui
        .wait_for("the SessionCreated lifecycle reaction", created_session)
        .await;

    let active_lease = gui
        .wait_for("selection, layout, and the active checkout lease", |gui| {
            let hierarchy = gui.desk.hierarchy()?;
            let workspace = hierarchy
                .workspaces
                .iter()
                .find(|branch| branch.workspace.id == workspace_id)?;
            let session = workspace
                .sessions
                .iter()
                .find(|branch| branch.session.id == session_id)?;
            let lease = workspace.write_lease.as_ref()?;
            let layout = gui.view().layout?;
            (gui.desk.selected() == Some(&session_id)
                && hierarchy.tree_state.selected == Some(HierarchyKey::session(session_id.clone()))
                && session.session.mode == SessionMode::MainCheckout
                && lease.session_id == session_id
                && lease.state == LeaseState::Active
                && layout
                    .panes()
                    .iter()
                    .all(|pane| pane.kind != PaneKind::Agent))
            .then(|| lease.clone())
        })
        .await;

    let view = gui.view();
    let layout = view.layout.expect("session details reached the Desk");
    assert_eq!(layout.pane_count(), 2);
    assert!(layout
        .panes()
        .iter()
        .all(|pane| pane.kind == PaneKind::Shell));
    let hierarchy = gui.desk.hierarchy().expect("the unified tree");
    let created = hierarchy.workspaces[0]
        .sessions
        .iter()
        .find(|branch| branch.session.id == session_id)
        .expect("the created Session is in its Workspace");
    assert!(
        created.nodes.iter().all(|node| !node.is_agentic),
        "the starter flow must never execute Claude or any other agent"
    );

    // A second daemon over the same temporary SQLite database is the persistence
    // proof. Dropping the window first closes its transport without touching state.
    drop(gui);
    daemon.shutdown().await;
    assert!(state.path().join("turn.db").is_file());

    let daemon = turnd::start(turnd::Config::in_dir(state.path()))
        .await
        .expect("turnd restarts over the isolated persistent store");
    let mut restored = GuiHarness::connect(daemon.socket_path().to_path_buf());
    restored.wait_until_ready().await;

    restored
        .wait_for("the persisted tree, selection, and fenced lease", |gui| {
            let hierarchy = gui.desk.hierarchy()?;
            let workspace = hierarchy
                .workspaces
                .iter()
                .find(|branch| branch.workspace.id == workspace_id)?;
            let session = workspace
                .sessions
                .iter()
                .find(|branch| branch.session.id == session_id)?;
            let lease = workspace.write_lease.as_ref()?;
            (workspace.workspace.name == "turn-e2e"
                && workspace.workspace.root == canonical_project.display().to_string()
                && session.session.name == "Investigate safely"
                && session.session.mode == SessionMode::MainCheckout
                && gui.desk.selected() == Some(&session_id)
                && hierarchy.tree_state.selected == Some(HierarchyKey::session(session_id.clone()))
                && lease.id == active_lease.id
                && lease.session_id == session_id
                && lease.state == LeaseState::RecoveryRequired)
                .then_some(())
        })
        .await;

    drop(restored);
    daemon.shutdown().await;
}
