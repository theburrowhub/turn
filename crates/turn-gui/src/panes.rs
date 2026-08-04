//! Turning the daemon's layout tree into rectangles.
//!
//! The daemon owns the arrangement: a split is a direction plus children with
//! fractional sizes, and every pane operation answers with the resulting layout so
//! the window renders what the daemon decided rather than its own guess. What the
//! daemon cannot do is arithmetic in pixels — it has none — so this module is the
//! one place that turns fractions into rectangles, and it is pure so the awkward
//! cases can be tested without a window.
//!
//! Three things it has to get right:
//!
//! * **Dividers take space.** A split of three children has two dividers, and the
//!   fractions apply to what is left after they are subtracted. Distributing first
//!   and then drawing dividers on top is what makes the last pane in a row a few
//!   pixels narrower than it should be, every time, cumulatively.
//! * **A drag is a fraction, not a distance.** `resize_pane` takes a fraction of the
//!   parent split, so a divider dragged twenty points has to be divided by the
//!   extent of the split it lives in — which is not the width of the window once
//!   splits are nested.
//! * **Directional navigation is geometric.** "The pane to the left" is a question
//!   about rectangles, not about tree order, and a user with a three-pane layout
//!   will notice immediately if it is answered from the tree.

use egui::{Pos2, Rect, Vec2};
use turn_core::ids::{NodeId, PaneId};
use turn_core::model::{Direction, Layout, LayoutNode, PaneKind};

/// The thickness of a divider, in points.
///
/// Wide enough to grab without being a visible gutter. The hit area is widened
/// separately — see [`Divider::grab_rect`] — because a four-point target is a
/// frustrating thing to aim at with a trackpad.
pub const DIVIDER_THICKNESS: f32 = 4.0;

/// How much wider than the divider its pointer target is.
pub const DIVIDER_GRAB_MARGIN: f32 = 3.0;

/// The smallest a pane may be drawn before it is left out entirely.
///
/// A pane thinner than this cannot show even one column of text, and painting it
/// would cost a draw call to produce a sliver.
pub const MIN_PANE_EXTENT: f32 = 12.0;

/// One pane, placed.
#[derive(Debug, Clone, PartialEq)]
pub struct PaneRect {
    pub pane_id: PaneId,
    pub kind: PaneKind,
    /// The process behind it, when there is one. Absent for an empty slot after a
    /// partial restore, or for one of Turn's own views.
    pub node_id: Option<NodeId>,
    pub title: Option<String>,
    pub rect: Rect,
}

impl PaneRect {
    /// Whether this pane is backed by a pty and therefore paints a grid.
    pub fn is_terminal(&self) -> bool {
        self.kind.is_terminal()
    }
}

/// A draggable boundary between two panes.
#[derive(Debug, Clone, PartialEq)]
pub struct Divider {
    /// Which way the split runs. `Horizontal` means the panes are side by side and
    /// the divider is vertical.
    pub direction: Direction,
    pub rect: Rect,
    /// The pane on the left, or above. Dragging towards the other pane grows this
    /// one, which is the pane the resize request names.
    pub before: PaneId,
    /// The pane on the right, or below.
    pub after: PaneId,
    /// The extent of the parent split along its own axis, once dividers are taken
    /// out. Dividing a drag by this turns points into the fraction the protocol
    /// wants.
    pub usable_extent: f32,
}

impl Divider {
    /// The pointer target, wider than the line so it can be grabbed.
    pub fn grab_rect(&self) -> Rect {
        match self.direction {
            Direction::Horizontal => self.rect.expand2(Vec2::new(DIVIDER_GRAB_MARGIN, 0.0)),
            Direction::Vertical => self.rect.expand2(Vec2::new(0.0, DIVIDER_GRAB_MARGIN)),
        }
    }

    /// The fraction to grow [`Divider::before`] by, for a drag of `delta` points.
    ///
    /// Returns `None` for a drag too small to be worth a round trip, and for a split
    /// with no usable extent — which happens for one frame while a window is being
    /// resized to nothing.
    pub fn fraction_for_drag(&self, delta: Vec2) -> Option<f32> {
        if self.usable_extent <= 0.0 {
            return None;
        }
        let along = match self.direction {
            Direction::Horizontal => delta.x,
            Direction::Vertical => delta.y,
        };
        let fraction = along / self.usable_extent;
        if fraction.abs() < f32::EPSILON {
            return None;
        }
        Some(fraction)
    }
}

/// Every pane and divider, placed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Arrangement {
    pub panes: Vec<PaneRect>,
    pub dividers: Vec<Divider>,
    /// True while one pane is filling the session. The layout tree is untouched, so
    /// un-zooming restores the exact previous geometry — the arrangement simply
    /// leaves the others out.
    pub zoomed: bool,
}

impl Arrangement {
    /// The pane under a point, if any.
    pub fn pane_at(&self, pos: Pos2) -> Option<&PaneRect> {
        self.panes.iter().find(|pane| pane.rect.contains(pos))
    }

    pub fn pane(&self, id: &PaneId) -> Option<&PaneRect> {
        self.panes.iter().find(|pane| &pane.pane_id == id)
    }

    /// The divider whose grab area contains a point.
    pub fn divider_at(&self, pos: Pos2) -> Option<&Divider> {
        self.dividers
            .iter()
            .find(|divider| divider.grab_rect().contains(pos))
    }
}

/// Which way a keyboard navigation is going.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
    Up,
    Down,
}

/// Places every pane in `area`.
///
/// A zoomed pane fills the area on its own and produces no dividers: there is
/// nothing to drag when there is one pane.
pub fn arrange(layout: &Layout, area: Rect) -> Arrangement {
    let mut arrangement = Arrangement::default();
    if area.width() <= 0.0 || area.height() <= 0.0 {
        return arrangement;
    }

    if let Some(zoomed) = &layout.zoomed {
        if let Some(pane) = layout.get(zoomed) {
            arrangement.zoomed = true;
            arrangement.panes.push(placed(pane, area));
            return arrangement;
        }
        // A zoom pointing at a pane that no longer exists is a stale id, not a
        // reason to draw nothing. Fall through to the whole layout.
    }

    place(&layout.root, area, &mut arrangement);
    arrangement
}

fn placed(pane: &turn_core::model::Pane, rect: Rect) -> PaneRect {
    PaneRect {
        pane_id: pane.id.clone(),
        kind: pane.kind,
        node_id: pane.node_id.clone(),
        title: pane.title.clone(),
        rect,
    }
}

fn place(node: &LayoutNode, area: Rect, out: &mut Arrangement) {
    match node {
        LayoutNode::Leaf(pane) => {
            if area.width() >= MIN_PANE_EXTENT && area.height() >= MIN_PANE_EXTENT {
                out.panes.push(placed(pane, area));
            }
        }
        LayoutNode::Split(split) => {
            let children = &split.children;
            if children.is_empty() {
                return;
            }
            if children.len() == 1 {
                // A split of one is a shape the tree can hold transiently, after a
                // close. Drawing its only child full-size is right, and inventing a
                // divider for it would not be.
                place(&children[0].node, area, out);
                return;
            }

            let gutters = DIVIDER_THICKNESS * (children.len() - 1) as f32;
            let total = match split.direction {
                Direction::Horizontal => area.width(),
                Direction::Vertical => area.height(),
            };
            let usable = (total - gutters).max(0.0);
            let fractions = normalised(children.iter().map(|child| child.size));

            let mut cursor = match split.direction {
                Direction::Horizontal => area.min.x,
                Direction::Vertical => area.min.y,
            };
            for (index, child) in children.iter().enumerate() {
                let extent = usable * fractions.get(index).copied().unwrap_or(0.0);
                let child_area = match split.direction {
                    Direction::Horizontal => Rect::from_min_size(
                        Pos2::new(cursor, area.min.y),
                        Vec2::new(extent, area.height()),
                    ),
                    Direction::Vertical => Rect::from_min_size(
                        Pos2::new(area.min.x, cursor),
                        Vec2::new(area.width(), extent),
                    ),
                };
                place(&child.node, child_area, out);
                cursor += extent;

                if index + 1 < children.len() {
                    let divider_rect = match split.direction {
                        Direction::Horizontal => Rect::from_min_size(
                            Pos2::new(cursor, area.min.y),
                            Vec2::new(DIVIDER_THICKNESS, area.height()),
                        ),
                        Direction::Vertical => Rect::from_min_size(
                            Pos2::new(area.min.x, cursor),
                            Vec2::new(area.width(), DIVIDER_THICKNESS),
                        ),
                    };
                    // The divider belongs to the two panes it actually touches, which
                    // for a nested child is the pane nearest the boundary rather than
                    // the subtree's first pane.
                    let before = last_pane(&child.node);
                    let after = children
                        .get(index + 1)
                        .and_then(|next| first_pane(&next.node));
                    if let (Some(before), Some(after)) = (before, after) {
                        out.dividers.push(Divider {
                            direction: split.direction,
                            rect: divider_rect,
                            before,
                            after,
                            usable_extent: usable,
                        });
                    }
                    cursor += DIVIDER_THICKNESS;
                }
            }
        }
    }
}

/// Sizes as fractions summing to one.
///
/// A layout arriving from the store with sizes that do not sum to one is a
/// possibility rather than a bug worth refusing to draw: normalising shows the user
/// their panes in roughly the right proportions, while trusting the numbers would
/// leave a gap or overflow the area.
fn normalised(sizes: impl Iterator<Item = f32>) -> Vec<f32> {
    let sizes: Vec<f32> = sizes.map(|size| size.max(0.0)).collect();
    let total: f32 = sizes.iter().sum();
    if total <= 0.0 {
        let equal = 1.0 / sizes.len().max(1) as f32;
        return vec![equal; sizes.len()];
    }
    sizes.into_iter().map(|size| size / total).collect()
}

fn first_pane(node: &LayoutNode) -> Option<PaneId> {
    match node {
        LayoutNode::Leaf(pane) => Some(pane.id.clone()),
        LayoutNode::Split(split) => split.children.first().and_then(|c| first_pane(&c.node)),
    }
}

fn last_pane(node: &LayoutNode) -> Option<PaneId> {
    match node {
        LayoutNode::Leaf(pane) => Some(pane.id.clone()),
        LayoutNode::Split(split) => split.children.last().and_then(|c| last_pane(&c.node)),
    }
}

/// The pane a directional move from `from` should land on.
///
/// Geometric rather than tree-order: only panes that actually lie on that side and
/// overlap the source's perpendicular span are candidates, and the nearest wins. A
/// move with nothing on that side returns `None`, which the caller renders as
/// "nothing happened" rather than wrapping around — wrapping would send the user to
/// the far side of the window for pressing an arrow at the edge.
pub fn neighbour(arrangement: &Arrangement, from: &PaneId, side: Side) -> Option<PaneId> {
    let source = arrangement.pane(from)?.rect;
    let mut best: Option<(f32, f32, PaneId)> = None;

    for candidate in &arrangement.panes {
        if &candidate.pane_id == from {
            continue;
        }
        let rect = candidate.rect;
        let (distance, overlap) = match side {
            Side::Left => (
                source.min.x - rect.max.x,
                overlap_1d(source.min.y, source.max.y, rect.min.y, rect.max.y),
            ),
            Side::Right => (
                rect.min.x - source.max.x,
                overlap_1d(source.min.y, source.max.y, rect.min.y, rect.max.y),
            ),
            Side::Up => (
                source.min.y - rect.max.y,
                overlap_1d(source.min.x, source.max.x, rect.min.x, rect.max.x),
            ),
            Side::Down => (
                rect.min.y - source.max.y,
                overlap_1d(source.min.x, source.max.x, rect.min.x, rect.max.x),
            ),
        };
        // Negative distance means the candidate is on the wrong side or overlapping;
        // a tolerance of the divider thickness admits the immediate neighbour.
        if distance < -f32::EPSILON || distance > source.width().max(source.height()) * 8.0 {
            continue;
        }
        if overlap <= 0.0 {
            continue;
        }
        // Nearest first, then the largest shared edge, so a tall pane beside two
        // short ones goes to the one it shares most of its edge with.
        let key = (distance, -overlap);
        let better = match &best {
            None => true,
            Some((best_distance, best_overlap, _)) => key < (*best_distance, *best_overlap),
        };
        if better {
            best = Some((key.0, key.1, candidate.pane_id.clone()));
        }
    }
    best.map(|(_, _, id)| id)
}

fn overlap_1d(a_min: f32, a_max: f32, b_min: f32, b_max: f32) -> f32 {
    (a_max.min(b_max) - a_min.max(b_min)).max(0.0)
}

/// The size in cells a rectangle can show, given the size of one cell.
///
/// This is what makes the pty match what is drawn. Rounding down rather than to
/// nearest: a pty told it has one more column than the window can paint puts the
/// last character of every wrapped line where nobody can see it.
pub fn size_in_cells(rect: Rect, cell: Vec2) -> turn_proto::PtySize {
    let cols = if cell.x > 0.0 {
        (rect.width() / cell.x).floor()
    } else {
        0.0
    };
    let rows = if cell.y > 0.0 {
        (rect.height() / cell.y).floor()
    } else {
        0.0
    };
    // Clamped rather than allowed to be zero: `PtySize::new` treats a degenerate
    // size as a mistake, and a pane one pixel high during a window drag is not one.
    turn_proto::PtySize::new(rows.max(1.0) as u16, cols.max(1.0) as u16)
}

#[cfg(test)]
mod tests {
    use super::*;
    use turn_core::model::{Pane, Split};

    fn pane(name: &str) -> Pane {
        Pane::new(PaneKind::Terminal).with_title(name)
    }

    fn area() -> Rect {
        Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 600.0))
    }

    fn titled<'a>(arrangement: &'a Arrangement, title: &str) -> &'a PaneRect {
        arrangement
            .panes
            .iter()
            .find(|p| p.title.as_deref() == Some(title))
            .unwrap_or_else(|| panic!("no pane titled {title}"))
    }

    #[test]
    fn a_single_pane_fills_the_area_and_has_no_dividers() {
        let layout = Layout::single(pane("only"));
        let arrangement = arrange(&layout, area());
        assert_eq!(arrangement.panes.len(), 1);
        assert_eq!(arrangement.panes[0].rect, area());
        assert!(arrangement.dividers.is_empty());
        assert!(!arrangement.zoomed);
    }

    /// The divider takes its own space out of the split before the fractions apply.
    /// Laying out first and drawing the divider on top is what makes panes drift a
    /// few pixels narrower with every split.
    #[test]
    fn two_panes_share_the_area_left_over_after_the_divider() {
        let mut layout = Layout::single(pane("left"));
        let first = layout.panes()[0].id.clone();
        layout.split(&first, Direction::Horizontal, pane("right"));

        let arrangement = arrange(&layout, area());
        let left = titled(&arrangement, "left");
        let right = titled(&arrangement, "right");
        let usable = 1000.0 - DIVIDER_THICKNESS;

        assert!((left.rect.width() - usable / 2.0).abs() < 0.01);
        assert!((right.rect.width() - usable / 2.0).abs() < 0.01);
        assert!(
            (right.rect.min.x - (left.rect.max.x + DIVIDER_THICKNESS)).abs() < 0.01,
            "the panes must not overlap the divider"
        );
        assert_eq!(arrangement.dividers.len(), 1);
        let divider = &arrangement.dividers[0];
        assert_eq!(divider.direction, Direction::Horizontal);
        assert!((divider.usable_extent - usable).abs() < 0.01);
        assert_eq!(divider.before, left.pane_id);
        assert_eq!(divider.after, right.pane_id);
    }

    #[test]
    fn three_side_by_side_panes_have_two_dividers_and_equal_thirds() {
        let mut layout = Layout::single(pane("a"));
        let first = layout.panes()[0].id.clone();
        layout.split(&first, Direction::Horizontal, pane("b"));
        let second = layout.panes()[1].id.clone();
        layout.split(&second, Direction::Horizontal, pane("c"));

        let arrangement = arrange(&layout, area());
        assert_eq!(arrangement.panes.len(), 3);
        assert_eq!(arrangement.dividers.len(), 2);
        let widths: Vec<f32> = arrangement.panes.iter().map(|p| p.rect.width()).collect();
        let total: f32 = widths.iter().sum();
        assert!(
            (total + 2.0 * DIVIDER_THICKNESS - 1000.0).abs() < 0.05,
            "the panes plus the dividers must fill the area exactly; got {widths:?}"
        );
    }

    /// A nested split's divider has to be measured against the split it lives in,
    /// not against the window, or a drag in the right-hand column moves the pane
    /// twice as far as the pointer.
    #[test]
    fn a_nested_splits_divider_measures_against_its_own_parent() {
        let mut layout = Layout::single(pane("left"));
        let left = layout.panes()[0].id.clone();
        layout.split(&left, Direction::Horizontal, pane("right-top"));
        let right_top = layout.panes()[1].id.clone();
        layout.split(&right_top, Direction::Vertical, pane("right-bottom"));

        let arrangement = arrange(&layout, area());
        assert_eq!(arrangement.panes.len(), 3);
        let vertical = arrangement
            .dividers
            .iter()
            .find(|d| d.direction == Direction::Vertical)
            .expect("the nested split has a horizontal divider line");
        assert!(
            (vertical.usable_extent - (600.0 - DIVIDER_THICKNESS)).abs() < 0.01,
            "the nested divider spans the height of its own column, got {}",
            vertical.usable_extent
        );

        // And a drag of a tenth of that column is a tenth, not a twentieth.
        let fraction = vertical
            .fraction_for_drag(Vec2::new(0.0, (600.0 - DIVIDER_THICKNESS) / 10.0))
            .expect("a drag of sixty points is worth sending");
        assert!((fraction - 0.1).abs() < 0.001, "got {fraction}");
    }

    #[test]
    fn a_drag_becomes_a_fraction_of_the_parent_and_a_zero_drag_is_not_sent() {
        let mut layout = Layout::single(pane("left"));
        let first = layout.panes()[0].id.clone();
        layout.split(&first, Direction::Horizontal, pane("right"));
        let arrangement = arrange(&layout, area());
        let divider = &arrangement.dividers[0];

        let grow = divider
            .fraction_for_drag(Vec2::new(divider.usable_extent / 4.0, 0.0))
            .expect("a quarter of the split is worth sending");
        assert!((grow - 0.25).abs() < 0.001);
        let shrink = divider
            .fraction_for_drag(Vec2::new(-divider.usable_extent / 4.0, 0.0))
            .expect("dragging the other way shrinks it");
        assert!((shrink + 0.25).abs() < 0.001);
        assert_eq!(
            divider.fraction_for_drag(Vec2::ZERO),
            None,
            "a drag of nothing must not cost a round trip"
        );
        // Movement across the divider rather than along it changes nothing.
        assert_eq!(divider.fraction_for_drag(Vec2::new(0.0, 50.0)), None);
    }

    #[test]
    fn a_divider_is_easier_to_grab_than_it_is_to_see() {
        let mut layout = Layout::single(pane("left"));
        let first = layout.panes()[0].id.clone();
        layout.split(&first, Direction::Horizontal, pane("right"));
        let arrangement = arrange(&layout, area());
        let divider = &arrangement.dividers[0];

        assert!(divider.grab_rect().width() > divider.rect.width());
        assert_eq!(
            divider.grab_rect().height(),
            divider.rect.height(),
            "widening it along its own length would steal clicks from the panes"
        );
        let just_outside = Pos2::new(divider.rect.min.x - DIVIDER_GRAB_MARGIN / 2.0, 300.0);
        assert!(arrangement.divider_at(just_outside).is_some());
    }

    #[test]
    fn a_zoomed_pane_fills_the_area_alone_and_offers_nothing_to_drag() {
        let mut layout = Layout::single(pane("left"));
        let first = layout.panes()[0].id.clone();
        layout.split(&first, Direction::Horizontal, pane("right"));
        let right = layout.panes()[1].id.clone();
        layout.zoomed = Some(right.clone());

        let arrangement = arrange(&layout, area());
        assert!(arrangement.zoomed);
        assert_eq!(arrangement.panes.len(), 1);
        assert_eq!(arrangement.panes[0].pane_id, right);
        assert_eq!(arrangement.panes[0].rect, area());
        assert!(arrangement.dividers.is_empty());
    }

    /// A zoom naming a pane that has since closed is a stale id. Drawing the whole
    /// layout is the recovery; drawing nothing would be a blank window.
    #[test]
    fn a_zoom_pointing_at_a_pane_that_is_gone_falls_back_to_the_whole_layout() {
        let mut layout = Layout::single(pane("only"));
        layout.zoomed = Some(PaneId::new());
        let arrangement = arrange(&layout, area());
        assert!(!arrangement.zoomed);
        assert_eq!(arrangement.panes.len(), 1);
    }

    #[test]
    fn navigation_finds_the_pane_that_is_actually_on_that_side() {
        let mut layout = Layout::single(pane("left"));
        let first = layout.panes()[0].id.clone();
        layout.split(&first, Direction::Horizontal, pane("right"));
        let arrangement = arrange(&layout, area());
        let left = titled(&arrangement, "left").pane_id.clone();
        let right = titled(&arrangement, "right").pane_id.clone();

        assert_eq!(
            neighbour(&arrangement, &left, Side::Right),
            Some(right.clone())
        );
        assert_eq!(neighbour(&arrangement, &right, Side::Left), Some(left));
        assert_eq!(
            neighbour(&arrangement, &right, Side::Right),
            None,
            "an arrow at the edge must do nothing rather than wrap to the far side"
        );
        assert_eq!(neighbour(&arrangement, &right, Side::Up), None);
    }

    /// The case tree order gets wrong: from the tall left pane, "right" must be the
    /// pane whose edge it shares most of, and up and down within the column must
    /// work from either.
    #[test]
    fn navigation_across_a_nested_split_uses_geometry_and_not_tree_order() {
        let mut layout = Layout::single(pane("left"));
        let left_id = layout.panes()[0].id.clone();
        layout.split(&left_id, Direction::Horizontal, pane("top-right"));
        let top_right = layout.panes()[1].id.clone();
        layout.split(&top_right, Direction::Vertical, pane("bottom-right"));

        let arrangement = arrange(&layout, area());
        let left = titled(&arrangement, "left").pane_id.clone();
        let top = titled(&arrangement, "top-right").pane_id.clone();
        let bottom = titled(&arrangement, "bottom-right").pane_id.clone();

        assert_eq!(
            neighbour(&arrangement, &top, Side::Down),
            Some(bottom.clone())
        );
        assert_eq!(
            neighbour(&arrangement, &bottom, Side::Up),
            Some(top.clone())
        );
        assert_eq!(
            neighbour(&arrangement, &top, Side::Left),
            Some(left.clone())
        );
        assert_eq!(
            neighbour(&arrangement, &bottom, Side::Left),
            Some(left.clone())
        );
        // From the tall pane, "right" lands on one of the two — either is defensible,
        // but it must not be nothing.
        let rightwards = neighbour(&arrangement, &left, Side::Right);
        assert!(
            rightwards == Some(top) || rightwards == Some(bottom),
            "got {rightwards:?}"
        );
    }

    #[test]
    fn a_pane_too_small_to_show_anything_is_left_out_rather_than_drawn_as_a_sliver() {
        let mut layout = Layout::single(pane("wide"));
        let first = layout.panes()[0].id.clone();
        layout.split(&first, Direction::Horizontal, pane("sliver"));
        // Push the second pane down to a couple of points.
        if let LayoutNode::Split(split) = &mut layout.root {
            split.children[0].size = 0.999;
            split.children[1].size = 0.001;
        }
        let arrangement = arrange(&layout, area());
        assert_eq!(arrangement.panes.len(), 1);
        assert_eq!(arrangement.panes[0].title.as_deref(), Some("wide"));
    }

    #[test]
    fn a_layout_whose_fractions_do_not_add_up_is_normalised_rather_than_refused() {
        let mut layout = Layout::single(pane("a"));
        let first = layout.panes()[0].id.clone();
        layout.split(&first, Direction::Horizontal, pane("b"));
        if let LayoutNode::Split(split) = &mut layout.root {
            // A store that saved 3 and 1 instead of 0.75 and 0.25.
            split.children[0].size = 3.0;
            split.children[1].size = 1.0;
        }
        let arrangement = arrange(&layout, area());
        let usable = 1000.0 - DIVIDER_THICKNESS;
        assert!((titled(&arrangement, "a").rect.width() - usable * 0.75).abs() < 0.01);
        assert!((titled(&arrangement, "b").rect.width() - usable * 0.25).abs() < 0.01);
    }

    #[test]
    fn a_split_with_no_sizes_at_all_falls_back_to_equal_shares() {
        let mut layout = Layout::single(pane("a"));
        let first = layout.panes()[0].id.clone();
        layout.split(&first, Direction::Horizontal, pane("b"));
        if let LayoutNode::Split(split) = &mut layout.root {
            split.children[0].size = 0.0;
            split.children[1].size = 0.0;
        }
        let arrangement = arrange(&layout, area());
        let usable = 1000.0 - DIVIDER_THICKNESS;
        for placed in &arrangement.panes {
            assert!((placed.rect.width() - usable / 2.0).abs() < 0.01);
        }
    }

    #[test]
    fn a_split_holding_one_child_draws_it_full_size_without_inventing_a_divider() {
        // The shape the tree holds transiently after a close.
        let layout = Layout {
            root: LayoutNode::Split(Split {
                direction: Direction::Horizontal,
                children: vec![turn_core::model::Child {
                    size: 1.0,
                    node: LayoutNode::Leaf(pane("survivor")),
                }],
            }),
            active: None,
            zoomed: None,
        };
        let arrangement = arrange(&layout, area());
        assert_eq!(arrangement.panes.len(), 1);
        assert_eq!(arrangement.panes[0].rect, area());
        assert!(arrangement.dividers.is_empty());
    }

    #[test]
    fn an_area_with_no_room_produces_nothing_rather_than_negative_rectangles() {
        let layout = Layout::single(pane("only"));
        let arrangement = arrange(&layout, Rect::from_min_size(Pos2::ZERO, Vec2::ZERO));
        assert!(arrangement.panes.is_empty());
        assert!(arrangement.dividers.is_empty());
    }

    /// The pty has to be told the size that is actually painted, in cells. Rounding
    /// up would hide the last column of every wrapped line off the edge.
    #[test]
    fn the_size_in_cells_rounds_down_so_the_pty_matches_what_is_drawn() {
        let cell = Vec2::new(8.0, 17.0);
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(8.0 * 80.0 + 7.0, 17.0 * 24.0 + 16.0));
        let size = size_in_cells(rect, cell);
        assert_eq!((size.rows, size.cols), (24, 80));
    }

    #[test]
    fn a_pane_briefly_smaller_than_one_cell_still_reports_a_usable_size() {
        // Mid-drag a window can be a few pixels tall, and `PtySize` refuses a zero.
        let size = size_in_cells(
            Rect::from_min_size(Pos2::ZERO, Vec2::new(2.0, 2.0)),
            Vec2::new(8.0, 17.0),
        );
        assert_eq!((size.rows, size.cols), (1, 1));
        assert!(!turn_proto::PtySize::was_degenerate(size.rows, size.cols));
    }
}
