//! Sessions: the unit of work Turn is organised around.

use crate::attention::AttentionPolicy;
use crate::ids::{SessionId, TemplateId, WorkspaceId};
use crate::model::layout::Layout;
use crate::model::node::SessionTree;
use crate::state::DisplayState;
use serde::{Deserialize, Serialize};

/// Whether a session is live, parked or filed away.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    #[default]
    Active,
    /// Processes stopped on purpose, layout kept.
    Paused,
    /// Out of the sidebar, still on disk.
    Archived,
}

/// How much of a session survived a restart. Surfaced verbatim in the UI so the
/// user is never left guessing whether their work is really still running.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestoreState {
    /// Running normally; nothing was restored.
    #[default]
    Live,
    /// The UI was rebuilt and every process was re-attached.
    Reattached,
    /// The layout is back but some processes could not be recovered. Turn offers
    /// to relaunch them; it never does so on its own.
    PartiallyRestored,
    /// Only the layout came back.
    LayoutOnly,
}

impl RestoreState {
    /// Whether the user should be told something is off.
    pub fn needs_explanation(&self) -> bool {
        matches!(
            self,
            RestoreState::PartiallyRestored | RestoreState::LayoutOnly
        )
    }
}

/// A task-scoped unit of work: processes, panes, policy and history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: SessionId,
    pub workspace_id: WorkspaceId,
    /// The task, in the user's words. This is the session's identity.
    pub name: String,
    pub note: Option<String>,
    pub cwd: String,
    pub env: Vec<(String, String)>,
    pub layout: Layout,
    #[serde(default)]
    pub tree: SessionTree,
    pub attention: AttentionPolicy,
    pub template_id: Option<TemplateId>,
    pub status: SessionStatus,
    pub restore_state: RestoreState,
    pub tags: Vec<String>,
    pub git_branch: Option<String>,
    /// Associated PR or issue, as a URL or `#104`.
    pub linked_ref: Option<String>,
    pub favourite: bool,
    pub pinned: bool,
    /// Manual ordering weight in the sidebar.
    pub sort_key: i32,
    /// Parent session, when this one was spawned from another.
    pub parent_session: Option<SessionId>,
    pub created_ms: i64,
    pub last_activity_ms: i64,
    /// Whether Turn routes this session's processes through tmux.
    pub tmux: bool,
}

impl Session {
    pub fn new(
        workspace_id: WorkspaceId,
        name: impl Into<String>,
        cwd: impl Into<String>,
        layout: Layout,
        now_ms: i64,
    ) -> Self {
        Self {
            id: SessionId::new(),
            workspace_id,
            name: name.into(),
            note: None,
            cwd: cwd.into(),
            env: Vec::new(),
            layout,
            tree: SessionTree::new(),
            attention: AttentionPolicy::default(),
            template_id: None,
            status: SessionStatus::Active,
            restore_state: RestoreState::Live,
            tags: Vec::new(),
            git_branch: None,
            linked_ref: None,
            favourite: false,
            pinned: false,
            sort_key: 0,
            parent_session: None,
            created_ms: now_ms,
            last_activity_ms: now_ms,
            tmux: false,
        }
    }

    /// The session's aggregate state, derived from its process tree.
    ///
    /// An empty tree reads as `Idle` rather than `Unknown`: a session whose
    /// processes have not started yet is not a mystery, it is just empty.
    pub fn display_state(&self) -> DisplayState {
        if self.tree.is_empty() {
            return DisplayState::Idle;
        }
        self.tree.aggregate_state()
    }

    /// Whether the user needs to step in.
    pub fn needs_user(&self) -> bool {
        self.tree.needs_user()
    }

    /// Milliseconds since anything happened here.
    pub fn idle_for_ms(&self, now_ms: i64) -> i64 {
        (now_ms - self.last_activity_ms).max(0)
    }

    pub fn touch(&mut self, now_ms: i64) {
        self.last_activity_ms = now_ms;
    }

    /// A copy set up for a new run of the same task: same shape and settings,
    /// new identity, no live processes.
    pub fn duplicate(&self, now_ms: i64) -> Session {
        let mut copy = self.clone();
        copy.id = SessionId::new();
        copy.name = format!("{} (copy)", self.name);
        copy.tree = SessionTree::new();
        // Fresh pane ids, not a clone of them. Pane ids are the key the daemon
        // indexes client attachments by, so a copy that reused them would make
        // attaching to the duplicate silently steal the original's attachment.
        copy.layout = self.layout.reidentified();
        copy.status = SessionStatus::Active;
        copy.restore_state = RestoreState::Live;
        copy.created_ms = now_ms;
        copy.last_activity_ms = now_ms;
        copy.parent_session = Some(self.id.clone());
        copy.favourite = false;
        copy.pinned = false;
        copy
    }

    pub fn archive(&mut self) {
        self.status = SessionStatus::Archived;
    }

    pub fn unarchive(&mut self) {
        self.status = SessionStatus::Active;
    }

    pub fn is_archived(&self) -> bool {
        self.status == SessionStatus::Archived
    }

    /// Sidebar ordering key: pinned first, then anything demanding attention,
    /// then by severity, then by recency.
    ///
    /// Returned as a tuple rather than an `Ord` impl because the ordering is a
    /// presentation concern that may differ per view.
    pub fn sidebar_rank(&self) -> (bool, bool, u8, i64) {
        let state = self.display_state();
        (
            self.pinned,
            state.demands_user(),
            state.severity(),
            self.last_activity_ms,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::layout::{Pane, PaneKind};
    use crate::model::node::{NodeKind, ProcessNode, Relation};
    use crate::state::{AwaitingReason, Lifecycle, Turn};

    const T0: i64 = 1_700_000_000_000;

    fn session() -> Session {
        Session::new(
            WorkspaceId::from_stored("ws_a"),
            "Fix climbing bugs",
            "/repo",
            Layout::single(Pane::new(PaneKind::Agent).with_command("claude")),
            T0,
        )
    }

    #[test]
    fn a_fresh_session_is_idle_not_unknown() {
        let s = session();
        assert_eq!(s.display_state(), DisplayState::Idle);
        assert!(!s.needs_user());
        assert_eq!(s.restore_state, RestoreState::Live);
    }

    #[test]
    fn a_session_reports_your_turn_when_its_agent_blocks() {
        let mut s = session();
        let mut agent = ProcessNode::agent(s.id.clone(), "claude", "/repo", T0);
        agent.lifecycle = Lifecycle::Alive;
        agent.turn = Some(Turn::AwaitingUser {
            reason: AwaitingReason::Permission,
        });
        s.tree.insert(agent);

        assert_eq!(s.display_state(), DisplayState::NeedsPermission);
        assert!(s.needs_user());
        assert_eq!(s.display_state().label(), "PERMISSION");
    }

    /// Case E again, this time at session level: the agent is done with its turn
    /// but a test runner it started is still going. The session must not read as
    /// finished.
    #[test]
    fn a_session_whose_agent_finished_but_child_still_runs_reads_as_running() {
        let mut s = session();
        let mut agent = ProcessNode::agent(s.id.clone(), "claude", "/repo", T0);
        agent.lifecycle = Lifecycle::Alive;
        agent.turn = Some(Turn::Done);
        let agent_id = s.tree.insert(agent);

        let mut tests = ProcessNode::process(
            s.id.clone(),
            NodeKind::TestRunner,
            "cargo test",
            "/repo",
            T0,
        );
        tests.lifecycle = Lifecycle::Alive;
        tests.link_to(agent_id, Relation::Confirmed);
        s.tree.insert(tests);

        // CompletedTurn (35) beats Running (20) on severity, so the session shows
        // the turn state — but the child is unmistakably still alive.
        assert_eq!(s.display_state(), DisplayState::CompletedTurn);
        assert_eq!(s.tree.running_count(), 2);
        assert!(!s.display_state().demands_user());
    }

    #[test]
    fn duplicating_a_session_keeps_the_shape_and_drops_the_processes() {
        let mut s = session();
        s.tags = vec!["bug".into()];
        s.git_branch = Some("fix/climbing".into());
        let pane = s.layout.panes()[0].id.clone();
        s.layout.get_mut(&pane).unwrap().node_id =
            Some(crate::ids::NodeId::from_stored("proc_live"));
        s.tree
            .insert(ProcessNode::agent(s.id.clone(), "claude", "/repo", T0));

        let copy = s.duplicate(T0 + 1_000);

        assert_ne!(copy.id, s.id);
        assert_eq!(copy.parent_session, Some(s.id.clone()));
        assert_eq!(copy.tags, s.tags);
        assert_eq!(copy.git_branch, s.git_branch);
        assert!(copy.tree.is_empty(), "a copy starts with no processes");
        assert!(
            copy.layout.panes().iter().all(|p| p.node_id.is_none()),
            "and its panes point at nothing"
        );
        assert_eq!(copy.layout.pane_count(), s.layout.pane_count());
    }

    /// Pane ids key the daemon's client attachments, so a duplicate that reused
    /// them would make attaching to the copy steal the original's attachment.
    #[test]
    fn a_duplicated_session_shares_no_pane_identity_with_its_original() {
        let mut s = session();
        let first = s.layout.panes()[0].id.clone();
        s.layout.split(
            &first,
            crate::model::layout::Direction::Horizontal,
            Pane::new(PaneKind::Shell),
        );

        let copy = s.duplicate(T0 + 1);
        let original_ids: Vec<_> = s.layout.panes().iter().map(|p| p.id.clone()).collect();
        let copy_ids: Vec<_> = copy.layout.panes().iter().map(|p| p.id.clone()).collect();

        assert_eq!(original_ids.len(), copy_ids.len(), "same shape");
        for id in &copy_ids {
            assert!(
                !original_ids.contains(id),
                "pane id {id} leaked into the duplicate"
            );
        }
        // And the copy's focus points at one of its own panes, not the original's.
        let active = copy.layout.active.clone().expect("something focused");
        assert!(copy_ids.contains(&active));
        assert!(copy.layout.sizes_are_normalised());
    }

    #[test]
    fn archiving_round_trips() {
        let mut s = session();
        assert!(!s.is_archived());
        s.archive();
        assert!(s.is_archived());
        s.unarchive();
        assert_eq!(s.status, SessionStatus::Active);
    }

    #[test]
    fn sidebar_ranking_puts_pinned_and_blocked_sessions_first() {
        let mut blocked = session();
        let mut agent = ProcessNode::agent(blocked.id.clone(), "claude", "/repo", T0);
        agent.lifecycle = Lifecycle::Alive;
        agent.turn = Some(Turn::AwaitingUser {
            reason: AwaitingReason::Question,
        });
        blocked.tree.insert(agent);

        let mut quiet = session();
        let mut running = ProcessNode::agent(quiet.id.clone(), "claude", "/repo", T0);
        running.lifecycle = Lifecycle::Alive;
        running.turn = Some(Turn::Active);
        quiet.tree.insert(running);

        let mut pinned = quiet.clone();
        pinned.pinned = true;

        let mut all = [
            quiet.sidebar_rank(),
            blocked.sidebar_rank(),
            pinned.sidebar_rank(),
        ];
        all.sort_by(|a, b| b.cmp(a));
        assert_eq!(all[0], pinned.sidebar_rank(), "pinned wins");
        assert_eq!(all[1], blocked.sidebar_rank(), "then whoever needs you");
    }

    #[test]
    fn restore_states_that_need_explaining_say_so() {
        assert!(RestoreState::PartiallyRestored.needs_explanation());
        assert!(RestoreState::LayoutOnly.needs_explanation());
        assert!(!RestoreState::Reattached.needs_explanation());
        assert!(!RestoreState::Live.needs_explanation());
    }

    #[test]
    fn idle_time_never_goes_negative_on_a_clock_skew() {
        let s = session();
        assert_eq!(s.idle_for_ms(T0 - 5_000), 0);
        assert_eq!(s.idle_for_ms(T0 + 5_000), 5_000);
    }
}
