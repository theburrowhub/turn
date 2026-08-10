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

/// Where a moved pane lands relative to the pane it was dropped on.
///
/// Named in the domain rather than derived from a pixel offset in the window,
/// because the meaning of the five zones is what makes a relocation predictable:
/// the four edges make the moved pane a sibling on that side, and the middle
/// exchanges the two panes. Every tiling UI shares this vocabulary, so a user
/// needs none of it explained.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DropZone {
    Left,
    Right,
    Above,
    Below,
    /// Exchange the two panes in place, leaving the geometry alone.
    Centre,
}

impl DropZone {
    /// The orientation a sibling drop needs. `Centre` needs none: it moves nothing
    /// about the tree's shape, which is why it is the one zone that cannot fail to
    /// find a place to put the pane.
    fn direction(self) -> Option<Direction> {
        match self {
            DropZone::Left | DropZone::Right => Some(Direction::Horizontal),
            DropZone::Above | DropZone::Below => Some(Direction::Vertical),
            DropZone::Centre => None,
        }
    }

    /// Whether the moved pane takes the position after the target in its split.
    fn lands_after(self) -> bool {
        matches!(self, DropZone::Right | DropZone::Below)
    }
}

/// A closed set of safe rearrangements for panes that already exist.
///
/// Applying one changes geometry only: pane ids, process bindings and the focused
/// pane survive. That makes these suitable for a visible Layout menu without
/// turning a layout choice into implicit process control.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayoutPreset {
    /// Keep the current split tree and give siblings equal shares recursively.
    Balanced,
    Columns,
    Rows,
    MainLeft,
    Grid,
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
    /// Re-attach if the process survived; otherwise leave a consequential command stopped.
    /// Commandless terminal panes are the exception: they are always restored as the user's
    /// shell, because an empty terminal is not useful and opening a shell has no automated
    /// side effect against the checkout.
    #[default]
    ReattachOnly,
    /// Start the pane again when a window returns to the Session (a shell, an agent pane, a
    /// file browser). The daemon still does not launch it unattended at boot; the connected
    /// window performs the relaunch while the Session is present on screen.
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
        if !Self::insert_beside(&mut self.root, target, direction, new_pane, true) {
            return false;
        }
        self.active = Some(new_id);
        true
    }

    /// Puts a pane next to `target`, on the side `after` names.
    ///
    /// The insertion half of both [`Self::split`] and [`Self::relocate`], which
    /// differ only in where the pane came from. Sharing it is what keeps one rule
    /// about joining: when the target already sits in a split of this direction the
    /// pane becomes a sibling there instead of nesting a new two-way split, because
    /// nesting would stop the outer dividers lining up — and for relocation that
    /// matters twice over, since rearranging a layout repeatedly would otherwise turn
    /// a flat row into a staircase of nested splits.
    fn insert_beside(
        node: &mut LayoutNode,
        target: &PaneId,
        direction: Direction,
        pane: Pane,
        after: bool,
    ) -> bool {
        match node {
            LayoutNode::Leaf(existing) => {
                if &existing.id != target {
                    return false;
                }
                // Replace this leaf with a split holding the old and new panes.
                let existing = std::mem::replace(existing, Pane::new(PaneKind::Placeholder));
                let mut children = vec![
                    Child {
                        size: 0.5,
                        node: LayoutNode::Leaf(existing),
                    },
                    Child {
                        size: 0.5,
                        node: LayoutNode::Leaf(pane),
                    },
                ];
                if !after {
                    children.swap(0, 1);
                }
                *node = LayoutNode::Split(Split {
                    direction,
                    children,
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
                        let at = if after { index + 1 } else { index };
                        split.children.insert(
                            at,
                            Child {
                                size: share,
                                node: LayoutNode::Leaf(pane),
                            },
                        );
                        enforce_minimum_shares(split);
                        return true;
                    }
                }
                for child in split.children.iter_mut() {
                    // `pane` is moved on success, so recurse on a clone and only
                    // commit when the child reports it consumed it.
                    let candidate = pane.clone();
                    if Self::insert_beside(&mut child.node, target, direction, candidate, after) {
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
        if Self::detach_in(&mut self.root, target).is_none() {
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

    /// Lifts a pane out of the tree and hands it back, collapsing the split it
    /// leaves behind when that split is down to one child.
    ///
    /// The removal half of both [`Self::close`] and [`Self::relocate`]: closing drops
    /// the pane it is given, relocating puts it back somewhere else, and neither
    /// should have its own opinion about what happens to the space or to a split with
    /// nothing left to divide.
    fn detach_in(node: &mut LayoutNode, target: &PaneId) -> Option<Pane> {
        // The root being the pane itself means this is the last pane in the layout,
        // and a layout with nothing in it has nowhere to put the cursor.
        let LayoutNode::Split(split) = node else {
            return None;
        };

        if let Some(index) = split
            .children
            .iter()
            .position(|c| matches!(&c.node, LayoutNode::Leaf(p) if &p.id == target))
        {
            let removed = split.children.remove(index);
            let size = removed.size;
            let pane = match removed.node {
                LayoutNode::Leaf(pane) => pane,
                // Unreachable: the search above only matches a leaf. Putting the
                // child back rather than panicking means even a future change that
                // broke that correspondence could not lose a pane.
                node => {
                    split.children.insert(index, Child { size, node });
                    return None;
                }
            };
            // Give the freed space back proportionally.
            let remaining: f32 = split.children.iter().map(|c| c.size).sum();
            if remaining > 0.0 {
                for child in split.children.iter_mut() {
                    child.size += size * (child.size / remaining);
                }
            }
            // A split with one child is not a split.
            if split.children.len() == 1 {
                let only = split.children.remove(0);
                *node = only.node;
            }
            return Some(pane);
        }

        split
            .children
            .iter_mut()
            // The recursion may collapse a nested split into a leaf, which is why
            // this walks the children rather than the panes.
            .find_map(|child| Self::detach_in(&mut child.node, target))
    }

    /// Adjusts a pane's share of its split by `delta`, taking from its next
    /// sibling. Sizes stay clamped so a pane can never be resized to nothing.
    pub fn resize(&mut self, target: &PaneId, delta: f32) -> bool {
        Self::resize_in(&mut self.root, target, delta)
    }

    fn resize_in(node: &mut LayoutNode, target: &PaneId, delta: f32) -> bool {
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
            let applied = delta.min(other - MIN_SHARE).max(MIN_SHARE - current);
            split.children[index].size = current + applied;
            split.children[neighbour].size = other - applied;
            return true;
        }

        split
            .children
            .iter_mut()
            .any(|c| Self::resize_in(&mut c.node, target, delta))
    }

    /// Moves one exact divider, identified by panes on its two sides.
    ///
    /// A divider can separate two whole subtrees rather than two direct Pane
    /// children. Addressing it by only one Pane is therefore ambiguous: recursively
    /// looking up that Pane may resize a nested split instead of the divider the user
    /// actually dragged. The ordered pair disambiguates the boundary by locating the
    /// split whose adjacent children contain `before` and `after` respectively.
    ///
    /// `delta` is a fraction of that split. A positive value grows the child before
    /// the divider and shrinks the child after it. As with [`Self::resize`], both
    /// children retain a minimum visible share.
    pub fn resize_divider(&mut self, before: &PaneId, after: &PaneId, delta: f32) -> bool {
        Self::resize_divider_in(&mut self.root, before, after, delta)
    }

    fn resize_divider_in(
        node: &mut LayoutNode,
        before: &PaneId,
        after: &PaneId,
        delta: f32,
    ) -> bool {
        let LayoutNode::Split(split) = node else {
            return false;
        };

        if let Some(index) = divider_index(split, before, after) {
            let current = split.children[index].size;
            let other = split.children[index + 1].size;
            let applied = delta.min(other - MIN_SHARE).max(MIN_SHARE - current);
            split.children[index].size = current + applied;
            split.children[index + 1].size = other - applied;
            return true;
        }

        split
            .children
            .iter_mut()
            .any(|child| Self::resize_divider_in(&mut child.node, before, after, delta))
    }

    /// Gives every child of the split containing one exact divider an equal share.
    ///
    /// Equalising all siblings, rather than only the two panes named by the caller,
    /// makes a double-click predictable for a row or column with three or more panes.
    /// Nested layouts inside those siblings retain their own proportions.
    pub fn equalize_divider(&mut self, before: &PaneId, after: &PaneId) -> bool {
        Self::equalize_divider_in(&mut self.root, before, after)
    }

    fn equalize_divider_in(node: &mut LayoutNode, before: &PaneId, after: &PaneId) -> bool {
        let LayoutNode::Split(split) = node else {
            return false;
        };

        if divider_index(split, before, after).is_some() {
            let share = 1.0 / split.children.len() as f32;
            for child in &mut split.children {
                child.size = share;
            }
            return true;
        }

        split
            .children
            .iter_mut()
            .any(|child| Self::equalize_divider_in(&mut child.node, before, after))
    }

    /// Rearranges the panes that already exist into a predictable shape.
    ///
    /// No Pane is created or removed and every live `node_id` is carried over.
    /// The operation is therefore purely visual even for a Session with running
    /// Agents. `Balanced` is the one variant that retains the current tree.
    pub fn apply_preset(&mut self, preset: LayoutPreset) -> bool {
        if preset == LayoutPreset::Balanced {
            equalize_all(&mut self.root);
            self.zoomed = None;
            return true;
        }

        let panes: Vec<Pane> = self.panes().into_iter().cloned().collect();
        if panes.is_empty() {
            return false;
        }
        let active = self.active.clone();
        self.root = match preset {
            LayoutPreset::Balanced => unreachable!("handled above"),
            LayoutPreset::Columns => split_from_panes(Direction::Horizontal, panes),
            LayoutPreset::Rows => split_from_panes(Direction::Vertical, panes),
            LayoutPreset::MainLeft => main_left_from_panes(panes),
            LayoutPreset::Grid => grid_from_panes(panes),
        };
        self.active = active.filter(|id| self.get(id).is_some());
        if self.active.is_none() {
            self.active = self.panes().first().map(|pane| pane.id.clone());
        }
        self.zoomed = None;
        self.normalise();
        true
    }

    /// Moves an existing pane so that it sits beside another one.
    ///
    /// This is what a user means by moving a pane: the pane leaves where it was and
    /// arrives somewhere else, so a row can become a column and a pane can leave one
    /// split for another. Exchanging two panes in place is only one of the five
    /// outcomes ([`DropZone::Centre`]) and it was the only one Turn used to be able to
    /// express, which is why the layout's *shape* used to be fixed at creation.
    ///
    /// Nothing about the pane itself changes. Its id, its process binding, whether it
    /// is the active pane and whether it is zoomed all survive, because moving a view
    /// is not process control: the node behind the pane never learns it happened.
    pub fn relocate(&mut self, moved: &PaneId, target: &PaneId, zone: DropZone) -> bool {
        if moved == target || self.get(moved).is_none() || self.get(target).is_none() {
            return false;
        }
        let Some(direction) = zone.direction() else {
            return self.exchange(moved, target);
        };

        // The two steps run on a copy of the tree and are committed only once the
        // result is known to hold every pane it started with. A relocation that
        // dropped one would leave a live process with no pane to reach it through,
        // and making that impossible structurally is worth cloning a tree that holds
        // a handful of panes.
        let expected = self.pane_count();
        let mut candidate = self.root.clone();
        // Extract before inserting, and in that order for a reason: when the target
        // is the moved pane's only sibling, extracting collapses the split they
        // shared and the target — whose id stays valid throughout — becomes the root
        // leaf. Inserting into that leaf is what turns `A│B` into `B` above `A` as a
        // genuine vertical split, rather than the original horizontal one with its
        // two children reordered.
        let Some(pane) = Self::detach_in(&mut candidate, moved) else {
            return false;
        };
        if !Self::insert_beside(&mut candidate, target, direction, pane, zone.lands_after()) {
            return false;
        }
        if candidate.panes().len() != expected {
            return false;
        }
        self.root = candidate;
        true
    }

    /// Exchanges two panes, which is [`DropZone::Centre`] under the name a caller
    /// with nothing to relocate — a template editor rearranging a draft — asks for.
    ///
    /// An alias and not a second implementation: it delegates, so the two names
    /// cannot come to mean different things.
    pub fn swap(&mut self, a: &PaneId, b: &PaneId) -> bool {
        self.relocate(a, b, DropZone::Centre)
    }

    /// Exchanges two panes' positions, leaving the geometry alone.
    fn exchange(&mut self, a: &PaneId, b: &PaneId) -> bool {
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

/// The smallest share of its split a pane may hold.
///
/// A pane thinner than this is a pane the user cannot see, and an invisible pane is
/// a running process with no way back to it. One constant for resizing, dragging a
/// divider and arriving in a split, because it is one rule.
const MIN_SHARE: f32 = 0.05;

/// Lifts any child below [`MIN_SHARE`] back to it, taking the difference from the
/// children that have room to spare.
///
/// Arriving in a split shrinks its existing children proportionally to make room, so
/// a split that already held a barely visible pane would otherwise squeeze it out of
/// sight. Taking the difference proportionally rather than equalising everything
/// keeps the proportions the user chose between the panes that are not at the floor.
///
/// A split of more than twenty children cannot give each one `MIN_SHARE`; there the
/// honest floor is an equal share, which is what the `min` computes.
fn enforce_minimum_shares(split: &mut Split) {
    let count = split.children.len();
    if count == 0 {
        return;
    }
    let floor = MIN_SHARE.min(1.0 / count as f32);
    let deficit: f32 = split
        .children
        .iter()
        .map(|child| (floor - child.size).max(0.0))
        .sum();
    if deficit <= 0.0 {
        return;
    }
    let surplus: f32 = split
        .children
        .iter()
        .map(|child| (child.size - floor).max(0.0))
        .sum();
    if surplus <= 0.0 {
        return;
    }
    // Sibling sizes sum to 1 and `floor` is at most their average, so the surplus
    // always covers the deficit and the total is conserved. The `min` is there so
    // that an un-normalised split cannot be inflated past 1 by the lift below.
    let taken = deficit.min(surplus);
    for child in split.children.iter_mut() {
        if child.size > floor {
            child.size -= taken * ((child.size - floor) / surplus);
        }
    }
    for child in split.children.iter_mut() {
        if child.size < floor {
            child.size = floor;
        }
    }
}

fn equalize_all(node: &mut LayoutNode) {
    if let LayoutNode::Split(split) = node {
        let share = 1.0 / split.children.len().max(1) as f32;
        for child in &mut split.children {
            child.size = share;
            equalize_all(&mut child.node);
        }
    }
}

fn split_from_panes(direction: Direction, panes: Vec<Pane>) -> LayoutNode {
    if panes.len() == 1 {
        return LayoutNode::Leaf(panes.into_iter().next().expect("one Pane"));
    }
    let share = 1.0 / panes.len() as f32;
    LayoutNode::Split(Split {
        direction,
        children: panes
            .into_iter()
            .map(|pane| Child {
                size: share,
                node: LayoutNode::Leaf(pane),
            })
            .collect(),
    })
}

fn main_left_from_panes(mut panes: Vec<Pane>) -> LayoutNode {
    if panes.len() == 1 {
        return LayoutNode::Leaf(panes.remove(0));
    }
    let main = panes.remove(0);
    LayoutNode::Split(Split {
        direction: Direction::Horizontal,
        children: vec![
            Child {
                size: 0.62,
                node: LayoutNode::Leaf(main),
            },
            Child {
                size: 0.38,
                node: split_from_panes(Direction::Vertical, panes),
            },
        ],
    })
}

fn grid_from_panes(panes: Vec<Pane>) -> LayoutNode {
    if panes.len() <= 2 {
        return split_from_panes(Direction::Horizontal, panes);
    }
    let row_count = panes.len().div_ceil(2);
    let share = 1.0 / row_count as f32;
    let rows = panes
        .chunks(2)
        .map(|row| Child {
            size: share,
            node: split_from_panes(Direction::Horizontal, row.to_vec()),
        })
        .collect();
    LayoutNode::Split(Split {
        direction: Direction::Vertical,
        children: rows,
    })
}

/// Finds the ordered boundary between two adjacent children of one split.
///
/// Pane ids are unique within a Layout. Testing containment rather than requiring
/// direct leaves is what lets an outer divider address a child that is itself a
/// nested row or column.
fn divider_index(split: &Split, before: &PaneId, after: &PaneId) -> Option<usize> {
    split.children.windows(2).position(|pair| {
        contains_pane(&pair[0].node, before) && contains_pane(&pair[1].node, after)
    })
}

fn contains_pane(node: &LayoutNode, pane_id: &PaneId) -> bool {
    match node {
        LayoutNode::Leaf(pane) => &pane.id == pane_id,
        LayoutNode::Split(split) => split
            .children
            .iter()
            .any(|child| contains_pane(&child.node, pane_id)),
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
    fn resizing_an_outer_divider_does_not_resize_its_nested_child() {
        let mut l = layout();
        let upper_left = l.panes()[0].id.clone();
        let right = Pane::new(PaneKind::Shell);
        let right_id = right.id.clone();
        l.split(&upper_left, Direction::Horizontal, right);

        let lower_left = Pane::new(PaneKind::Logs);
        let lower_left_id = lower_left.id.clone();
        l.split(&upper_left, Direction::Vertical, lower_left);

        assert!(l.resize_divider(&lower_left_id, &right_id, 0.15));
        let LayoutNode::Split(root) = &l.root else {
            panic!("expected the outer horizontal split");
        };
        assert!((root.children[0].size - 0.65).abs() < 0.001);
        assert!((root.children[1].size - 0.35).abs() < 0.001);

        let LayoutNode::Split(nested) = &root.children[0].node else {
            panic!("expected the left child to remain a vertical split");
        };
        assert!(
            nested
                .children
                .iter()
                .all(|child| (child.size - 0.5).abs() < 0.001),
            "moving the outer divider must not disturb its nested row"
        );
        assert!(l.sizes_are_normalised());
    }

    #[test]
    fn equalizing_a_two_child_divider_restores_halves() {
        let mut l = layout();
        let left = l.panes()[0].id.clone();
        let right = Pane::new(PaneKind::Shell);
        let right_id = right.id.clone();
        l.split(&left, Direction::Horizontal, right);
        assert!(l.resize_divider(&left, &right_id, 0.2));

        assert!(l.equalize_divider(&left, &right_id));
        let LayoutNode::Split(split) = &l.root else {
            panic!("expected a split");
        };
        assert!(split
            .children
            .iter()
            .all(|child| (child.size - 0.5).abs() < 0.001));
        assert!(l.sizes_are_normalised());
    }

    #[test]
    fn equalizing_one_divider_gives_all_three_siblings_equal_shares() {
        let mut l = layout();
        let first = l.panes()[0].id.clone();
        let second = Pane::new(PaneKind::Shell);
        let second_id = second.id.clone();
        l.split(&first, Direction::Horizontal, second);
        let third = Pane::new(PaneKind::Logs);
        let third_id = third.id.clone();
        l.split(&second_id, Direction::Horizontal, third);
        assert!(l.resize_divider(&first, &second_id, 0.1));

        assert!(l.equalize_divider(&second_id, &third_id));
        let LayoutNode::Split(split) = &l.root else {
            panic!("expected one flat split");
        };
        assert_eq!(split.children.len(), 3);
        assert!(split
            .children
            .iter()
            .all(|child| { (child.size - 1.0 / 3.0).abs() < 0.001 }));
        assert!(l.sizes_are_normalised());
    }

    #[test]
    fn divider_operations_reject_non_adjacent_and_reversed_pairs_without_mutation() {
        let mut l = layout();
        let first = l.panes()[0].id.clone();
        let second = Pane::new(PaneKind::Shell);
        let second_id = second.id.clone();
        l.split(&first, Direction::Horizontal, second);
        let third = Pane::new(PaneKind::Logs);
        let third_id = third.id.clone();
        l.split(&second_id, Direction::Horizontal, third);
        let before = l.clone();

        assert!(!l.resize_divider(&first, &third_id, 0.1));
        assert!(!l.equalize_divider(&second_id, &first));
        assert_eq!(l, before);
    }

    #[test]
    fn layout_presets_rearrange_only_geometry_and_preserve_live_panes() {
        let mut l = layout();
        let first = l.panes()[0].id.clone();
        l.get_mut(&first).unwrap().node_id = Some(NodeId::from_stored("proc_layout_main"));
        l.split(&first, Direction::Horizontal, Pane::new(PaneKind::Shell));
        let second = l.active.clone().unwrap();
        l.split(&second, Direction::Vertical, Pane::new(PaneKind::Logs));
        let before: Vec<_> = l
            .panes()
            .into_iter()
            .map(|pane| (pane.id.clone(), pane.node_id.clone()))
            .collect();

        for preset in [
            LayoutPreset::Columns,
            LayoutPreset::Rows,
            LayoutPreset::MainLeft,
            LayoutPreset::Grid,
            LayoutPreset::Balanced,
        ] {
            assert!(l.apply_preset(preset));
            let after: Vec<_> = l
                .panes()
                .into_iter()
                .map(|pane| (pane.id.clone(), pane.node_id.clone()))
                .collect();
            assert_eq!(after, before, "{preset:?} changed Pane or Process identity");
            assert_eq!(l.pane_count(), 3);
            assert!(l.sizes_are_normalised());
            assert_eq!(l.zoomed, None);
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
    fn dropping_a_pane_on_the_centre_of_another_exchanges_their_positions() {
        let mut l = layout();
        let first = l.panes()[0].id.clone();
        let shell = Pane::new(PaneKind::Shell).with_command("zsh");
        let shell_id = shell.id.clone();
        l.split(&first, Direction::Horizontal, shell);

        let order_before: Vec<_> = l.panes().iter().map(|p| p.id.clone()).collect();
        assert!(l.relocate(&first, &shell_id, DropZone::Centre));
        let order_after: Vec<_> = l.panes().iter().map(|p| p.id.clone()).collect();

        assert_eq!(order_before[0], order_after[1]);
        assert_eq!(order_before[1], order_after[0]);
        assert_eq!(l.pane_count(), 2);
        // The exchange is in place, so the tree keeps its shape and its direction.
        match &l.root {
            LayoutNode::Split(split) => {
                assert_eq!(split.direction, Direction::Horizontal);
                assert_eq!(split.children.len(), 2);
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(l.sizes_are_normalised());
    }

    #[test]
    fn swapping_is_the_same_operation_as_a_centre_relocation() {
        let mut l = layout();
        let first = l.panes()[0].id.clone();
        l.split(&first, Direction::Horizontal, Pane::new(PaneKind::Shell));
        let second = l.active.clone().unwrap();
        l.split(&second, Direction::Vertical, Pane::new(PaneKind::Logs));
        let mut other = l.clone();

        assert!(l.swap(&first, &second));
        assert!(other.relocate(&first, &second, DropZone::Centre));
        assert_eq!(l, other, "the alias must not mean something of its own");

        // Including when it refuses.
        let ghost = PaneId::from_stored("pane_ghost0002");
        assert!(!l.swap(&first, &first));
        assert!(!l.relocate(&first, &first, DropZone::Centre));
        assert!(!l.swap(&first, &ghost));
        assert!(!l.relocate(&first, &ghost, DropZone::Centre));
    }

    /// The complaint this whole operation exists for: with `A│B` you could get
    /// `B│A` and never `A` stacked over `B`.
    #[test]
    fn relocating_a_pane_below_its_only_sibling_turns_a_row_into_a_column() {
        let mut l = layout();
        let a = l.panes()[0].id.clone();
        let b_pane = Pane::new(PaneKind::Shell);
        let b = b_pane.id.clone();
        l.split(&a, Direction::Horizontal, b_pane);

        assert!(l.relocate(&a, &b, DropZone::Below));

        let LayoutNode::Split(split) = &l.root else {
            panic!("expected a split at the root, got {:?}", l.root);
        };
        assert_eq!(
            split.direction,
            Direction::Vertical,
            "the orientation must actually change, not the child order"
        );
        assert_eq!(split.children.len(), 2);
        let order: Vec<_> = l.panes().iter().map(|p| p.id.clone()).collect();
        assert_eq!(order, vec![b.clone(), a.clone()], "B is above A");
        assert!(l.sizes_are_normalised());
    }

    #[test]
    fn relocating_a_pane_above_its_only_sibling_puts_it_first_in_a_column() {
        let mut l = layout();
        let a = l.panes()[0].id.clone();
        let b_pane = Pane::new(PaneKind::Shell);
        let b = b_pane.id.clone();
        l.split(&a, Direction::Horizontal, b_pane);

        assert!(l.relocate(&a, &b, DropZone::Above));

        let LayoutNode::Split(split) = &l.root else {
            panic!("expected a split at the root, got {:?}", l.root);
        };
        assert_eq!(split.direction, Direction::Vertical);
        let order: Vec<_> = l.panes().iter().map(|p| p.id.clone()).collect();
        assert_eq!(order, vec![a, b], "A is above B");
        assert!(l.sizes_are_normalised());
    }

    #[test]
    fn a_pane_relocated_onto_itself_or_onto_a_pane_that_does_not_exist_moves_nothing() {
        let mut l = layout();
        let first = l.panes()[0].id.clone();
        l.split(&first, Direction::Horizontal, Pane::new(PaneKind::Shell));
        let second = l.active.clone().unwrap();
        let ghost = PaneId::from_stored("pane_ghost0001");
        let before = l.clone();

        for zone in [
            DropZone::Left,
            DropZone::Right,
            DropZone::Above,
            DropZone::Below,
            DropZone::Centre,
        ] {
            assert!(!l.relocate(&first, &first, zone), "{zone:?} onto itself");
            assert!(!l.relocate(&first, &ghost, zone), "{zone:?} onto a ghost");
            assert!(!l.relocate(&ghost, &second, zone), "{zone:?} of a ghost");
            assert_eq!(l, before, "{zone:?} left the layout changed");
        }
    }

    #[test]
    fn the_only_pane_in_a_layout_can_never_relocate_itself_out_of_existence() {
        let mut l = layout();
        let only = l.panes()[0].id.clone();
        let before = l.clone();

        for zone in [
            DropZone::Left,
            DropZone::Right,
            DropZone::Above,
            DropZone::Below,
            DropZone::Centre,
        ] {
            assert!(!l.relocate(&only, &only, zone));
            assert_eq!(l.pane_count(), 1);
            assert_eq!(l, before);
        }
    }

    #[test]
    fn a_pane_relocated_out_of_a_nested_split_joins_the_row_at_the_root() {
        // Horizontal[ A, Vertical[ B, C ] ]
        let mut l = layout();
        let a = l.panes()[0].id.clone();
        let b_pane = Pane::new(PaneKind::Shell);
        let b = b_pane.id.clone();
        l.split(&a, Direction::Horizontal, b_pane);
        let c_pane = Pane::new(PaneKind::Logs);
        let c = c_pane.id.clone();
        l.split(&b, Direction::Vertical, c_pane);

        assert!(l.relocate(&c, &a, DropZone::Left));

        let LayoutNode::Split(split) = &l.root else {
            panic!("expected the root row, got {:?}", l.root);
        };
        assert_eq!(split.direction, Direction::Horizontal);
        assert_eq!(split.children.len(), 3, "C joined the root row");
        let order: Vec<_> = l.panes().iter().map(|p| p.id.clone()).collect();
        assert_eq!(order, vec![c, a, b], "C landed to the left of A");
        for child in &split.children {
            assert!(
                matches!(child.node, LayoutNode::Leaf(_)),
                "the column B and C shared is down to one child, so it collapsed"
            );
        }
        assert!(l.sizes_are_normalised());
    }

    #[test]
    fn a_pane_relocated_from_the_root_into_a_nested_split_becomes_a_sibling_there() {
        // Horizontal[ A, Vertical[ B, C ] ]
        let mut l = layout();
        let a = l.panes()[0].id.clone();
        let b_pane = Pane::new(PaneKind::Shell);
        let b = b_pane.id.clone();
        l.split(&a, Direction::Horizontal, b_pane);
        let c_pane = Pane::new(PaneKind::Logs);
        let c = c_pane.id.clone();
        l.split(&b, Direction::Vertical, c_pane);

        assert!(l.relocate(&a, &b, DropZone::Below));

        // The root row is down to one child, so it collapsed into the column.
        let LayoutNode::Split(split) = &l.root else {
            panic!("expected the column, got {:?}", l.root);
        };
        assert_eq!(split.direction, Direction::Vertical);
        assert_eq!(
            split.children.len(),
            3,
            "A joined the existing column instead of nesting a new split inside it"
        );
        let order: Vec<_> = l.panes().iter().map(|p| p.id.clone()).collect();
        assert_eq!(order, vec![b, a, c]);
        assert!(l.sizes_are_normalised());
    }

    /// Without this, rearranging a layout slowly turns a flat row into a staircase of
    /// nested two-way splits and the dividers stop lining up.
    #[test]
    fn relocating_into_a_split_of_the_same_direction_joins_it_rather_than_nesting() {
        // Horizontal[ A, B, C ] with D stacked under C.
        let mut l = layout();
        let a = l.panes()[0].id.clone();
        let b_pane = Pane::new(PaneKind::Shell);
        let b = b_pane.id.clone();
        l.split(&a, Direction::Horizontal, b_pane);
        let c_pane = Pane::new(PaneKind::Logs);
        let c = c_pane.id.clone();
        l.split(&b, Direction::Horizontal, c_pane);
        let d_pane = Pane::new(PaneKind::TestOutput);
        let d = d_pane.id.clone();
        l.split(&c, Direction::Vertical, d_pane);

        assert!(l.relocate(&d, &b, DropZone::Right));

        let LayoutNode::Split(split) = &l.root else {
            panic!("expected the root row, got {:?}", l.root);
        };
        assert_eq!(split.direction, Direction::Horizontal);
        assert_eq!(split.children.len(), 4, "one flat row of four, not a nest");
        for child in &split.children {
            assert!(
                matches!(child.node, LayoutNode::Leaf(_)),
                "a relocation nested a split where a sibling would do"
            );
            assert!((child.size - 0.25).abs() < 0.001, "got {}", child.size);
        }
        let order: Vec<_> = l.panes().iter().map(|p| p.id.clone()).collect();
        assert_eq!(order, vec![a, b, d, c]);
        assert!(l.sizes_are_normalised());
    }

    #[test]
    fn the_space_a_relocated_pane_vacates_goes_to_the_siblings_it_left_behind() {
        // Horizontal[ Vertical[ A, B ], C ], with the column taking two thirds.
        let mut l = layout();
        let a = l.panes()[0].id.clone();
        let c_pane = Pane::new(PaneKind::Logs);
        let c = c_pane.id.clone();
        l.split(&a, Direction::Horizontal, c_pane);
        let b_pane = Pane::new(PaneKind::Shell);
        let b = b_pane.id.clone();
        l.split(&a, Direction::Vertical, b_pane);
        assert!(l.resize_divider(&b, &c, 0.15));

        assert!(l.relocate(&b, &c, DropZone::Right));

        let LayoutNode::Split(split) = &l.root else {
            panic!("expected the root row, got {:?}", l.root);
        };
        assert_eq!(split.children.len(), 3);
        // A was B's only sibling in the column, so it inherited the whole column's
        // share rather than the space evaporating.
        let order: Vec<_> = l.panes().iter().map(|p| p.id.clone()).collect();
        assert_eq!(order, vec![a.clone(), c, b]);
        assert!((split.children[0].size - 0.65 * (2.0 / 3.0)).abs() < 0.01);
        assert!(l.sizes_are_normalised());
        for pane in l.panes() {
            let _ = pane;
        }
    }

    #[test]
    fn a_pane_squeezed_by_an_arriving_sibling_keeps_a_visible_sliver() {
        // A row where two panes are already at the minimum, plus a pane to move in.
        let thin = || Pane::new(PaneKind::Shell);
        let mut l = Layout {
            root: LayoutNode::Split(Split {
                direction: Direction::Vertical,
                children: vec![
                    Child {
                        size: 0.5,
                        node: LayoutNode::Split(Split {
                            direction: Direction::Horizontal,
                            children: vec![
                                Child {
                                    size: 0.9,
                                    node: LayoutNode::Leaf(agent_pane()),
                                },
                                Child {
                                    size: 0.05,
                                    node: LayoutNode::Leaf(thin()),
                                },
                                Child {
                                    size: 0.05,
                                    node: LayoutNode::Leaf(thin()),
                                },
                            ],
                        }),
                    },
                    Child {
                        size: 0.5,
                        node: LayoutNode::Leaf(Pane::new(PaneKind::Logs)),
                    },
                ],
            }),
            active: None,
            zoomed: None,
        };
        l.active = Some(l.panes()[0].id.clone());
        assert!(l.sizes_are_normalised());
        let wide = l.panes()[0].id.clone();
        let arriving = l.panes()[3].id.clone();

        assert!(l.relocate(&arriving, &wide, DropZone::Right));

        let LayoutNode::Split(split) = &l.root else {
            panic!("expected the row, got {:?}", l.root);
        };
        assert_eq!(split.children.len(), 4);
        for child in &split.children {
            assert!(
                child.size >= MIN_SHARE - 0.0001,
                "a pane was squeezed to {} by an arrival",
                child.size
            );
        }
        assert!(l.sizes_are_normalised());
    }

    #[test]
    fn relocating_a_pane_moves_neither_the_focus_nor_the_zoom() {
        let mut l = layout();
        let a = l.panes()[0].id.clone();
        let b_pane = Pane::new(PaneKind::Shell);
        let b = b_pane.id.clone();
        l.split(&a, Direction::Horizontal, b_pane);
        let c_pane = Pane::new(PaneKind::Logs);
        let c = c_pane.id.clone();
        l.split(&b, Direction::Horizontal, c_pane);
        l.focus(&c);
        l.toggle_zoom(&c);

        assert!(l.relocate(&a, &b, DropZone::Below));

        assert_eq!(
            l.active.as_ref(),
            Some(&c),
            "moving a pane must not change what has focus"
        );
        assert_eq!(l.zoomed.as_ref(), Some(&c));
        assert!(l.get(&c).is_some());
        assert!(l.sizes_are_normalised());
    }

    #[test]
    fn relocating_a_pane_carries_its_identity_and_its_process_with_it() {
        let mut l = layout();
        let a = l.panes()[0].id.clone();
        l.get_mut(&a).unwrap().node_id = Some(NodeId::from_stored("proc_relocated01"));
        let b_pane = Pane::new(PaneKind::Shell);
        let b = b_pane.id.clone();
        l.split(&a, Direction::Horizontal, b_pane);
        let before: Vec<_> = l
            .panes()
            .into_iter()
            .map(|pane| (pane.id.clone(), pane.kind, pane.node_id.clone()))
            .collect();

        assert!(l.relocate(&a, &b, DropZone::Below));

        let after: Vec<_> = l
            .panes()
            .into_iter()
            .map(|pane| (pane.id.clone(), pane.kind, pane.node_id.clone()))
            .collect();
        assert_eq!(after.len(), before.len());
        for entry in &before {
            assert!(
                after.contains(entry),
                "relocation changed a pane's identity or its process binding: {entry:?}"
            );
        }
        assert_eq!(
            l.get(&a).and_then(|pane| pane.node_id.clone()),
            Some(NodeId::from_stored("proc_relocated01"))
        );
    }

    #[test]
    fn every_relocation_of_every_pane_in_a_deep_layout_keeps_the_layout_valid() {
        // Vertical[ Horizontal[ A, B ], Horizontal[ C, Vertical[ D, E ] ] ]
        let build = || {
            let mut l = layout();
            let a = l.panes()[0].id.clone();
            l.split(&a, Direction::Vertical, Pane::new(PaneKind::Shell));
            let c = l.active.clone().unwrap();
            l.split(&a, Direction::Horizontal, Pane::new(PaneKind::Logs));
            l.split(&c, Direction::Horizontal, Pane::new(PaneKind::TestOutput));
            let e = l.active.clone().unwrap();
            l.split(&e, Direction::Vertical, Pane::new(PaneKind::Server));
            l
        };
        let original = build();
        assert_eq!(original.pane_count(), 5);
        let ids: Vec<PaneId> = original.panes().iter().map(|p| p.id.clone()).collect();

        for moved in &ids {
            for target in &ids {
                for zone in [
                    DropZone::Left,
                    DropZone::Right,
                    DropZone::Above,
                    DropZone::Below,
                    DropZone::Centre,
                ] {
                    let mut l = original.clone();
                    let applied = l.relocate(moved, target, zone);
                    assert_eq!(
                        applied,
                        moved != target,
                        "relocating {moved} onto {target} {zone:?}"
                    );
                    assert_eq!(
                        l.pane_count(),
                        5,
                        "relocating {moved} onto {target} {zone:?} lost or duplicated a pane"
                    );
                    let mut seen: Vec<PaneId> =
                        l.panes().iter().map(|pane| pane.id.clone()).collect();
                    seen.sort();
                    let mut expected = ids.clone();
                    expected.sort();
                    assert_eq!(seen, expected);
                    assert!(
                        l.sizes_are_normalised(),
                        "relocating {moved} onto {target} {zone:?} left sizes that do not sum to 1"
                    );
                    assert_smallest_share_is_visible(&l.root);
                    assert_eq!(l.active, original.active);
                }
            }
        }
    }

    /// No split anywhere in the tree holds a pane too thin to be seen.
    fn assert_smallest_share_is_visible(node: &LayoutNode) {
        if let LayoutNode::Split(split) = node {
            let floor = MIN_SHARE.min(1.0 / split.children.len() as f32);
            for child in &split.children {
                assert!(
                    child.size >= floor - 0.0001,
                    "a child holds {} of its split",
                    child.size
                );
                assert_smallest_share_is_visible(&child.node);
            }
        }
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
