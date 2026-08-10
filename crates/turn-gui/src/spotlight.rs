//! Picking a subagent out of a crowded layout, and putting the layout back.
//!
//! An agent that is managing work has children: the subagents it reported through its hooks —
//! `Explore`, a reviewer, a teammate — and the processes it started that Turn found in the
//! process table. The tree lists all of them, which is what makes it possible to see what an
//! agent is doing. What the tree could not do was *show* you one: a subagent has no pane of its
//! own, it runs inside its parent's, and finding it meant reading a pane shared by four of them.
//!
//! So a click on a managed node maximises the pane it is running in, and a click on the thing
//! that owns it — its agent, its Session, its Workspace — puts the layout back. The tree becomes
//! the way you point at one worker among many, which is the job it was always closest to.
//!
//! The decision is a value rather than something buried in the click handler, because the part
//! worth getting right is *which* pane, and that is answerable without a window: a subagent's
//! pane is the nearest ancestor's, walking up until one is bound to a pane.

use turn_core::ids::{NodeId, PaneId};
use turn_proto::{SessionTreeView, TreeNodeView};

/// What a click on a row of the tree should do to the layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Spotlight {
    /// Maximise this pane, because the row the user picked runs inside it.
    Show(PaneId),
    /// Put the layout back, because the row the user picked owns others.
    Restore,
    /// Leave the layout alone. The row says nothing about which pane to show — a node with no
    /// pane anywhere above it, or a shell the user is merely selecting.
    Leave,
}

/// Whether this node is one an agent is *managing*: a subagent it reported, or a process it
/// started, at any depth below it.
///
/// The test is the ancestry rather than the node itself, and it has to be. A subagent reported
/// through a hook is agentic; a `bash` or a `gh` that an agent started is not, and both are
/// things the agent is managing and both are what the user asked to be able to point at. What
/// is *not* managed is the agent itself, and the shell the pane belongs to.
pub fn is_managed(session: &SessionTreeView, node: &TreeNodeView) -> bool {
    ancestors(session, node).any(|ancestor| ancestor.is_agentic)
}

/// The pane a node's output appears in: its own, or the nearest one above it.
///
/// A subagent has no pane. It runs inside the agent's, which runs inside a shell's, and the
/// binding may be on any of them — so the walk goes up until it finds one rather than assuming
/// which level holds it.
pub fn hosting_pane(session: &SessionTreeView, node: &TreeNodeView) -> Option<PaneId> {
    std::iter::once(node)
        .chain(ancestors(session, node))
        .find_map(|candidate| {
            candidate
                .pane_bindings
                .first()
                .map(|binding| binding.pane_id.clone())
        })
}

/// What clicking this node should do.
pub fn for_node(session: &SessionTreeView, node: &TreeNodeView) -> Spotlight {
    if is_managed(session, node) {
        match hosting_pane(session, node) {
            Some(pane) => Spotlight::Show(pane),
            // A node Turn knows about with no pane anywhere above it. Nothing to show, and
            // guessing a pane would maximise something the user did not point at.
            None => Spotlight::Leave,
        }
    } else if node.child_count > 0 {
        // The owner of the things below it: clicking it is asking to see them all again.
        Spotlight::Restore
    } else {
        Spotlight::Leave
    }
}

/// The chain of parents above a node, nearest first.
///
/// Bounded by the number of nodes in the Session, so a cycle in the parent links — which the
/// daemon does not produce, and which a corrupted store could — ends the walk instead of hanging
/// the window.
fn ancestors<'a>(
    session: &'a SessionTreeView,
    node: &'a TreeNodeView,
) -> impl Iterator<Item = &'a TreeNodeView> {
    let mut next: Option<NodeId> = node.parent.clone();
    let mut budget = session.nodes.len();
    std::iter::from_fn(move || {
        if budget == 0 {
            return None;
        }
        budget -= 1;
        let id = next.take()?;
        let found = session
            .nodes
            .iter()
            .find(|candidate| candidate.node_id == id)?;
        next = found.parent.clone();
        Some(found)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use turn_core::ids::SessionId;
    use turn_core::model::hierarchy::PaneNodeBinding;
    use turn_core::model::node::{NodeKind, ProcessNode};
    use turn_core::model::{Layout, Pane, PaneKind, Session};
    use turn_proto::{SessionSummary, TreeNodeView};

    const T0: i64 = 1_700_000_000_000;

    /// A shell hosting an agent, one subagent the agent reported, and one process it started.
    /// Only the shell is bound to a pane, which is the shape a real Session has: a subagent runs
    /// inside its parent's pane and has none of its own.
    pub(super) fn fixture() -> (SessionTreeView, PaneId) {
        let session_id = SessionId::from_stored("sess_spotlight");
        let pane = PaneId::from_stored("pane_left");

        let shell = ProcessNode::process(session_id.clone(), NodeKind::Shell, "zsh", "/repo", T0);
        let mut agent = ProcessNode::agent(session_id.clone(), "claude", "/repo", T0);
        agent.parent = Some(shell.id.clone());
        let mut subagent = ProcessNode::process(
            session_id.clone(),
            NodeKind::Subagent,
            "Explore",
            "/repo",
            T0,
        );
        subagent.parent = Some(agent.id.clone());
        let mut started =
            ProcessNode::process(session_id.clone(), NodeKind::Unknown, "gh", "/repo", T0);
        started.parent = Some(agent.id.clone());

        let view = |node: &ProcessNode, depth: usize, children: usize| {
            TreeNodeView::from_node(node, depth, children, T0)
        };
        let mut shell_view = view(&shell, 0, 1);
        shell_view.pane_bindings = vec![PaneNodeBinding {
            pane_id: pane.clone(),
            session_id: session_id.clone(),
            node_id: shell.id.clone(),
            temporary: false,
            surface_id: None,
            opened_ms: T0,
        }];

        let layout = Layout::single(Pane::new(PaneKind::Shell));
        let summary = SessionSummary::from_session(
            &Session::new(
                turn_core::ids::WorkspaceId::from_stored("ws_spotlight"),
                "Spotlight",
                "/repo",
                layout,
                T0,
            ),
            0,
            false,
            T0,
        );

        (
            SessionTreeView {
                session: summary,
                nodes: vec![
                    shell_view,
                    view(&agent, 1, 2),
                    view(&subagent, 2, 0),
                    view(&started, 2, 0),
                ],
            },
            pane,
        )
    }

    pub(super) fn node_titled<'a>(session: &'a SessionTreeView, title: &str) -> &'a TreeNodeView {
        session
            .nodes
            .iter()
            .find(|node| node.title == title)
            .unwrap_or_else(|| panic!("the fixture has a node titled {title:?}"))
    }

    /// The feature: a subagent has no pane, and clicking it still shows you one.
    #[test]
    fn a_subagent_is_shown_in_the_pane_it_runs_inside() {
        let (session, pane) = fixture();
        let subagent = node_titled(&session, "Explore");
        assert!(
            subagent.pane_bindings.is_empty(),
            "a subagent has no pane of its own — that is the whole difficulty"
        );
        assert_eq!(
            for_node(&session, subagent),
            Spotlight::Show(pane),
            "so the pane shown is the nearest one above it"
        );
    }

    /// A plain process an agent started is managed too. The ask was "every subagent *or process*
    /// the agent is managing", and a `gh` an agent ran is as worth pointing at as a subagent.
    #[test]
    fn a_process_the_agent_started_is_shown_the_same_way() {
        let (session, pane) = fixture();
        assert_eq!(
            for_node(&session, node_titled(&session, "gh")),
            Spotlight::Show(pane)
        );
    }

    /// Clicking what owns them puts the layout back. Without it, a maximised pane is a state the
    /// tree can enter and not leave.
    #[test]
    fn clicking_the_agent_that_owns_them_restores_the_layout() {
        let (session, _) = fixture();
        assert_eq!(
            for_node(&session, node_titled(&session, "claude")),
            Spotlight::Restore,
            "the agent owns the rows below it, so it is the way back out"
        );
        assert_eq!(
            for_node(&session, node_titled(&session, "zsh")),
            Spotlight::Restore,
            "and so is the shell above it"
        );
    }

    /// A node with nothing under it and no agent above it says nothing about panes. Selecting it
    /// must not disturb a layout the user arranged.
    #[test]
    fn a_lone_node_leaves_the_layout_alone() {
        let (mut session, _) = fixture();
        session.nodes.retain(|node| node.title == "zsh");
        if let Some(only) = session.nodes.first_mut() {
            only.child_count = 0;
            only.pane_bindings.clear();
        }
        let only = session.nodes[0].clone();
        assert_eq!(for_node(&session, &only), Spotlight::Leave);
    }

    /// When the fixture's nodes started.
    pub(super) fn started_at() -> i64 {
        T0
    }

    /// Records that a node last produced something at `at_ms`.
    pub(super) fn set_last_activity(session: &mut SessionTreeView, title: &str, at_ms: i64) {
        let node = session
            .nodes
            .iter_mut()
            .find(|node| node.title == title)
            .expect("the fixture has this node");
        node.activity_preview = Some(turn_core::model::hierarchy::ActivityPreview {
            node_id: node.node_id.clone(),
            raw_source_sequence: None,
            normalized_text: "working".into(),
            source: turn_core::model::hierarchy::PreviewSource::SemanticEvent,
            confidence: turn_core::Confidence::Explicit,
            stable: true,
            contains_sensitive_data: false,
            redacted: false,
            updated_ms: at_ms,
        });
    }

    /// Marks a node as finished.
    pub(super) fn end_node(session: &mut SessionTreeView, title: &str) {
        let node = session
            .nodes
            .iter_mut()
            .find(|node| node.title == title)
            .expect("the fixture has this node");
        node.lifecycle = turn_core::state::Lifecycle::Exited { code: 0 };
    }

    /// A parent link that points into a loop must not hang the window. The daemon does not
    /// produce one; a hand-edited store could.
    #[test]
    fn a_cycle_in_the_parent_links_ends_the_walk_instead_of_hanging() {
        let (mut session, _) = fixture();
        let (first, second) = (
            session.nodes[0].node_id.clone(),
            session.nodes[1].node_id.clone(),
        );
        session.nodes[0].parent = Some(second);
        session.nodes[1].parent = Some(first);
        session.nodes[0].pane_bindings.clear();
        let looped = session.nodes[0].clone();
        // Terminates, and finds no pane, which is the honest answer for a broken tree.
        assert_eq!(hosting_pane(&session, &looped), None);
    }
}

/// How long a managed node has had nothing to say, and whether that is long enough to mention.
///
/// The signal is the activity preview's own timestamp, which the daemon updates when the node
/// produces something worth showing. Turn does not invent a heartbeat: a node with no preview at
/// all has never said anything, and "silent since it started" is a different claim from "silent
/// for six minutes" — so the first is reported from the node's start and the second from its last
/// word, and neither is guessed.
///
/// Only for nodes an agent is *managing*. A shell sitting at a prompt is not idle, it is waiting
/// for its owner, and a tree that nagged about it would be wrong about every pane the user is
/// not currently typing in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Idleness {
    /// Milliseconds since the node last produced anything.
    pub silent_ms: i64,
    /// Whether it has been silent long enough to be worth a word on the row.
    pub worth_saying: bool,
}

/// How long a subagent may be quiet before the row says so, in milliseconds.
///
/// Two minutes. Short enough that a worker which has finished and left nothing behind is noticed
/// in the same sitting, long enough that an agent thinking about a hard question is not accused
/// of having stalled. It is a threshold for a *word on a row*, not for an intervention: nothing
/// is stopped and nothing steals focus, so being wrong about it costs a line of text.
pub const IDLE_AFTER_MS: i64 = 2 * 60 * 1000;

/// Whether and how long this node has been quiet.
///
/// `None` for a node that is not being managed, for one that has already ended — a finished
/// worker is not idle, it is finished — and when the clock disagrees with the record, which
/// happens across a daemon restart and must not produce a negative duration presented as a
/// number.
pub fn idleness(session: &SessionTreeView, node: &TreeNodeView, now_ms: i64) -> Option<Idleness> {
    if !is_managed(session, node) || node.lifecycle.is_terminal() {
        return None;
    }
    let last = node
        .activity_preview
        .as_ref()
        .map(|preview| preview.updated_ms)
        .unwrap_or(node.started_ms);
    let silent_ms = now_ms.checked_sub(last).filter(|silent| *silent >= 0)?;
    Some(Idleness {
        silent_ms,
        worth_saying: silent_ms >= IDLE_AFTER_MS,
    })
}

/// A silence, in the shortest form that is still true.
///
/// Whole minutes past a minute, seconds below it. A row is scanned rather than read, and "4m" is
/// read at a glance where "4 minutes 12 seconds" is not — while under a minute the seconds are
/// the only thing that distinguishes "just now" from "nearly a minute".
pub fn describe_silence(silent_ms: i64) -> String {
    let seconds = silent_ms / 1000;
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m", seconds / 60)
    } else {
        format!("{}h{}m", seconds / 3600, (seconds % 3600) / 60)
    }
}

#[cfg(test)]
mod idle_tests {
    use super::tests::*;
    use super::*;

    /// The signal the user asked for: a subagent that has gone quiet says so on its own row.
    #[test]
    fn a_subagent_that_has_gone_quiet_says_how_long() {
        let (mut session, _) = fixture();
        let now = started_at() + IDLE_AFTER_MS + 60_000;
        set_last_activity(&mut session, "Explore", started_at());

        let subagent = node_titled(&session, "Explore");
        let idle = idleness(&session, subagent, now).expect("a managed, living node");
        assert!(idle.worth_saying, "three minutes is worth a word");
        assert_eq!(describe_silence(idle.silent_ms), "3m");
    }

    /// A worker that has just spoken is not idle, however long the Session has been open.
    #[test]
    fn a_subagent_that_just_spoke_is_not_idle() {
        let (mut session, _) = fixture();
        let now = started_at() + 10 * IDLE_AFTER_MS;
        set_last_activity(&mut session, "Explore", now - 5_000);
        let idle = idleness(&session, node_titled(&session, "Explore"), now).expect("managed");
        assert!(!idle.worth_saying);
        assert_eq!(describe_silence(idle.silent_ms), "5s");
    }

    /// A shell is never idle. It is waiting for the person at the keyboard, and a tree that
    /// nagged about it would be wrong about every pane the user is not typing in right now.
    #[test]
    fn a_shell_is_never_reported_as_idle() {
        let (session, _) = fixture();
        assert_eq!(
            idleness(
                &session,
                node_titled(&session, "zsh"),
                started_at() + 10 * IDLE_AFTER_MS
            ),
            None
        );
    }

    /// A finished worker is finished, not idle. Saying "silent for 40 minutes" about something
    /// that ended cleanly forty minutes ago invites the user to fix a thing that is not wrong.
    #[test]
    fn a_worker_that_has_ended_is_not_reported_as_idle() {
        let (mut session, _) = fixture();
        end_node(&mut session, "Explore");
        assert_eq!(
            idleness(
                &session,
                node_titled(&session, "Explore"),
                started_at() + 10 * IDLE_AFTER_MS
            ),
            None
        );
    }

    /// A clock that disagrees with the record — which happens across a restart — must not produce
    /// a negative silence dressed up as a number.
    #[test]
    fn a_record_from_the_future_reports_nothing_rather_than_a_negative_silence() {
        let (mut session, _) = fixture();
        set_last_activity(&mut session, "Explore", started_at() + 60_000);
        assert_eq!(
            idleness(&session, node_titled(&session, "Explore"), started_at()),
            None
        );
    }

    #[test]
    fn a_silence_is_described_in_the_shortest_true_form() {
        assert_eq!(describe_silence(0), "0s");
        assert_eq!(describe_silence(59_999), "59s");
        assert_eq!(describe_silence(60_000), "1m");
        assert_eq!(describe_silence(3_600_000), "1h0m");
        assert_eq!(describe_silence(3_900_000), "1h5m");
    }
}
