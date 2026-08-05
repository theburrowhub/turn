//! Panes and the layout tree.
//!
//! A layout is a binary-ish tree of splits with fractional sizes. Splits hold a
//! list of children rather than exactly two, so three side-by-side panes are one
//! split with three children instead of a lopsided nest — which is what makes
//! "resize" behave the way a user expects.

use crate::ids::{NodeId, PaneId};
use serde::{Deserialize, Serialize};

/// What a pane shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaneKind {
    Terminal,
    Agent,
    Shell,
    /// A full-screen terminal application.
    Tui,
    Logs,
    TestOutput,
    Server,
    /// Turn's own views, which have no process behind them.
    EventLog,
    AgentTree,
    ProcessDetails,
    Preview,
    TmuxTerminal,
    /// Reserved for integrations that do not exist yet. Present so a template
    /// written today does not fail to load tomorrow.
    Placeholder,
}

impl PaneKind {
    /// Whether this pane type is backed by a pty.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            PaneKind::Terminal
                | PaneKind::Agent
                | PaneKind::Shell
                | PaneKind::Tui
                | PaneKind::Logs
                | PaneKind::TestOutput
                | PaneKind::Server
                | PaneKind::TmuxTerminal
        )
    }
}

/// Split orientation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Horizontal,
    Vertical,
}

/// One visual pane.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pane {
    pub id: PaneId,
    pub kind: PaneKind,
    pub title: Option<String>,
    /// The command to run when this pane is materialised.
    pub command: Option<String>,
    pub args: Vec<String>,
    /// Working directory, absolute or relative to the session's cwd.
    pub cwd: Option<String>,
    pub env: Vec<(String, String)>,
    /// The process this pane is showing, once it exists.
    pub node_id: Option<NodeId>,
    /// Whether to bring the process back on restore.
    pub restore: RestoreBehaviour,
}

/// What happens to a pane's process when a session is restored.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestoreBehaviour {
    /// Re-attach if the process survived; otherwise leave the pane empty and
    /// offer a button. The safe default: never re-runs anything by itself.
    #[default]
    ReattachOnly,
    /// Eligible to offer for an explicit relaunch (a shell, a file browser).
    /// The restore path never treats this metadata as launch authority.
    Relaunch,
    /// Do not restore at all.
    Skip,
}

impl Pane {
    pub fn new(kind: PaneKind) -> Self {
        Self {
            id: PaneId::new(),
            kind,
            title: None,
            command: None,
            args: Vec::new(),
            cwd: None,
            env: Vec::new(),
            node_id: None,
            restore: RestoreBehaviour::default(),
        }
    }

    pub fn with_command(mut self, command: impl Into<String>) -> Self {
        self.command = Some(command.into());
        self
    }

    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    pub fn with_cwd(mut self, cwd: impl Into<String>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    pub fn with_restore(mut self, restore: RestoreBehaviour) -> Self {
        self.restore = restore;
        self
    }
}

/// A child of a split: a node plus the fraction of the split it occupies.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Child {
    /// Fraction of the parent's extent, in `0.0..=1.0`. Siblings sum to 1.
    pub size: f32,
    pub node: LayoutNode,
}

/// A split container.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Split {
    pub direction: Direction,
    pub children: Vec<Child>,
}

/// A node in the layout tree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LayoutNode {
    Leaf(Pane),
    Split(Split),
}

impl LayoutNode {
    /// Every pane in this subtree, left to right, top to bottom.
    pub fn panes(&self) -> Vec<&Pane> {
        match self {
            LayoutNode::Leaf(pane) => vec![pane],
            LayoutNode::Split(split) => {
                split.children.iter().flat_map(|c| c.node.panes()).collect()
            }
        }
    }

    fn find_mut(&mut self, id: &PaneId) -> Option<&mut Pane> {
        match self {
            LayoutNode::Leaf(pane) if &pane.id == id => Some(pane),
            LayoutNode::Leaf(_) => None,
            LayoutNode::Split(split) => split.children.iter_mut().find_map(|c| c.node.find_mut(id)),
        }
    }
}

/// The pane arrangement of a session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Layout {
    pub root: LayoutNode,
    /// The pane with keyboard focus.
    pub active: Option<PaneId>,
    /// A pane temporarily filling the session. The tree is untouched, so
    /// un-zooming restores the exact previous geometry.
    pub zoomed: Option<PaneId>,
}

impl Layout {
    /// A layout with a single pane.
    pub fn single(pane: Pane) -> Self {
        let id = pane.id.clone();
        Self {
            root: LayoutNode::Leaf(pane),
            active: Some(id),
            zoomed: None,
        }
    }

    pub fn panes(&self) -> Vec<&Pane> {
        self.root.panes()
    }

    pub fn pane_count(&self) -> usize {
        self.panes().len()
    }

    pub fn get(&self, id: &PaneId) -> Option<&Pane> {
        self.panes().into_iter().find(|p| &p.id == id)
    }

    pub fn get_mut(&mut self, id: &PaneId) -> Option<&mut Pane> {
        self.root.find_mut(id)
    }

    /// Splits `target`, inserting `new_pane` next to it.
    ///
    /// When the target already sits in a split of the same direction, the new
    /// pane joins that split as a sibling rather than nesting a new one. Nesting
    /// there would make the divider between the outer panes stop lining up.
    pub fn split(&mut self, target: &PaneId, direction: Direction, new_pane: Pane) -> bool {
        let new_id = new_pane.id.clone();
        if !Self::split_in(&mut self.root, target, direction, new_pane) {
            return false;
        }
        self.active = Some(new_id);
        true
    }

    fn split_in(
        node: &mut LayoutNode,
        target: &PaneId,
        direction: Direction,
        new_pane: Pane,
    ) -> bool {
        match node {
            LayoutNode::Leaf(pane) => {
                if &pane.id != target {
                    return false;
                }
                // Replace this leaf with a split holding the old and new panes.
                let existing = std::mem::replace(pane, Pane::new(PaneKind::Placeholder));
                *node = LayoutNode::Split(Split {
                    direction,
                    children: vec![
                        Child {
                            size: 0.5,
                            node: LayoutNode::Leaf(existing),
                        },
                        Child {
                            size: 0.5,
                            node: LayoutNode::Leaf(new_pane),
                        },
                    ],
                });
                true
            }
            LayoutNode::Split(split) => {
                // Same direction and the target is a direct child: join as a sibling.
                if split.direction == direction {
                    let position = split
                        .children
                        .iter()
                        .position(|c| matches!(&c.node, LayoutNode::Leaf(p) if &p.id == target));
                    if let Some(index) = position {
                        let count = split.children.len() + 1;
                        let share = 1.0 / count as f32;
                        // Shrink everyone proportionally to make room.
                        for child in split.children.iter_mut() {
                            child.size *= 1.0 - share;
                        }
                        split.children.insert(
                            index + 1,
                            Child {
                                size: share,
                                node: LayoutNode::Leaf(new_pane),
                            },
                        );
                        return true;
                    }
                }
                for child in split.children.iter_mut() {
                    // `new_pane` is moved on success, so recurse on a clone and
                    // only commit when the child reports it consumed it.
                    let candidate = new_pane.clone();
                    if Self::split_in(&mut child.node, target, direction, candidate) {
                        return true;
                    }
                }
                false
            }
        }
    }

    /// Removes a pane, collapsing any split left with a single child.
    ///
    /// Returns false for the last remaining pane: a session always has at least
    /// one pane, and an empty layout has nowhere to put the cursor.
    pub fn close(&mut self, target: &PaneId) -> bool {
        if self.pane_count() <= 1 {
            return false;
        }
        if !Self::close_in(&mut self.root, target) {
            return false;
        }
        if self.zoomed.as_ref() == Some(target) {
            self.zoomed = None;
        }
        if self.active.as_ref() == Some(target) {
            self.active = self.panes().first().map(|p| p.id.clone());
        }
        true
    }

    fn close_in(node: &mut LayoutNode, target: &PaneId) -> bool {
        let LayoutNode::Split(split) = node else {
            return false;
        };

        if let Some(index) = split
            .children
            .iter()
            .position(|c| matches!(&c.node, LayoutNode::Leaf(p) if &p.id == target))
        {
            let removed = split.children.remove(index);
            // Give the freed space back proportionally.
            let remaining: f32 = split.children.iter().map(|c| c.size).sum();
            if remaining > 0.0 {
                for child in split.children.iter_mut() {
                    child.size += removed.size * (child.size / remaining);
                }
            }
            // A split with one child is not a split.
            if split.children.len() == 1 {
                let only = split.children.remove(0);
                *node = only.node;
            }
            return true;
        }

        for child in split.children.iter_mut() {
            if Self::close_in(&mut child.node, target) {
                // The recursion may have collapsed a nested split into a leaf.
                return true;
            }
        }
        false
    }

    /// Adjusts a pane's share of its split by `delta`, taking from its next
    /// sibling. Sizes stay clamped so a pane can never be resized to nothing.
    pub fn resize(&mut self, target: &PaneId, delta: f32) -> bool {
        Self::resize_in(&mut self.root, target, delta)
    }

    fn resize_in(node: &mut LayoutNode, target: &PaneId, delta: f32) -> bool {
        const MIN_SIZE: f32 = 0.05;
        let LayoutNode::Split(split) = node else {
            return false;
        };

        if let Some(index) = split
            .children
            .iter()
            .position(|c| matches!(&c.node, LayoutNode::Leaf(p) if &p.id == target))
        {
            // Borrow from the following sibling, or the preceding one at the end.
            let neighbour = if index + 1 < split.children.len() {
                index + 1
            } else if index > 0 {
                index - 1
            } else {
                return false;
            };
            let current = split.children[index].size;
            let other = split.children[neighbour].size;
            let applied = delta.min(other - MIN_SIZE).max(MIN_SIZE - current);
            split.children[index].size = current + applied;
            split.children[neighbour].size = other - applied;
            return true;
        }

        split
            .children
            .iter_mut()
            .any(|c| Self::resize_in(&mut c.node, target, delta))
    }

    /// Exchanges two panes' positions, leaving the geometry alone.
    pub fn swap(&mut self, a: &PaneId, b: &PaneId) -> bool {
        if a == b || self.get(a).is_none() || self.get(b).is_none() {
            return false;
        }
        // Park `a` behind a sentinel first. Writing `b`'s pane straight into
        // `a`'s slot would make the next lookup for `b` find that fresh copy
        // instead of the original, and the swap would quietly undo itself.
        let sentinel = Pane::new(PaneKind::Placeholder);
        let sentinel_id = sentinel.id.clone();

        let Some(slot_a) = self.root.find_mut(a) else {
            return false;
        };
        let pane_a = std::mem::replace(slot_a, sentinel);

        let Some(slot_b) = self.root.find_mut(b) else {
            return false;
        };
        let pane_b = std::mem::replace(slot_b, pane_a);

        let Some(slot_sentinel) = self.root.find_mut(&sentinel_id) else {
            return false;
        };
        *slot_sentinel = pane_b;
        true
    }

    /// Fills the session with one pane, or restores the previous geometry.
    pub fn toggle_zoom(&mut self, target: &PaneId) -> bool {
        if self.get(target).is_none() {
            return false;
        }
        self.zoomed = match &self.zoomed {
            Some(current) if current == target => None,
            _ => Some(target.clone()),
        };
        true
    }

    pub fn focus(&mut self, target: &PaneId) -> bool {
        if self.get(target).is_none() {
            return false;
        }
        self.active = Some(target.clone());
        true
    }

    /// Moves focus to the next pane, wrapping. Drives the cycle-panes shortcut.
    pub fn focus_next(&mut self) -> Option<PaneId> {
        let panes: Vec<PaneId> = self.panes().iter().map(|p| p.id.clone()).collect();
        if panes.is_empty() {
            return None;
        }
        let index = self
            .active
            .as_ref()
            .and_then(|a| panes.iter().position(|p| p == a))
            .map(|i| (i + 1) % panes.len())
            .unwrap_or(0);
        self.active = Some(panes[index].clone());
        self.active.clone()
    }

    pub fn focus_previous(&mut self) -> Option<PaneId> {
        let panes: Vec<PaneId> = self.panes().iter().map(|p| p.id.clone()).collect();
        if panes.is_empty() {
            return None;
        }
        let index = self
            .active
            .as_ref()
            .and_then(|a| panes.iter().position(|p| p == a))
            .map(|i| (i + panes.len() - 1) % panes.len())
            .unwrap_or(0);
        self.active = Some(panes[index].clone());
        self.active.clone()
    }

    /// A copy of this layout with the same shape but brand-new pane identities.
    ///
    /// Pane ids are what the daemon keys client attachments and pty ownership by,
    /// so anything that produces a *second* layout from an existing one — copying
    /// a session, instantiating a template — must mint new ones. Two layouts
    /// sharing a pane id is not a cosmetic problem: attaching to one would take
    /// over the other.
    ///
    /// Process bindings are dropped for the same reason: a copy owns no processes.
    pub fn reidentified(&self) -> Layout {
        let mut copy = self.clone();
        reassign_pane_ids(&mut copy.root);
        copy.active = copy.root.panes().first().map(|p| p.id.clone());
        copy.zoomed = None;
        copy
    }

    /// Whether sibling sizes still sum to 1 everywhere. A structural invariant
    /// worth asserting in tests rather than discovering as a rendering glitch.
    pub fn sizes_are_normalised(&self) -> bool {
        fn check(node: &LayoutNode) -> bool {
            match node {
                LayoutNode::Leaf(_) => true,
                LayoutNode::Split(split) => {
                    let total: f32 = split.children.iter().map(|c| c.size).sum();
                    (total - 1.0).abs() < 0.001 && split.children.iter().all(|c| check(&c.node))
                }
            }
        }
        check(&self.root)
    }

    /// Forces sibling sizes back to summing to 1. Used after loading a layout
    /// from disk, where hand-edited templates may not add up.
    pub fn normalise(&mut self) {
        fn fix(node: &mut LayoutNode) {
            if let LayoutNode::Split(split) = node {
                let total: f32 = split.children.iter().map(|c| c.size).sum();
                if total > 0.0 {
                    for child in split.children.iter_mut() {
                        child.size /= total;
                    }
                } else {
                    let share = 1.0 / split.children.len() as f32;
                    for child in split.children.iter_mut() {
                        child.size = share;
                    }
                }
                for child in split.children.iter_mut() {
                    fix(&mut child.node);
                }
            }
        }
        fix(&mut self.root);
    }
}

/// Gives every pane in a subtree a fresh id and clears its process binding.
fn reassign_pane_ids(node: &mut LayoutNode) {
    match node {
        LayoutNode::Leaf(pane) => {
            pane.id = PaneId::new();
            pane.node_id = None;
        }
        LayoutNode::Split(split) => {
            for child in split.children.iter_mut() {
                reassign_pane_ids(&mut child.node);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent_pane() -> Pane {
        Pane::new(PaneKind::Agent).with_command("claude")
    }

    fn layout() -> Layout {
        Layout::single(agent_pane())
    }

    #[test]
    fn a_new_layout_has_one_focused_pane() {
        let l = layout();
        assert_eq!(l.pane_count(), 1);
        assert_eq!(l.active, Some(l.panes()[0].id.clone()));
        assert!(l.sizes_are_normalised());
    }

    #[test]
    fn splitting_creates_two_even_panes_and_focuses_the_new_one() {
        let mut l = layout();
        let first = l.panes()[0].id.clone();
        let shell = Pane::new(PaneKind::Shell).with_command("zsh");
        let shell_id = shell.id.clone();

        assert!(l.split(&first, Direction::Horizontal, shell));
        assert_eq!(l.pane_count(), 2);
        assert_eq!(l.active, Some(shell_id));
        assert!(l.sizes_are_normalised());
    }

    #[test]
    fn splitting_the_same_direction_three_times_gives_three_equal_siblings() {
        let mut l = layout();
        let first = l.panes()[0].id.clone();
        l.split(&first, Direction::Horizontal, Pane::new(PaneKind::Shell));
        let second = l.active.clone().unwrap();
        l.split(&second, Direction::Horizontal, Pane::new(PaneKind::Logs));

        assert_eq!(l.pane_count(), 3);
        // One flat split of three, not a nest of two-way splits.
        match &l.root {
            LayoutNode::Split(split) => {
                assert_eq!(split.children.len(), 3, "should be one flat split");
                for child in &split.children {
                    assert!((child.size - 1.0 / 3.0).abs() < 0.01, "got {}", child.size);
                }
            }
            other => panic!("expected a split at the root, got {other:?}"),
        }
        assert!(l.sizes_are_normalised());
    }

    #[test]
    fn splitting_the_other_direction_nests() {
        let mut l = layout();
        let first = l.panes()[0].id.clone();
        l.split(&first, Direction::Horizontal, Pane::new(PaneKind::Shell));
        let second = l.active.clone().unwrap();
        l.split(&second, Direction::Vertical, Pane::new(PaneKind::Tui));

        assert_eq!(l.pane_count(), 3);
        match &l.root {
            LayoutNode::Split(split) => {
                assert_eq!(split.direction, Direction::Horizontal);
                assert_eq!(split.children.len(), 2, "the vertical split nested inside");
            }
            other => panic!("unexpected root {other:?}"),
        }
        assert!(l.sizes_are_normalised());
    }

    #[test]
    fn closing_a_pane_returns_its_space_and_collapses_the_split() {
        let mut l = layout();
        let first = l.panes()[0].id.clone();
        let shell = Pane::new(PaneKind::Shell);
        let shell_id = shell.id.clone();
        l.split(&first, Direction::Horizontal, shell);

        assert!(l.close(&shell_id));
        assert_eq!(l.pane_count(), 1);
        assert!(
            matches!(&l.root, LayoutNode::Leaf(p) if p.id == first),
            "a split with one child collapses back to a leaf"
        );
        assert!(l.sizes_are_normalised());
    }

    #[test]
    fn the_last_pane_cannot_be_closed() {
        let mut l = layout();
        let only = l.panes()[0].id.clone();
        assert!(!l.close(&only));
        assert_eq!(l.pane_count(), 1);
    }

    #[test]
    fn closing_the_focused_pane_moves_focus_somewhere_valid() {
        let mut l = layout();
        let first = l.panes()[0].id.clone();
        let shell = Pane::new(PaneKind::Shell);
        let shell_id = shell.id.clone();
        l.split(&first, Direction::Horizontal, shell);
        assert_eq!(l.active, Some(shell_id.clone()));

        l.close(&shell_id);
        assert_eq!(l.active, Some(first));
        assert!(l.get(l.active.as_ref().unwrap()).is_some());
    }

    #[test]
    fn closing_a_zoomed_pane_clears_the_zoom() {
        let mut l = layout();
        let first = l.panes()[0].id.clone();
        let shell = Pane::new(PaneKind::Shell);
        let shell_id = shell.id.clone();
        l.split(&first, Direction::Horizontal, shell);
        l.toggle_zoom(&shell_id);
        assert_eq!(l.zoomed, Some(shell_id.clone()));

        l.close(&shell_id);
        assert_eq!(l.zoomed, None);
    }

    #[test]
    fn resizing_moves_space_between_siblings_and_keeps_the_total() {
        let mut l = layout();
        let first = l.panes()[0].id.clone();
        l.split(&first, Direction::Horizontal, Pane::new(PaneKind::Shell));

        assert!(l.resize(&first, 0.15));
        assert!(l.sizes_are_normalised(), "resize must conserve the total");
        match &l.root {
            LayoutNode::Split(split) => {
                assert!((split.children[0].size - 0.65).abs() < 0.001);
                assert!((split.children[1].size - 0.35).abs() < 0.001);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn a_pane_cannot_be_resized_out_of_existence() {
        let mut l = layout();
        let first = l.panes()[0].id.clone();
        l.split(&first, Direction::Horizontal, Pane::new(PaneKind::Shell));

        l.resize(&first, 10.0);
        assert!(l.sizes_are_normalised());
        for pane in l.panes() {
            let _ = pane;
        }
        match &l.root {
            LayoutNode::Split(split) => {
                for child in &split.children {
                    assert!(child.size >= 0.049, "pane collapsed to {}", child.size);
                }
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn zoom_toggles_without_disturbing_the_tree() {
        let mut l = layout();
        let first = l.panes()[0].id.clone();
        l.split(&first, Direction::Horizontal, Pane::new(PaneKind::Shell));
        let before = l.root.clone();

        assert!(l.toggle_zoom(&first));
        assert_eq!(l.zoomed, Some(first.clone()));
        assert_eq!(l.root, before, "zoom is a view state, not a layout change");

        assert!(l.toggle_zoom(&first));
        assert_eq!(l.zoomed, None);
    }

    #[test]
    fn swapping_two_panes_exchanges_their_positions() {
        let mut l = layout();
        let first = l.panes()[0].id.clone();
        let shell = Pane::new(PaneKind::Shell).with_command("zsh");
        let shell_id = shell.id.clone();
        l.split(&first, Direction::Horizontal, shell);

        let order_before: Vec<_> = l.panes().iter().map(|p| p.id.clone()).collect();
        assert!(l.swap(&first, &shell_id));
        let order_after: Vec<_> = l.panes().iter().map(|p| p.id.clone()).collect();

        assert_eq!(order_before[0], order_after[1]);
        assert_eq!(order_before[1], order_after[0]);
        assert_eq!(l.pane_count(), 2);
    }

    #[test]
    fn cycling_focus_visits_every_pane_and_wraps() {
        let mut l = layout();
        let first = l.panes()[0].id.clone();
        l.split(&first, Direction::Horizontal, Pane::new(PaneKind::Shell));
        let second = l.active.clone().unwrap();
        l.split(&second, Direction::Vertical, Pane::new(PaneKind::Logs));

        l.focus(&first);
        let mut seen = vec![first.clone()];
        for _ in 0..2 {
            seen.push(l.focus_next().unwrap());
        }
        assert_eq!(seen.len(), 3);
        assert_eq!(l.focus_next().unwrap(), first, "cycling wraps");

        // And backwards.
        l.focus(&first);
        assert_eq!(l.focus_previous().unwrap(), seen[2]);
    }

    #[test]
    fn operations_on_an_unknown_pane_fail_instead_of_corrupting_the_tree() {
        let mut l = layout();
        let ghost = PaneId::from_stored("pane_ghost0001");
        let before = l.clone();

        assert!(!l.split(&ghost, Direction::Horizontal, Pane::new(PaneKind::Shell)));
        assert!(!l.close(&ghost));
        assert!(!l.resize(&ghost, 0.1));
        assert!(!l.toggle_zoom(&ghost));
        assert!(!l.focus(&ghost));
        assert_eq!(l, before);
    }

    #[test]
    fn a_hand_edited_layout_with_bad_sizes_is_normalised_on_load() {
        let mut l = Layout {
            root: LayoutNode::Split(Split {
                direction: Direction::Horizontal,
                children: vec![
                    Child {
                        size: 3.0,
                        node: LayoutNode::Leaf(agent_pane()),
                    },
                    Child {
                        size: 1.0,
                        node: LayoutNode::Leaf(Pane::new(PaneKind::Shell)),
                    },
                ],
            }),
            active: None,
            zoomed: None,
        };
        assert!(!l.sizes_are_normalised());
        l.normalise();
        assert!(l.sizes_are_normalised());
        match &l.root {
            LayoutNode::Split(split) => assert!((split.children[0].size - 0.75).abs() < 0.001),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn a_deep_layout_round_trips_through_json() {
        let mut l = layout();
        let first = l.panes()[0].id.clone();
        l.split(&first, Direction::Horizontal, Pane::new(PaneKind::Shell));
        let second = l.active.clone().unwrap();
        l.split(&second, Direction::Vertical, Pane::new(PaneKind::Tui));

        let json = serde_json::to_string(&l).unwrap();
        let back: Layout = serde_json::from_str(&json).unwrap();
        assert_eq!(l, back);
        assert_eq!(back.pane_count(), 3);
    }
}
