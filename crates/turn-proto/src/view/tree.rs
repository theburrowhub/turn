//! The process tree as the UI draws it: flat rows with a depth, and honesty
//! about which edges are guesses.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use turn_core::ids::{NodeId, SessionId};
use turn_core::model::{
    ActivityPreview, NodeKind, PaneNodeBinding, PreviewVisibility, ProcessNode, Relationship,
    SessionTree,
};
use turn_core::state::{DisplayState, Lifecycle, Turn};

use super::hierarchy::NodePaneCapability;
use super::session::AgentSummary;

/// One row of the agent/process tree.
///
/// Flat with a `depth` rather than nested, for the same reason
/// [`SessionTree`](turn_core::model::SessionTree) stores parent pointers: the
/// shape changes as subagents come and go, and re-rendering a list is cheaper and
/// less error-prone than diffing a recursive structure.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct TreeNodeView {
    pub node_id: NodeId,
    pub session_id: SessionId,
    pub parent: Option<NodeId>,
    /// What the parent edge means and how confidently it was established.
    pub relationship: Relationship,
    /// Derived from the five-level confidence ladder; never guessed by the UI.
    pub relationship_is_provisional: bool,
    /// Indentation level. Roots are 0.
    pub depth: usize,
    pub child_count: usize,

    pub kind: NodeKind,
    /// Whether this node carries the agent turn axis at all. A shell has no turn
    /// state and the UI must not render an empty slot for one.
    pub is_agentic: bool,
    /// The title to show, already resolved through the precedence: a name the user
    /// typed, then what an agent declared about itself, then what the process printed,
    /// then the command. Resolved here so no client can apply a different order.
    pub title: String,
    /// Whether that title is something Turn read rather than was told.
    ///
    /// True for a title a process set for itself. It is drawn differently for a
    /// reason no filter can address: `✓ tests passed`, or the name of another of the
    /// user's sessions, are both valid text a process is free to print, so such a
    /// name must never carry the same authority as one a tool reported through a
    /// contract.
    #[serde(default)]
    pub title_is_provisional: bool,
    pub command: String,
    pub args: Vec<String>,
    pub cwd: String,
    pub pid: Option<u32>,
    pub ppid: Option<u32>,

    pub lifecycle: Lifecycle,
    /// Present only for agentic nodes.
    pub turn: Option<Turn>,
    /// The projection of the two axes. Derived, never stored.
    pub display_state: DisplayState,
    /// The short sidebar label for `display_state`.
    pub state_label: String,
    pub severity: u8,
    pub needs_user: bool,
    pub interaction_pending: bool,

    /// The last stable, redacted semantic fact suitable for the navigation row.
    /// This is never raw PTY output or restored conversation history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity_preview: Option<ActivityPreview>,
    pub preview_visibility: PreviewVisibility,

    /// Zero or more visual bindings. An empty list is the normal background
    /// state; it does not say that the Process is stopped.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pane_bindings: Vec<PaneNodeBinding>,
    /// What an explicit open action may render. It is independent of whether a
    /// Pane is already bound.
    pub pane_capability: NodePaneCapability,
    pub started_ms: i64,
    pub ended_ms: Option<i64>,
    /// How long it has been running, or how long it ran.
    pub runtime_ms: i64,
    pub exit_code: Option<i32>,

    /// Agent detail, including any pending permission or question.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentSummary>,
}

impl TreeNodeView {
    /// Projects one node, given its place in the tree.
    pub fn from_node(node: &ProcessNode, depth: usize, child_count: usize, now_ms: i64) -> Self {
        let display_state = node.display_state();
        // One call, so a client cannot apply a different precedence than the daemon.
        let (resolved_title, title_source) = node.resolved_title();
        Self {
            node_id: node.id.clone(),
            session_id: node.session_id.clone(),
            parent: node.parent.clone(),
            relationship: node.relationship,
            // A root has no edge to qualify. Unknown confidence is shown only on
            // an actual parent relationship, never invented between Session and
            // its structurally contained root Process.
            relationship_is_provisional: node.parent.is_some()
                && node.relationship.confidence.is_provisional(),
            depth,
            child_count,
            kind: node.kind,
            is_agentic: node.kind.is_agentic(),
            title: resolved_title,
            title_is_provisional: title_source.is_provisional(),
            command: node.command.clone(),
            args: node.args.clone(),
            cwd: node.cwd.clone(),
            pid: node.pid,
            ppid: node.ppid,
            lifecycle: node.lifecycle.clone(),
            turn: node.turn.clone(),
            display_state,
            state_label: display_state.label().to_string(),
            severity: display_state.severity(),
            needs_user: display_state.demands_user(),
            interaction_pending: node.interaction_pending,
            activity_preview: if node.preview_visibility == PreviewVisibility::Hide {
                None
            } else {
                node.activity_preview.clone()
            },
            preview_visibility: node.preview_visibility,
            pane_bindings: Vec::new(),
            pane_capability: NodePaneCapability::default(),
            started_ms: node.started_ms,
            ended_ms: node.ended_ms,
            runtime_ms: node.runtime_ms(now_ms),
            exit_code: node.exit_code,
            agent: AgentSummary::from_node(node),
        }
    }

    /// Flattens a whole tree into draw order: each root followed by its subtree,
    /// depth-first, siblings in insertion order.
    ///
    /// A node whose parent is not in the tree is treated as a root rather than
    /// hidden. Turn refuses to invent relationships, and the corollary is that it
    /// must not lose a process just because it cannot place it — an orphan renders
    /// at the top level, visible and unattributed.
    pub fn flatten(tree: &SessionTree, now_ms: i64) -> Vec<TreeNodeView> {
        let mut out = Vec::new();
        // Guards against a cycle reaching us from a corrupted store. `relink`
        // refuses to build one, but this walk must terminate regardless of what
        // it is handed.
        let mut visited: HashSet<NodeId> = HashSet::new();

        let roots: Vec<&ProcessNode> = tree
            .iter()
            .filter(|n| match &n.parent {
                None => true,
                Some(parent) => tree.get(parent).is_none(),
            })
            .collect();

        for root in roots {
            push_subtree(tree, root, now_ms, &mut visited, &mut out);
        }

        // Anything a cycle kept us from reaching is still the user's process and
        // is still shown, at the root, rather than silently dropped.
        for node in tree.iter() {
            if !visited.contains(&node.id) {
                let children = tree.children(&node.id).len();
                out.push(TreeNodeView::from_node(node, 0, children, now_ms));
                visited.insert(node.id.clone());
            }
        }

        out
    }

    /// Legacy convenience projection when no binding/capability repository is
    /// available. The rows remain honest: no Pane or attachable terminal is
    /// invented from Layout alone.
    pub fn for_session(session: &turn_core::model::Session, now_ms: i64) -> Vec<TreeNodeView> {
        Self::flatten(&session.tree, now_ms)
    }

    /// Projects a Session with the normalised Pane→Node records and runtime
    /// capabilities supplied by the daemon. This is the constructor used by the
    /// unified hierarchy; `pane_node_bindings` is authoritative in protocol v3.
    pub fn for_session_with_panes(
        session: &turn_core::model::Session,
        bindings: &[PaneNodeBinding],
        capabilities: &HashMap<NodeId, NodePaneCapability>,
        now_ms: i64,
    ) -> Vec<TreeNodeView> {
        let mut rows = Self::flatten(&session.tree, now_ms);
        for row in &mut rows {
            row.pane_bindings = bindings
                .iter()
                .filter(|binding| {
                    binding.session_id == session.id && binding.node_id == row.node_id
                })
                .cloned()
                .collect();
            if let Some(capability) = capabilities.get(&row.node_id) {
                row.pane_capability = capability.clone();
            }
        }
        rows
    }
}

/// Appends `root` and everything under it, depth-first, siblings in insertion
/// order.
///
/// The walk keeps its own stack rather than recursing, for the same reason
/// [`SessionTree::descendants`](turn_core::model::SessionTree::descendants) does.
/// `visited` bounds the number of *visits*, which is what stops a cycle; nothing
/// bounds the *depth*. A parent chain arrives from the store, which is trusted but
/// not infallible — this walk already exists to tolerate a dangling parent and a
/// cycle — so a chain thousands of links long is a shape the store can legitimately
/// hand over, and one frame per link would end the daemon in a stack overflow
/// rather than a rendered tree.
fn push_subtree<'a>(
    tree: &'a SessionTree,
    root: &'a ProcessNode,
    now_ms: i64,
    visited: &mut HashSet<NodeId>,
    out: &mut Vec<TreeNodeView>,
) {
    let mut stack: Vec<(&'a ProcessNode, usize)> = vec![(root, 0)];
    while let Some((node, depth)) = stack.pop() {
        if !visited.insert(node.id.clone()) {
            continue;
        }
        let children = tree.children(&node.id);
        out.push(TreeNodeView::from_node(node, depth, children.len(), now_ms));
        // Pushed in reverse so popping yields insertion order, which is the order
        // the user watched the tree grow in.
        for child in children.into_iter().rev() {
            stack.push((child, depth + 1));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use turn_core::event::Confidence;
    use turn_core::ids::{PaneId, SessionId};
    use turn_core::model::{PreviewSource, Relation, RelationshipKind};
    use turn_core::state::AwaitingReason;

    const T0: i64 = 1_700_000_000_000;

    fn session() -> SessionId {
        SessionId::from_stored("sess_tree0001")
    }

    fn tree_with_confirmed_subagent() -> (SessionTree, NodeId, NodeId) {
        let mut tree = SessionTree::new();
        let mut agent = ProcessNode::agent(session(), "claude", "/repo", T0);
        agent.lifecycle = Lifecycle::Alive;
        agent.turn = Some(Turn::Active);
        let agent_id = tree.insert(agent);

        let mut sub = ProcessNode::agent(session(), "claude --subagent", "/repo", T0);
        sub.kind = NodeKind::Subagent;
        sub.lifecycle = Lifecycle::Alive;
        sub.turn = Some(Turn::Active);
        sub.link_to(agent_id.clone(), Relation::Confirmed);
        let sub_id = tree.insert(sub);

        (tree, agent_id, sub_id)
    }

    #[test]
    fn a_confirmed_subagent_renders_indented_and_not_as_a_guess() {
        let (tree, agent_id, sub_id) = tree_with_confirmed_subagent();
        let rows = TreeNodeView::flatten(&tree, T0 + 5_000);

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].node_id, agent_id);
        assert_eq!(rows[0].depth, 0);
        assert_eq!(rows[0].child_count, 1);

        assert_eq!(rows[1].node_id, sub_id);
        assert_eq!(rows[1].depth, 1);
        assert_eq!(rows[1].relationship.kind, RelationshipKind::SpawnedBy);
        assert_eq!(rows[1].relationship.confidence, Confidence::Explicit);
        assert!(
            !rows[1].relationship_is_provisional,
            "a tool-reported subagent is a fact, not a guess"
        );
        assert!(rows[1].is_agentic);
    }

    /// The rule the brief is explicit about: a relationship guessed from the
    /// process table must reach the UI marked as a guess.
    #[test]
    fn an_inferred_parent_link_reaches_the_ui_marked_as_provisional() {
        let (mut tree, agent_id, _) = tree_with_confirmed_subagent();
        let mut guessed =
            ProcessNode::process(session(), NodeKind::TestRunner, "npm test", "/", T0);
        guessed.lifecycle = Lifecycle::Alive;
        guessed.link_to(agent_id, Relation::Inferred);
        let guessed_id = tree.insert(guessed);

        let rows = TreeNodeView::flatten(&tree, T0);
        let row = rows.iter().find(|r| r.node_id == guessed_id).unwrap();
        assert_eq!(row.relationship.kind, RelationshipKind::SpawnedBy);
        assert_eq!(row.relationship.confidence, Confidence::InferredHigh);
        assert!(row.relationship_is_provisional);
        assert!(row.turn.is_none(), "a test runner owes the user nothing");
        assert_eq!(row.display_state, DisplayState::Running);
    }

    #[test]
    fn an_unattributable_process_renders_at_the_root_rather_than_vanishing() {
        let (mut tree, _, _) = tree_with_confirmed_subagent();
        let orphan = ProcessNode::process(session(), NodeKind::Unknown, "mystery", "/", T0);
        let orphan_id = tree.insert(orphan);

        let rows = TreeNodeView::flatten(&tree, T0);
        let row = rows.iter().find(|r| r.node_id == orphan_id).unwrap();
        assert_eq!(row.depth, 0);
        assert_eq!(row.relationship.kind, RelationshipKind::Unknown);
        assert_eq!(row.relationship.confidence, Confidence::Unknown);
        assert!(
            !row.relationship_is_provisional,
            "a root has no parent edge to mark as provisional"
        );
        assert_eq!(rows.len(), 3, "every process is accounted for");
    }

    /// A parent removed while its child lived on. The child must still be drawn.
    #[test]
    fn a_node_whose_parent_is_missing_is_promoted_to_a_root() {
        let mut tree = SessionTree::new();
        let mut child = ProcessNode::process(session(), NodeKind::Server, "npm run dev", "/", T0);
        // A parent id that was never inserted, as a stale store row would give us.
        child.link_to(NodeId::from_stored("proc_ghost0001"), Relation::Inferred);
        tree.insert(child);

        let rows = TreeNodeView::flatten(&tree, T0);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].depth, 0, "it is orphaned, not hidden");
        assert!(
            rows[0].parent.is_some(),
            "but we do not pretend it had no parent"
        );
        assert!(
            rows[0].relationship_is_provisional,
            "the dangling parent edge remains an inferred edge"
        );
    }

    #[test]
    fn the_derived_state_and_its_label_come_from_the_two_axes() {
        let mut tree = SessionTree::new();
        let mut blocked = ProcessNode::agent(session(), "claude", "/repo", T0);
        blocked.lifecycle = Lifecycle::Alive;
        blocked.turn = Some(Turn::AwaitingUser {
            reason: AwaitingReason::Permission,
        });
        tree.insert(blocked);

        let row = &TreeNodeView::flatten(&tree, T0)[0];
        assert_eq!(row.display_state, DisplayState::NeedsPermission);
        assert_eq!(row.state_label, "PERMISSION");
        assert!(row.needs_user);
        assert_eq!(row.severity, DisplayState::NeedsPermission.severity());
    }

    #[test]
    fn a_crashed_agent_that_last_said_it_was_waiting_reads_as_failed() {
        let mut tree = SessionTree::new();
        let mut dead = ProcessNode::agent(session(), "claude", "/repo", T0);
        dead.turn = Some(Turn::AwaitingUser {
            reason: AwaitingReason::Question,
        });
        dead.lifecycle = Lifecycle::Exited { code: 1 };
        dead.ended_ms = Some(T0 + 9_000);
        dead.exit_code = Some(1);
        tree.insert(dead);

        let row = &TreeNodeView::flatten(&tree, T0 + 60_000)[0];
        assert_eq!(row.display_state, DisplayState::Failed);
        assert!(!row.needs_user, "a dead agent must not still be asking");
        assert_eq!(row.runtime_ms, 9_000, "runtime freezes at the exit");
    }

    #[test]
    fn a_deep_tree_flattens_in_draw_order() {
        let mut tree = SessionTree::new();
        let root = tree.insert(ProcessNode::agent(session(), "claude", "/", T0));

        let mut mid = ProcessNode::agent(session(), "reviewer", "/", T0);
        mid.kind = NodeKind::Subagent;
        mid.link_to(root.clone(), Relation::Confirmed);
        let mid_id = tree.insert(mid);

        let mut leaf = ProcessNode::process(session(), NodeKind::TestRunner, "cargo test", "/", T0);
        leaf.link_to(mid_id.clone(), Relation::Confirmed);
        let leaf_id = tree.insert(leaf);

        let mut sibling = ProcessNode::process(session(), NodeKind::Shell, "zsh", "/", T0);
        sibling.link_to(root.clone(), Relation::Confirmed);
        let sibling_id = tree.insert(sibling);

        let rows = TreeNodeView::flatten(&tree, T0);
        let order: Vec<(&NodeId, usize)> = rows.iter().map(|r| (&r.node_id, r.depth)).collect();
        assert_eq!(
            order,
            vec![(&root, 0), (&mid_id, 1), (&leaf_id, 2), (&sibling_id, 1)]
        );
    }

    #[test]
    fn an_empty_tree_flattens_to_no_rows() {
        assert!(TreeNodeView::flatten(&SessionTree::new(), T0).is_empty());
    }

    /// The store is trusted but not infallible. A cycle must not hang the daemon,
    /// and must not make a process disappear from the user's view either.
    #[test]
    fn a_cyclic_tree_from_a_corrupt_store_still_terminates_and_shows_everything() {
        let mut tree = SessionTree::new();
        let a = tree.insert(ProcessNode::process(
            session(),
            NodeKind::Shell,
            "a",
            "/",
            T0,
        ));
        let b = tree.insert(ProcessNode::process(
            session(),
            NodeKind::Shell,
            "b",
            "/",
            T0,
        ));
        // Hand-built cycle, bypassing `relink`'s refusal.
        tree.get_mut(&a).unwrap().parent = Some(b.clone());
        tree.get_mut(&a).unwrap().relation = Relation::Inferred;
        tree.get_mut(&b).unwrap().parent = Some(a.clone());
        tree.get_mut(&b).unwrap().relation = Relation::Inferred;

        let rows = TreeNodeView::flatten(&tree, T0);
        assert_eq!(rows.len(), 2, "both processes remain visible");
        let ids: HashSet<&NodeId> = rows.iter().map(|r| &r.node_id).collect();
        assert!(ids.contains(&a) && ids.contains(&b));
    }

    /// The other thing a corrupt store can hand over: not a cycle, but a chain.
    ///
    /// `SessionTree` accepts any parent pointer it is given, so a deep chain is a
    /// legal shape rather than a bug to fix upstream, and one stack frame per link
    /// would abort the daemon — losing every other session with it, not just the
    /// tree that could not be drawn.
    ///
    /// The walk runs on a thread with a deliberately small stack, so the guarantee
    /// is "deeper than the stack can hold" rather than "deeper than this machine's
    /// default happened to be", and the chain stays short enough that the test costs
    /// a fraction of a second.
    #[test]
    fn a_parent_chain_deeper_than_the_stack_flattens_instead_of_overflowing_it() {
        const DEPTH: usize = 2_000;
        const SMALL_STACK: usize = 128 * 1024;

        let mut tree = SessionTree::new();
        let mut parent = tree.insert(ProcessNode::process(
            session(),
            NodeKind::Shell,
            "link 0",
            "/",
            T0,
        ));
        for i in 1..DEPTH {
            let mut child =
                ProcessNode::process(session(), NodeKind::Shell, format!("link {i}"), "/", T0);
            child.link_to(parent.clone(), Relation::Inferred);
            parent = tree.insert(child);
        }

        let rows = std::thread::scope(|scope| {
            std::thread::Builder::new()
                .stack_size(SMALL_STACK)
                .spawn_scoped(scope, || TreeNodeView::flatten(&tree, T0))
                .expect("a thread to walk the chain on")
                .join()
                .expect("the walk must not take its thread down with it")
        });

        assert_eq!(rows.len(), DEPTH, "every link is still the user's process");
        assert_eq!(rows[0].depth, 0);
        assert_eq!(
            rows[DEPTH - 1].depth,
            DEPTH - 1,
            "and the indentation still describes the chain"
        );
    }

    #[test]
    fn a_tree_row_round_trips_through_json() {
        let (tree, _, _) = tree_with_confirmed_subagent();
        let rows = TreeNodeView::flatten(&tree, T0);
        let json = serde_json::to_string(&rows).unwrap();
        assert!(json.contains("\"kind\":\"spawned_by\""), "got {json}");
        assert!(json.contains("\"confidence\":\"explicit\""), "got {json}");
        assert!(json.contains("\"relationship_is_provisional\""));
        let back: Vec<TreeNodeView> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, rows);
    }

    #[test]
    fn hierarchy_rows_project_preview_bindings_and_runtime_capability_without_coupling_lifetimes() {
        let mut s = turn_core::model::Session::new(
            turn_core::ids::WorkspaceId::from_stored("ws_tree0001"),
            "Review",
            "/repo",
            turn_core::model::Layout::single(turn_core::model::Pane::new(
                turn_core::model::PaneKind::Agent,
            )),
            T0,
        );
        let mut reviewer = ProcessNode::agent(s.id.clone(), "reviewer", "/repo", T0);
        reviewer.activity_preview = Some(ActivityPreview {
            node_id: reviewer.id.clone(),
            raw_source_sequence: Some(7),
            normalized_text: "Reviewing auth.rs".into(),
            source: PreviewSource::SemanticEvent,
            confidence: Confidence::Explicit,
            stable: true,
            contains_sensitive_data: false,
            redacted: false,
            updated_ms: T0 + 10,
        });
        let node_id = s.tree.insert(reviewer);
        let binding = PaneNodeBinding {
            pane_id: PaneId::from_stored("pane_review"),
            session_id: s.id.clone(),
            node_id: node_id.clone(),
            temporary: true,
            surface_id: Some("window-a".into()),
            opened_ms: T0 + 20,
        };
        let capabilities = HashMap::from([(
            node_id.clone(),
            NodePaneCapability::Terminal {
                streams: vec![crate::PaneStream::Cells],
            },
        )]);

        let rows = TreeNodeView::for_session_with_panes(
            &s,
            std::slice::from_ref(&binding),
            &capabilities,
            T0 + 30,
        );

        assert_eq!(
            rows[0].activity_preview.as_ref().unwrap().normalized_text,
            "Reviewing auth.rs"
        );
        assert_eq!(rows[0].pane_bindings, vec![binding]);
        assert_eq!(rows[0].pane_capability, capabilities[&node_id]);
        assert!(rows[0].pane_bindings[0].temporary);
    }
}
