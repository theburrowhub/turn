//! Process and agent nodes, and the tree they form inside a session.

use crate::event::AgentRef;
use crate::event::Confidence;
use crate::ids::{NodeId, SessionId};
use crate::model::hierarchy::{
    ActivityPreview, AgentName, NameSource, PreviewVisibility, Relationship, RelationshipKind,
};
use crate::state::{DisplayState, Lifecycle, Turn};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// What kind of thing a node is. Drives the icon and the default pane type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    /// A top-level coding agent.
    Agent,
    /// An agent spawned by another agent.
    Subagent,
    Shell,
    /// A terminal we have no opinion about.
    Terminal,
    /// A full-screen terminal UI (lazygit, btop, fang).
    Tui,
    Server,
    Watcher,
    TestRunner,
    Build,
    Background,
    TmuxSession,
    TmuxPane,
    /// Seen in the process table, purpose unknown.
    Unknown,
}

impl NodeKind {
    /// Whether this kind carries the agent turn axis.
    pub fn is_agentic(&self) -> bool {
        matches!(self, NodeKind::Agent | NodeKind::Subagent)
    }
}

/// How sure we are that a node's parent really is its parent.
///
/// The brief is explicit that Turn must not invent relationships. A hook that
/// says "I spawned a subagent" is [`Relation::Confirmed`]; a pid whose ppid
/// happens to match is [`Relation::Inferred`]; anything else stays
/// [`Relation::Unknown`] and renders at the session root rather than under a
/// guessed parent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Relation {
    /// The tool told us.
    Confirmed,
    /// Derived from the process table or the pty hierarchy.
    Inferred,
    /// No parent could be established.
    Unknown,
}

impl Relation {
    /// Whether the UI should mark the edge as a guess.
    pub fn is_provisional(&self) -> bool {
        !matches!(self, Relation::Confirmed)
    }
}

/// Agent-specific detail, present only on agentic nodes.
///
/// Not `Eq`: `cost_usd` is a float. Comparing agent info for exact equality is
/// not something the domain needs anyway.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentInfo {
    pub agent: AgentRef,
    #[serde(default)]
    pub name: AgentName,
    /// The agent's own conversation/thread id, used to resume it.
    pub external_id: Option<String>,
    /// Subagent type reported by the tool ("Explore", "code-reviewer").
    pub agent_type: Option<String>,
    pub current_task: Option<String>,
    pub last_message: Option<String>,
    /// A pending permission the user has not answered.
    pub pending_permission: Option<PendingPermission>,
    pub pending_question: Option<String>,
    pub tokens_used: Option<u64>,
    pub cost_usd: Option<f64>,
    pub permission_mode: Option<String>,
    pub git_branch: Option<String>,
    /// Whether this agent can be resumed after its process ends.
    pub resumable: bool,
}

/// A permission the agent is blocked on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingPermission {
    pub summary: String,
    pub command: Option<String>,
    pub tool_name: Option<String>,
    pub risk: crate::event::Risk,
    pub requested_ms: i64,
    /// The directory the command would run in. Shown verbatim so the user can
    /// see they are about to approve something in the wrong repo.
    pub cwd: Option<String>,
}

/// Any process Turn knows about.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessNode {
    pub id: NodeId,
    pub session_id: SessionId,
    pub kind: NodeKind,
    pub title: String,
    pub command: String,
    pub args: Vec<String>,
    pub cwd: String,
    pub pid: Option<u32>,
    pub ppid: Option<u32>,
    pub lifecycle: Lifecycle,
    /// Present only for agentic nodes. `None` here is what makes
    /// [`DisplayState::derive`] treat a shell as a plain process.
    pub turn: Option<Turn>,
    pub agent: Option<AgentInfo>,
    pub parent: Option<NodeId>,
    pub relation: Relation,
    /// Typed edge plus the five-level confidence ladder. `relation` remains the
    /// compact legacy projection used by old event paths; this is authoritative.
    #[serde(default)]
    pub relationship: Relationship,
    #[serde(default)]
    pub activity_preview: Option<ActivityPreview>,
    #[serde(default)]
    pub preview_visibility: PreviewVisibility,
    /// The title the process set for itself, already sanitised by `turn-pty`.
    ///
    /// Lives on every node rather than only on agents because everything that runs
    /// in a terminal sets titles: a shell through `PROMPT_COMMAND`, `vim` on every
    /// file it opens, `ssh` on connect. Agents route theirs through
    /// [`AgentName::apply_process_title`] so it competes with declared names;
    /// non-agent nodes have no declared name, so this is simply the better title
    /// when it exists.
    #[serde(default)]
    pub process_title: Option<String>,
    /// A name the user typed for this node, which nothing else may override.
    #[serde(default)]
    pub user_title: Option<String>,
    pub started_ms: i64,
    pub ended_ms: Option<i64>,
    pub exit_code: Option<i32>,
    /// Environment entries worth surfacing. Never the whole environment: that
    /// leaks secrets into the UI and the store.
    pub env_highlights: HashMap<String, String>,
    /// Set when the node is waiting on the user for anything.
    pub interaction_pending: bool,
}

impl ProcessNode {
    /// A node for a non-agent process.
    pub fn process(
        session_id: SessionId,
        kind: NodeKind,
        command: impl Into<String>,
        cwd: impl Into<String>,
        started_ms: i64,
    ) -> Self {
        let command = command.into();
        Self {
            id: NodeId::new(),
            session_id,
            kind,
            title: command.clone(),
            command,
            args: Vec::new(),
            cwd: cwd.into(),
            pid: None,
            ppid: None,
            lifecycle: Lifecycle::Spawning,
            turn: None,
            agent: None,
            parent: None,
            relation: Relation::Unknown,
            relationship: Relationship::default(),
            activity_preview: None,
            preview_visibility: PreviewVisibility::Inherit,
            process_title: None,
            user_title: None,
            started_ms,
            ended_ms: None,
            exit_code: None,
            env_highlights: HashMap::new(),
            interaction_pending: false,
        }
    }

    /// A node for an agent, which gains the turn axis.
    pub fn agent(
        session_id: SessionId,
        command: impl Into<String>,
        cwd: impl Into<String>,
        started_ms: i64,
    ) -> Self {
        let mut node = Self::process(session_id, NodeKind::Agent, command, cwd, started_ms);
        node.turn = Some(Turn::Idle);
        let info = AgentInfo {
            name: AgentName::fallback(node.title.clone()),
            ..AgentInfo::default()
        };
        node.agent = Some(info);
        node
    }

    /// The flattened state for display.
    pub fn display_state(&self) -> DisplayState {
        DisplayState::derive(&self.lifecycle, self.turn.as_ref())
    }

    /// The title to show, and where it came from.
    ///
    /// One function so no caller can apply a different precedence: a name the user
    /// typed, then whatever an agent declared about itself, then the title the
    /// process wrote, then the command it was launched with. The source travels
    /// with the text because a title read off a terminal must never be presented
    /// with the authority of one a tool reported — no amount of sanitising makes
    /// `✓ tests passed` a fact.
    pub fn resolved_title(&self) -> (String, NameSource) {
        if let Some(user) = &self.user_title {
            return (user.clone(), NameSource::ExplicitParentEvent);
        }
        if let Some(agent) = &self.agent {
            let name = &agent.name;
            if name.user_renamed || !name.display_name.is_empty() {
                // The agent's own naming already ran the precedence, including any
                // process title it accepted.
                if !name.display_name.is_empty() {
                    return (name.display_name.clone(), name.source);
                }
            }
        }
        if let Some(title) = &self.process_title {
            return (title.clone(), NameSource::ProcessTitle);
        }
        (self.title.clone(), NameSource::Fallback)
    }

    /// Records a title the process set for itself, and reports whether anything
    /// visible changed.
    ///
    /// Returning a bool is what lets the daemon avoid pushing an update per
    /// keystroke: a shell that rewrites an identical title on every prompt is
    /// common, and each accepted change would otherwise become traffic.
    pub fn set_process_title(&mut self, title: impl Into<String>) -> bool {
        let title = title.into();
        if title.is_empty() {
            return false;
        }
        if self.process_title.as_deref() == Some(title.as_str()) {
            return false;
        }
        self.process_title = Some(title.clone());
        match &mut self.agent {
            Some(agent) => agent.name.apply_process_title(title),
            // A shell or a TUI has no declared name to lose, so the title stands
            // unless the user named it.
            None => self.user_title.is_none(),
        }
    }

    /// Forgets the process title, so a dead process stops announcing what it was
    /// doing. Reports whether anything visible changed.
    pub fn clear_process_title(&mut self) -> bool {
        let had = self.process_title.take().is_some();
        let fallback = self.title.clone();
        match &mut self.agent {
            Some(agent) => agent.name.clear_process_title(fallback) || had,
            None => had,
        }
    }

    /// Attaches this node under a parent, recording how sure we are.
    pub fn link_to(&mut self, parent: NodeId, relation: Relation) {
        self.parent = Some(parent);
        self.relation = relation;
        self.relationship = match relation {
            Relation::Confirmed => Relationship {
                kind: RelationshipKind::SpawnedBy,
                confidence: Confidence::Explicit,
            },
            Relation::Inferred => Relationship {
                kind: RelationshipKind::SpawnedBy,
                confidence: Confidence::InferredHigh,
            },
            Relation::Unknown => Relationship::default(),
        };
    }

    pub fn is_running(&self) -> bool {
        self.lifecycle.is_running()
    }

    /// How long the node has been alive, or how long it lived.
    pub fn runtime_ms(&self, now_ms: i64) -> i64 {
        self.ended_ms.unwrap_or(now_ms) - self.started_ms
    }
}

/// The node hierarchy of one session.
///
/// Stored flat with parent pointers rather than as nested structs: processes
/// arrive out of order (a child's hook can land before the parent's spawn
/// notification) and re-parenting a flat map is trivial where re-parenting a
/// tree is not.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionTree {
    nodes: HashMap<NodeId, ProcessNode>,
    /// Insertion order, so the tree renders stably instead of hash order.
    order: Vec<NodeId>,
}

impl SessionTree {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, node: ProcessNode) -> NodeId {
        let id = node.id.clone();
        if !self.nodes.contains_key(&id) {
            self.order.push(id.clone());
        }
        self.nodes.insert(id.clone(), node);
        id
    }

    pub fn get(&self, id: &NodeId) -> Option<&ProcessNode> {
        self.nodes.get(id)
    }

    pub fn get_mut(&mut self, id: &NodeId) -> Option<&mut ProcessNode> {
        self.nodes.get_mut(id)
    }

    pub fn remove(&mut self, id: &NodeId) -> Option<ProcessNode> {
        self.order.retain(|n| n != id);
        // Children of a removed node do not vanish; they become roots, marked as
        // having lost their parent rather than silently re-attached elsewhere.
        for node in self.nodes.values_mut() {
            if node.parent.as_ref() == Some(id) {
                node.parent = None;
                node.relation = Relation::Unknown;
                node.relationship = Relationship::default();
            }
        }
        self.nodes.remove(id)
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// All nodes in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = &ProcessNode> {
        self.order.iter().filter_map(|id| self.nodes.get(id))
    }

    /// Nodes with no parent, which render at the top level of the tree.
    pub fn roots(&self) -> Vec<&ProcessNode> {
        self.iter().filter(|n| n.parent.is_none()).collect()
    }

    /// Direct children of a node, in insertion order.
    pub fn children(&self, parent: &NodeId) -> Vec<&ProcessNode> {
        self.iter()
            .filter(|n| n.parent.as_ref() == Some(parent))
            .collect()
    }

    /// Every descendant, depth-first.
    pub fn descendants(&self, parent: &NodeId) -> Vec<&ProcessNode> {
        let mut out = Vec::new();
        let mut stack: Vec<NodeId> = self
            .children(parent)
            .into_iter()
            .map(|n| n.id.clone())
            .rev()
            .collect();
        while let Some(id) = stack.pop() {
            if let Some(node) = self.nodes.get(&id) {
                out.push(node);
                for child in self.children(&id).into_iter().rev() {
                    stack.push(child.id.clone());
                }
            }
        }
        out
    }

    /// Finds a node by OS pid. Used to attach a process we spotted in the
    /// process table to one we already track.
    pub fn find_by_pid(&self, pid: u32) -> Option<&ProcessNode> {
        self.iter().find(|n| n.pid == Some(pid))
    }

    /// Finds an agent node by the tool's own session/thread id, which is how
    /// hook callbacks identify themselves.
    pub fn find_by_external_id(&self, external_id: &str) -> Option<&ProcessNode> {
        self.iter().find(|n| {
            n.agent
                .as_ref()
                .and_then(|a| a.external_id.as_deref())
                .is_some_and(|id| id == external_id)
        })
    }

    pub fn find_by_external_id_mut(&mut self, external_id: &str) -> Option<&mut ProcessNode> {
        let id = self.find_by_external_id(external_id)?.id.clone();
        self.nodes.get_mut(&id)
    }

    /// The session's primary agent: the first agentic root.
    pub fn primary_agent(&self) -> Option<&ProcessNode> {
        self.iter()
            .find(|n| n.kind == NodeKind::Agent && n.parent.is_none())
            .or_else(|| self.iter().find(|n| n.kind.is_agentic()))
    }

    pub fn subagent_count(&self) -> usize {
        self.iter().filter(|n| n.kind == NodeKind::Subagent).count()
    }

    pub fn running_count(&self) -> usize {
        self.iter().filter(|n| n.is_running()).count()
    }

    /// Processes that outlived the daemon that started them.
    ///
    /// Counted separately from `running_count` because the difference is what the user can
    /// do about them: a running process can be stopped by ending its Session, and one of
    /// these cannot be stopped by Turn at all. Any surface offering a destructive act has
    /// to say so before the user commits to it, which means the count has to travel with
    /// the summary rather than being recomputed from a tree the window may not hold.
    pub fn orphaned_count(&self) -> usize {
        self.iter()
            .filter(|n| n.lifecycle == Lifecycle::Orphaned)
            .count()
    }

    /// The session's aggregate state: the most severe of its running nodes.
    ///
    /// Severity rather than recency, because a session with one failure and nine
    /// happy processes needs to read as failed.
    pub fn aggregate_state(&self) -> DisplayState {
        self.iter()
            .map(|n| n.display_state())
            .max_by_key(|s| s.severity())
            .unwrap_or(DisplayState::Unknown)
    }

    /// Whether any node is blocked on the user.
    pub fn needs_user(&self) -> bool {
        self.iter().any(|n| n.display_state().demands_user())
    }

    /// Re-parents a node once a better-quality relation arrives.
    ///
    /// Confirmed links overwrite inferred ones, never the other way round: once
    /// a tool has told us the truth, a coincidence in the process table must not
    /// undo it.
    pub fn relink(&mut self, child: &NodeId, parent: NodeId, relation: Relation) -> bool {
        let Some(node) = self.nodes.get_mut(child) else {
            return false;
        };
        if node.relation == Relation::Confirmed && relation != Relation::Confirmed {
            return false;
        }
        // Refuse to create a cycle.
        if self.would_cycle(child, &parent) {
            return false;
        }
        let Some(node) = self.nodes.get_mut(child) else {
            return false;
        };
        node.link_to(parent, relation);
        true
    }

    /// Whether making `parent` the parent of `child` would create a loop.
    fn would_cycle(&self, child: &NodeId, parent: &NodeId) -> bool {
        if child == parent {
            return true;
        }
        let mut cursor = Some(parent.clone());
        let mut hops = 0;
        while let Some(id) = cursor {
            if &id == child {
                return true;
            }
            // Defensive bound in case the store ever hands us a corrupt tree.
            hops += 1;
            if hops > 1_000 {
                return true;
            }
            cursor = self.nodes.get(&id).and_then(|n| n.parent.clone());
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::AwaitingReason;

    const T0: i64 = 1_700_000_000_000;

    fn tree_with_agent() -> (SessionTree, NodeId) {
        let session = SessionId::from_stored("sess_a");
        let mut tree = SessionTree::new();
        let agent = ProcessNode::agent(session, "claude", "/repo", T0);
        let id = tree.insert(agent);
        (tree, id)
    }

    /// The issue's headline case: two instances of the same agent must be tellable
    /// apart, and neither may affect the other.
    #[test]
    fn two_instances_of_one_agent_keep_independent_titles() {
        let session = SessionId::from_stored("sess_a");
        let mut first = ProcessNode::agent(session.clone(), "claude", "/repo", T0);
        let mut second = ProcessNode::agent(session, "claude", "/repo", T0);

        assert!(first.set_process_title("fixing the climbing bug"));
        assert!(second.set_process_title("reviewing PR #104"));

        assert_eq!(first.resolved_title().0, "fixing the climbing bug");
        assert_eq!(second.resolved_title().0, "reviewing PR #104");

        // Changing one leaves the other alone.
        assert!(first.set_process_title("running the tests"));
        assert_eq!(second.resolved_title().0, "reviewing PR #104");
    }

    /// A name someone typed is the one thing a process may not overwrite. A shell
    /// rewriting its title on every prompt would otherwise erase it.
    #[test]
    fn a_title_the_user_typed_is_never_overwritten_by_the_process() {
        let session = SessionId::from_stored("sess_a");
        let mut node = ProcessNode::agent(session, "claude", "/repo", T0);
        node.user_title = Some("Auth refactor".into());

        node.set_process_title("something the agent printed");
        let (title, source) = node.resolved_title();
        assert_eq!(title, "Auth refactor");
        assert!(!source.is_provisional(), "a typed name is not a guess");
    }

    /// A subagent whose parent declared it "Reviewer" keeps that name: a hook
    /// payload is a contract, a title is free text the process writes.
    #[test]
    fn a_declared_name_outranks_whatever_the_process_prints() {
        let session = SessionId::from_stored("sess_a");
        let mut node = ProcessNode::agent(session, "claude", "/repo", T0);
        node.agent.as_mut().unwrap().name = AgentName::declared("Reviewer");

        let changed = node.set_process_title("bash-3.2");
        assert!(!changed, "nothing visible should change");
        assert_eq!(node.resolved_title().0, "Reviewer");
        // The title is still recorded, so the inspector can show what it said.
        assert_eq!(node.process_title.as_deref(), Some("bash-3.2"));
    }

    /// Shells and TUIs have no declared name, so their title simply is the better
    /// one. This is why the field lives on the node and not only on agents.
    #[test]
    fn a_shell_takes_its_process_title_because_it_has_no_declared_name() {
        let session = SessionId::from_stored("sess_a");
        let mut node = ProcessNode::process(session, NodeKind::Shell, "zsh", "/repo", T0);
        assert_eq!(node.resolved_title().0, "zsh");

        assert!(node.set_process_title("~/space-troopers"));
        let (title, source) = node.resolved_title();
        assert_eq!(title, "~/space-troopers");
        assert_eq!(source, NameSource::ProcessTitle);
        assert!(source.is_provisional(), "read off a terminal, not reported");
    }

    /// A repeated identical title changes nothing, which is what keeps a
    /// `PROMPT_COMMAND` firing on every command from becoming a push per command.
    #[test]
    fn an_unchanged_title_reports_no_change_so_it_produces_no_traffic() {
        let session = SessionId::from_stored("sess_a");
        let mut node = ProcessNode::process(session, NodeKind::Shell, "zsh", "/", T0);
        assert!(node.set_process_title("~/repo"));
        assert!(!node.set_process_title("~/repo"), "identical, so no change");
        assert!(!node.set_process_title(""), "empty is not a title");
    }

    /// A dead process must stop announcing what it was doing.
    #[test]
    fn a_finished_process_stops_advertising_its_last_title() {
        let session = SessionId::from_stored("sess_a");
        let mut shell = ProcessNode::process(session.clone(), NodeKind::Shell, "zsh", "/", T0);
        shell.set_process_title("compiling…");
        assert!(shell.clear_process_title());
        assert_eq!(shell.resolved_title().0, "zsh");

        // An agent falls back to what its parent declared, not to the command.
        let mut sub = ProcessNode::agent(session, "claude", "/", T0);
        sub.agent.as_mut().unwrap().name = AgentName::declared("Reviewer");
        sub.process_title = Some("stale".into());
        sub.agent
            .as_mut()
            .unwrap()
            .name
            .apply_process_title("stale");
        sub.clear_process_title();
        assert_eq!(sub.resolved_title().0, "Reviewer");
    }

    /// Precedence is a total order and it is asserted as one, so adding a source
    /// cannot silently re-rank the others.
    #[test]
    fn what_a_tool_reports_always_outranks_what_a_terminal_prints() {
        assert!(NameSource::ExplicitParentEvent.outranks(NameSource::Integration));
        assert!(NameSource::Integration.outranks(NameSource::StructuredTask));
        assert!(NameSource::StructuredTask.outranks(NameSource::ProcessTitle));
        assert!(NameSource::ProcessTitle.outranks(NameSource::Inferred));
        assert!(NameSource::Inferred.outranks(NameSource::Fallback));

        // And only the bottom three are shown as guesses.
        assert!(!NameSource::ExplicitParentEvent.is_provisional());
        assert!(!NameSource::Integration.is_provisional());
        assert!(!NameSource::StructuredTask.is_provisional());
        assert!(NameSource::ProcessTitle.is_provisional());
    }

    /// A title never destroys the declared name, even while it is on screen.
    #[test]
    fn a_process_title_never_erases_what_the_parent_declared() {
        let mut name = AgentName::declared("Reviewer");
        name.source = NameSource::Inferred; // weaker than ProcessTitle
        assert!(name.apply_process_title("whatever the process says"));
        assert_eq!(name.display_name, "whatever the process says");
        assert_eq!(
            name.declared_name.as_deref(),
            Some("Reviewer"),
            "the declaration survives underneath"
        );
        // And clearing restores it rather than the fallback.
        name.clear_process_title("zsh");
        assert_eq!(name.display_name, "Reviewer");
    }

    #[test]
    fn an_agent_gets_the_turn_axis_and_a_shell_does_not() {
        let session = SessionId::from_stored("sess_a");
        let agent = ProcessNode::agent(session.clone(), "claude", "/repo", T0);
        assert!(agent.turn.is_some());
        assert!(agent.agent.is_some());

        let shell = ProcessNode::process(session, NodeKind::Shell, "zsh", "/repo", T0);
        assert!(shell.turn.is_none(), "a shell owes the user nothing");
        assert_eq!(shell.display_state(), DisplayState::Starting);
    }

    #[test]
    fn subagents_hang_off_their_parent_with_a_confirmed_link() {
        let (mut tree, parent) = tree_with_agent();
        let session = SessionId::from_stored("sess_a");
        let mut sub = ProcessNode::agent(session, "claude-sub", "/repo", T0);
        sub.kind = NodeKind::Subagent;
        sub.link_to(parent.clone(), Relation::Confirmed);
        let sub_id = tree.insert(sub);

        assert_eq!(tree.children(&parent).len(), 1);
        assert_eq!(tree.roots().len(), 1, "the subagent is not a root");
        assert_eq!(tree.subagent_count(), 1);
        assert!(!tree.get(&sub_id).unwrap().relation.is_provisional());
    }

    #[test]
    fn an_unattributable_process_stays_at_the_root_rather_than_being_guessed_under_a_parent() {
        let (mut tree, _parent) = tree_with_agent();
        let session = SessionId::from_stored("sess_a");
        let orphan = ProcessNode::process(session, NodeKind::Unknown, "mystery", "/", T0);
        let id = tree.insert(orphan);

        assert_eq!(tree.get(&id).unwrap().relation, Relation::Unknown);
        assert_eq!(tree.roots().len(), 2, "it is visible, just not parented");
    }

    #[test]
    fn a_confirmed_link_is_not_overwritten_by_an_inferred_one() {
        let (mut tree, real_parent) = tree_with_agent();
        let session = SessionId::from_stored("sess_a");
        let other = tree.insert(ProcessNode::process(
            session.clone(),
            NodeKind::Shell,
            "zsh",
            "/",
            T0,
        ));
        let mut sub = ProcessNode::agent(session, "sub", "/", T0);
        sub.kind = NodeKind::Subagent;
        sub.link_to(real_parent.clone(), Relation::Confirmed);
        let sub_id = tree.insert(sub);

        assert!(
            !tree.relink(&sub_id, other, Relation::Inferred),
            "a coincidence in the process table must not override the truth"
        );
        assert_eq!(tree.get(&sub_id).unwrap().parent, Some(real_parent));
    }

    #[test]
    fn an_inferred_link_is_upgraded_when_the_tool_confirms_it() {
        let (mut tree, parent) = tree_with_agent();
        let session = SessionId::from_stored("sess_a");
        let mut child = ProcessNode::process(session, NodeKind::Unknown, "node", "/", T0);
        child.link_to(parent.clone(), Relation::Inferred);
        let child_id = tree.insert(child);

        assert!(tree.relink(&child_id, parent, Relation::Confirmed));
        assert_eq!(tree.get(&child_id).unwrap().relation, Relation::Confirmed);
    }

    #[test]
    fn relinking_refuses_to_build_a_cycle() {
        let (mut tree, root) = tree_with_agent();
        let session = SessionId::from_stored("sess_a");
        let mut child = ProcessNode::process(session, NodeKind::Shell, "zsh", "/", T0);
        child.link_to(root.clone(), Relation::Confirmed);
        let child_id = tree.insert(child);

        assert!(!tree.relink(&root, child_id, Relation::Confirmed));
        assert!(!tree.relink(&root, root.clone(), Relation::Confirmed));
    }

    #[test]
    fn removing_a_parent_promotes_its_children_instead_of_deleting_them() {
        let (mut tree, parent) = tree_with_agent();
        let session = SessionId::from_stored("sess_a");
        let mut child = ProcessNode::process(session, NodeKind::TestRunner, "npm test", "/", T0);
        child.link_to(parent.clone(), Relation::Confirmed);
        let child_id = tree.insert(child);

        tree.remove(&parent);
        assert_eq!(tree.len(), 1);
        let orphan = tree.get(&child_id).unwrap();
        assert!(orphan.parent.is_none());
        assert_eq!(orphan.relation, Relation::Unknown);
    }

    #[test]
    fn descendants_walks_the_whole_subtree() {
        let (mut tree, root) = tree_with_agent();
        let session = SessionId::from_stored("sess_a");
        let mut mid = ProcessNode::agent(session.clone(), "reviewer", "/", T0);
        mid.kind = NodeKind::Subagent;
        mid.link_to(root.clone(), Relation::Confirmed);
        let mid_id = tree.insert(mid);

        let mut leaf = ProcessNode::process(session, NodeKind::TestRunner, "cargo test", "/", T0);
        leaf.link_to(mid_id.clone(), Relation::Confirmed);
        tree.insert(leaf);

        assert_eq!(tree.descendants(&root).len(), 2);
        assert_eq!(tree.descendants(&mid_id).len(), 1);
    }

    #[test]
    fn the_aggregate_state_is_the_most_severe_not_the_most_recent() {
        let (mut tree, agent_id) = tree_with_agent();
        let session = SessionId::from_stored("sess_a");

        // The agent is quietly running.
        tree.get_mut(&agent_id).unwrap().lifecycle = Lifecycle::Alive;
        tree.get_mut(&agent_id).unwrap().turn = Some(Turn::Active);

        // A child failed.
        let mut failed = ProcessNode::process(session, NodeKind::TestRunner, "npm test", "/", T0);
        failed.lifecycle = Lifecycle::Exited { code: 1 };
        tree.insert(failed);

        assert_eq!(tree.aggregate_state(), DisplayState::Failed);
    }

    #[test]
    fn needs_user_is_true_when_any_node_is_blocked() {
        let (mut tree, agent_id) = tree_with_agent();
        assert!(!tree.needs_user());
        let node = tree.get_mut(&agent_id).unwrap();
        node.lifecycle = Lifecycle::Alive;
        node.turn = Some(Turn::AwaitingUser {
            reason: AwaitingReason::Permission,
        });
        assert!(tree.needs_user());
        assert_eq!(tree.aggregate_state(), DisplayState::NeedsPermission);
    }

    #[test]
    fn nodes_are_findable_by_pid_and_by_the_tools_own_session_id() {
        let (mut tree, agent_id) = tree_with_agent();
        {
            let node = tree.get_mut(&agent_id).unwrap();
            node.pid = Some(4242);
            node.agent.as_mut().unwrap().external_id = Some("claude-abc123".into());
        }
        assert_eq!(tree.find_by_pid(4242).unwrap().id, agent_id);
        assert_eq!(
            tree.find_by_external_id("claude-abc123").unwrap().id,
            agent_id
        );
        assert!(tree.find_by_pid(1).is_none());
    }

    #[test]
    fn iteration_order_is_stable_across_calls() {
        let session = SessionId::from_stored("sess_a");
        let mut tree = SessionTree::new();
        let ids: Vec<_> = (0..10)
            .map(|i| {
                tree.insert(ProcessNode::process(
                    session.clone(),
                    NodeKind::Shell,
                    format!("cmd{i}"),
                    "/",
                    T0,
                ))
            })
            .collect();
        let first: Vec<_> = tree.iter().map(|n| n.id.clone()).collect();
        let second: Vec<_> = tree.iter().map(|n| n.id.clone()).collect();
        assert_eq!(first, ids);
        assert_eq!(first, second);
    }

    #[test]
    fn the_primary_agent_is_the_agentic_root() {
        let (mut tree, agent_id) = tree_with_agent();
        let session = SessionId::from_stored("sess_a");
        tree.insert(ProcessNode::process(
            session,
            NodeKind::Shell,
            "zsh",
            "/",
            T0,
        ));
        assert_eq!(tree.primary_agent().unwrap().id, agent_id);
    }
}
