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
//! * **So is a drop.** Which of a target's five regions the pointer is in is what
//!   decides whether a dragged pane lands left, right, above, below or exchanges, so
//!   the bands and the preview rectangle are computed here — and the preview the user
//!   sees is the same rectangle the decision was made from, not a second guess at it.

use egui::{Pos2, Rect, Vec2};
use turn_core::ids::{NodeId, PaneId};
use turn_core::model::{Direction, DropZone, Layout, LayoutNode, PaneKind};

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

/// The share of a target's width, or height, that one edge band takes.
///
/// Both numbers in this pair are the feature. Bands too thin and "left" is a
/// coin-flip nobody aims for; bands too thick and the centre — the one zone that
/// exchanges rather than splits — becomes impossible to mean. A little over a
/// quarter of each axis leaves the middle roughly two fifths of the pane in each
/// direction, which is a target the size of a pane's own text area.
pub const DROP_EDGE_SHARE: f32 = 0.28;

/// The smallest an edge band may be, in points.
///
/// A share alone would make the bands of a small pane too thin to hit on purpose.
pub const DROP_EDGE_MIN: f32 = 22.0;

/// The largest an edge band may be, in points.
///
/// A share alone would give a maximised pane bands hundreds of points deep, so a
/// pointer resting well inside the pane would still read as an edge.
pub const DROP_EDGE_MAX: f32 = 96.0;

/// One pane, placed.
#[derive(Debug, Clone, PartialEq)]
pub struct PaneRect {
    pub pane_id: PaneId,
    pub kind: PaneKind,
    /// True only when the operator pinned this renderer. False means `kind` is
    /// daemon-detected and can follow a new semantic subject automatically.
    pub kind_is_user_set: bool,
    /// Runtime truth underneath the selected renderer. A manual terminal override
    /// cannot make a semantic-only Process attachable.
    pub terminal_capability: bool,
    /// The process behind it, when there is one. Absent for an empty slot after a
    /// partial restore, or for one of Turn's own views.
    pub node_id: Option<NodeId>,
    pub title: Option<String>,
    pub rect: Rect,
}

impl PaneRect {
    /// Whether this pane is backed by a pty and therefore paints a grid.
    pub fn is_terminal(&self) -> bool {
        self.terminal_capability
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

    /// Where `moved` would land if it were dropped at `pointer`.
    ///
    /// `None` for a pointer outside every pane, and for a pointer over the pane being
    /// moved: a pane cannot be relocated relative to itself, and a drop with no target
    /// is how a drag is abandoned.
    pub fn drop_target_at(&self, moved: &PaneId, pointer: Pos2) -> Option<DropTarget> {
        let target = self
            .pane_at(pointer)
            .filter(|target| &target.pane_id != moved)?;
        let zone = drop_zone_at(target.rect, pointer);
        Some(DropTarget {
            pane_id: target.pane_id.clone(),
            zone,
            preview: drop_preview(target.rect, zone),
        })
    }

    /// The rectangle every pane sits inside.
    ///
    /// The union of what was drawn rather than the area it was drawn into, so a pane
    /// left out for being too small cannot make the layout look wider than it is.
    fn bounds(&self) -> Option<Rect> {
        let mut bounds: Option<Rect> = None;
        for pane in &self.panes {
            bounds = Some(match bounds {
                Some(union) => union.union(pane.rect),
                None => pane.rect,
            });
        }
        bounds
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

impl Side {
    /// The drop zone a directional move means.
    ///
    /// Moving right means landing on the *right* of the pane over there, not on the
    /// side of it that faces you: dropping a pane on its right neighbour's left edge
    /// would put it back exactly where it already was.
    pub fn zone(self) -> DropZone {
        match self {
            Side::Left => DropZone::Left,
            Side::Right => DropZone::Right,
            Side::Up => DropZone::Above,
            Side::Down => DropZone::Below,
        }
    }
}

/// Where a dragged pane would land, and what that looks like.
#[derive(Debug, Clone, PartialEq)]
pub struct DropTarget {
    /// The pane under the pointer. Never the pane being moved.
    pub pane_id: PaneId,
    pub zone: DropZone,
    /// The region the moved pane would occupy. This is what the window highlights:
    /// an outline of the whole target would say "here somewhere", which is the one
    /// thing the five zones exist to stop it saying.
    pub preview: Rect,
}

/// One edge band of a pane that is `extent` points across.
///
/// Clamped at both ends, and never more than a third of the axis, so that even a pane
/// squeezed to a sliver keeps a middle: five zones of which one is unreachable would
/// be four zones and a lie.
pub fn drop_edge_band(extent: f32) -> f32 {
    if extent <= 0.0 {
        return 0.0;
    }
    (extent * DROP_EDGE_SHARE)
        .clamp(DROP_EDGE_MIN, DROP_EDGE_MAX)
        .min(extent / 3.0)
}

/// Which of a target's five regions a point is in.
///
/// Each edge is scored by how far into its own band the point is, as a fraction of
/// that band. A point inside no band is in the centre; a point inside two — a corner —
/// belongs to the one it is proportionally deepest into, which is what makes the
/// diagonal between "left" and "above" fall where a user would draw it rather than
/// where the pane's aspect ratio happens to put it. An exact tie resolves in the order
/// left, right, above, below, so a corner is at least never a flicker between two.
pub fn drop_zone_at(target: Rect, pointer: Pos2) -> DropZone {
    let across = drop_edge_band(target.width());
    let down = drop_edge_band(target.height());
    if across <= 0.0 || down <= 0.0 {
        // A pane with no measurable extent can still be exchanged with, and exchanging
        // is the zone that asks nothing of the geometry.
        return DropZone::Centre;
    }
    let depths = [
        (DropZone::Left, (pointer.x - target.min.x) / across),
        (DropZone::Right, (target.max.x - pointer.x) / across),
        (DropZone::Above, (pointer.y - target.min.y) / down),
        (DropZone::Below, (target.max.y - pointer.y) / down),
    ];
    let mut shallowest = (DropZone::Centre, 1.0);
    for (zone, depth) in depths {
        if depth < shallowest.1 {
            shallowest = (zone, depth);
        }
    }
    shallowest.0
}

/// The region a pane dropped in `zone` would occupy.
///
/// Half the target for an edge, because that is what a split of two gives; the whole
/// target for the centre, because an exchange puts the moved pane exactly where the
/// target is now.
pub fn drop_preview(target: Rect, zone: DropZone) -> Rect {
    let half = target.size() / 2.0;
    match zone {
        DropZone::Left => Rect::from_min_size(target.min, Vec2::new(half.x, target.height())),
        DropZone::Right => Rect::from_min_size(
            Pos2::new(target.center().x, target.min.y),
            Vec2::new(half.x, target.height()),
        ),
        DropZone::Above => Rect::from_min_size(target.min, Vec2::new(target.width(), half.y)),
        DropZone::Below => Rect::from_min_size(
            Pos2::new(target.min.x, target.center().y),
            Vec2::new(target.width(), half.y),
        ),
        DropZone::Centre => target,
    }
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

    if let Some(root) = layout.tiled_root() {
        place(&root, area, &mut arrangement);
    }
    arrangement
}

fn placed(pane: &turn_core::model::Pane, rect: Rect) -> PaneRect {
    PaneRect {
        pane_id: pane.id.clone(),
        kind: pane.presentation_kind(),
        kind_is_user_set: pane.kind_is_user_set,
        terminal_capability: pane.has_terminal_capability(),
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

/// How close to the layout's edge a pane has to be to count as owning it.
///
/// Half a point: sibling rectangles are computed from the same cursor, so a pane that
/// touches an edge touches it exactly, and anything larger would start admitting the
/// pane behind a divider.
const EDGE_TOLERANCE: f32 = 0.5;

/// The relocation a directional move command means: which pane to name, and which of
/// its five regions to land in.
///
/// Two readings, and the second one is the whole reason this is not a swap:
///
/// * **With a neighbour on that side**, the pane lands on the far side of it. In a row
///   of three that shifts the pane one place along; against a nested column it moves
///   the pane into that column. Both are the layout a drag onto that neighbour's far
///   edge would have produced, which is the point — the keyboard and the pointer must
///   not disagree about what "move right" means.
/// * **With nothing on that side**, the pane is already against that edge of the
///   layout, and the move becomes a move to the *outer* edge: the pane leaves the split
///   it is nested in and attaches to that side of whichever pane owns that edge of the
///   layout. This is the reading that makes the keyboard a first-class path rather than
///   a degraded one. Doing nothing here is the other honest option and it is a dead
///   end: in `A│B` nothing is above either pane, so "move up" would never be able to
///   turn a row into a column, and in a row of three no keyboard press could ever
///   produce anything but another row. That is the owner's original complaint, left
///   half-fixed for anybody without a pointer.
///
/// When no *other* pane owns that edge the pane already spans it alone, there is
/// genuinely nowhere further to go, and this returns `None` for the caller to report.
pub fn relocation(
    arrangement: &Arrangement,
    moved: &PaneId,
    side: Side,
) -> Option<(PaneId, DropZone)> {
    let target = match neighbour(arrangement, moved, side) {
        Some(neighbour) => neighbour,
        None => outer_edge_owner(arrangement, moved, side)?,
    };
    Some((target, side.zone()))
}

/// The pane holding the layout's own edge on one side, nearest the pane being moved.
///
/// Ranked by the edge it shares with the moved pane, then by how close it is, then by
/// the order it was drawn in — which is tree order, so a tie between two panes that are
/// equally far away and share no edge at all still resolves the same way every time.
fn outer_edge_owner(arrangement: &Arrangement, moved: &PaneId, side: Side) -> Option<PaneId> {
    let bounds = arrangement.bounds()?;
    let source = arrangement.pane(moved)?.rect;
    let mut best: Option<(f32, f32, PaneId)> = None;

    for candidate in &arrangement.panes {
        if &candidate.pane_id == moved {
            continue;
        }
        let rect = candidate.rect;
        let (distance_to_edge, overlap) = match side {
            Side::Left => (
                (rect.min.x - bounds.min.x).abs(),
                overlap_1d(source.min.y, source.max.y, rect.min.y, rect.max.y),
            ),
            Side::Right => (
                (bounds.max.x - rect.max.x).abs(),
                overlap_1d(source.min.y, source.max.y, rect.min.y, rect.max.y),
            ),
            Side::Up => (
                (rect.min.y - bounds.min.y).abs(),
                overlap_1d(source.min.x, source.max.x, rect.min.x, rect.max.x),
            ),
            Side::Down => (
                (bounds.max.y - rect.max.y).abs(),
                overlap_1d(source.min.x, source.max.x, rect.min.x, rect.max.x),
            ),
        };
        if distance_to_edge > EDGE_TOLERANCE {
            continue;
        }
        let key = (-overlap, source.center().distance(rect.center()));
        let better = match &best {
            None => true,
            Some((best_overlap, best_distance, _)) => key < (*best_overlap, *best_distance),
        };
        if better {
            best = Some((key.0, key.1, candidate.pane_id.clone()));
        }
    }
    best.map(|(_, _, id)| id)
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

    /// Both ends of the clamp, and the rule that keeps the middle reachable. A band that
    /// grew with the pane would make a maximised pane one big edge.
    #[test]
    fn an_edge_band_is_a_share_of_the_pane_but_never_thinner_than_a_target_nor_a_third_of_it() {
        assert!((drop_edge_band(200.0) - 56.0).abs() < 0.01, "a share");
        assert!(
            (drop_edge_band(1000.0) - DROP_EDGE_MAX).abs() < 0.01,
            "a wide pane's bands stop growing, or its centre would be unreachable"
        );
        assert!(
            (drop_edge_band(60.0) - 20.0).abs() < 0.01,
            "a third of a narrow pane, so the two bands and a centre each get one"
        );
        // The minimum only applies where there is room for it.
        assert!(drop_edge_band(120.0) >= DROP_EDGE_MIN);
        assert_eq!(drop_edge_band(0.0), 0.0);
        for extent in [13.0f32, 40.0, 97.0, 333.0, 1600.0] {
            assert!(
                drop_edge_band(extent) * 2.0 < extent,
                "a pane {extent} points across kept no centre"
            );
        }
    }

    #[test]
    fn each_edge_of_a_pane_reads_as_that_edge_and_the_middle_reads_as_an_exchange() {
        let target = Rect::from_min_size(Pos2::new(100.0, 50.0), Vec2::new(400.0, 300.0));
        let band = drop_edge_band(400.0);
        let inside = band / 2.0;

        assert_eq!(
            drop_zone_at(target, Pos2::new(target.min.x + inside, target.center().y)),
            DropZone::Left
        );
        assert_eq!(
            drop_zone_at(target, Pos2::new(target.max.x - inside, target.center().y)),
            DropZone::Right
        );
        assert_eq!(
            drop_zone_at(target, Pos2::new(target.center().x, target.min.y + inside)),
            DropZone::Above
        );
        assert_eq!(
            drop_zone_at(target, Pos2::new(target.center().x, target.max.y - inside)),
            DropZone::Below
        );
        assert_eq!(drop_zone_at(target, target.center()), DropZone::Centre);
        // And the centre is not a knife edge: a pointer well inside the pane but nowhere
        // near the middle of it still means "exchange".
        assert_eq!(
            drop_zone_at(
                target,
                Pos2::new(target.min.x + band * 1.5, target.min.y + band * 1.4)
            ),
            DropZone::Centre
        );
    }

    /// A pane squeezed narrow is where a share-only rule stops working: the bands have to
    /// stay hittable and the centre has to stay meanable.
    #[test]
    fn a_narrow_pane_still_has_five_regions() {
        let target = Rect::from_min_size(Pos2::ZERO, Vec2::new(60.0, 400.0));
        assert_eq!(drop_zone_at(target, Pos2::new(3.0, 200.0)), DropZone::Left);
        assert_eq!(
            drop_zone_at(target, Pos2::new(57.0, 200.0)),
            DropZone::Right
        );
        assert_eq!(
            drop_zone_at(target, Pos2::new(30.0, 200.0)),
            DropZone::Centre
        );
        assert_eq!(drop_zone_at(target, Pos2::new(30.0, 4.0)), DropZone::Above);
        assert_eq!(
            drop_zone_at(target, Pos2::new(30.0, 396.0)),
            DropZone::Below
        );
    }

    /// The corner case, literally. Comparing how deep the pointer is into each band as a
    /// fraction of that band is what stops a wide pane's corners all reading as "above".
    #[test]
    fn a_corner_belongs_to_the_band_the_pointer_is_proportionally_deepest_into() {
        let wide = Rect::from_min_size(Pos2::ZERO, Vec2::new(900.0, 200.0));
        let across = drop_edge_band(900.0);
        let down = drop_edge_band(200.0);
        assert!(across > down, "the bands differ, which is the point");

        // Nearer its own left band than its own top band, in fractions of each.
        assert_eq!(
            drop_zone_at(wide, Pos2::new(across * 0.2, down * 0.6)),
            DropZone::Left
        );
        assert_eq!(
            drop_zone_at(wide, Pos2::new(across * 0.6, down * 0.2)),
            DropZone::Above
        );
        // A perfect diagonal tie resolves the same way every time rather than flickering.
        let tie = drop_zone_at(wide, Pos2::new(across * 0.5, down * 0.5));
        assert_eq!(tie, DropZone::Left);
    }

    #[test]
    fn the_preview_is_the_half_the_pane_would_take_and_the_whole_pane_for_an_exchange() {
        let target = Rect::from_min_size(Pos2::new(20.0, 40.0), Vec2::new(400.0, 200.0));

        let left = drop_preview(target, DropZone::Left);
        assert_eq!(left.min, target.min);
        assert!((left.width() - 200.0).abs() < 0.01);
        assert!((left.height() - target.height()).abs() < 0.01);

        let right = drop_preview(target, DropZone::Right);
        assert_eq!(right.max, target.max);
        assert!((right.width() - 200.0).abs() < 0.01);

        let above = drop_preview(target, DropZone::Above);
        assert_eq!(above.min, target.min);
        assert!((above.height() - 100.0).abs() < 0.01);
        assert!((above.width() - target.width()).abs() < 0.01);

        let below = drop_preview(target, DropZone::Below);
        assert_eq!(below.max, target.max);
        assert!((below.height() - 100.0).abs() < 0.01);

        assert_eq!(
            drop_preview(target, DropZone::Centre),
            target,
            "an exchange puts the moved pane exactly where the target is"
        );
    }

    #[test]
    fn a_drop_lands_on_the_pane_under_the_pointer_and_in_the_region_it_is_over() {
        let mut layout = Layout::single(pane("left"));
        let first = layout.panes()[0].id.clone();
        layout.split(&first, Direction::Horizontal, pane("right"));
        let arrangement = arrange(&layout, area());
        let left = titled(&arrangement, "left").pane_id.clone();
        let right = titled(&arrangement, "right");
        let right_rect = right.rect;
        let right_id = right.pane_id.clone();

        let onto_top = arrangement
            .drop_target_at(
                &left,
                Pos2::new(right_rect.center().x, right_rect.min.y + 6.0),
            )
            .expect("the pointer is over the right pane");
        assert_eq!(onto_top.pane_id, right_id);
        assert_eq!(onto_top.zone, DropZone::Above);
        assert_eq!(onto_top.preview, drop_preview(right_rect, DropZone::Above));

        let onto_middle = arrangement
            .drop_target_at(&left, right_rect.center())
            .expect("the pointer is over the right pane");
        assert_eq!(onto_middle.zone, DropZone::Centre);
        assert_eq!(onto_middle.preview, right_rect);
    }

    /// The two ways a gesture ends without a move: back where it started, or nowhere.
    #[test]
    fn a_drop_on_the_pane_being_moved_or_outside_every_pane_is_not_a_target() {
        let mut layout = Layout::single(pane("left"));
        let first = layout.panes()[0].id.clone();
        layout.split(&first, Direction::Horizontal, pane("right"));
        let arrangement = arrange(&layout, area());
        let left = titled(&arrangement, "left");
        let left_id = left.pane_id.clone();

        assert_eq!(
            arrangement.drop_target_at(&left_id, left.rect.center()),
            None
        );
        assert_eq!(
            arrangement.drop_target_at(&left_id, Pos2::new(-40.0, -40.0)),
            None
        );
    }

    /// The keyboard reading of "move right": the pane lands on the far side of the pane
    /// that is there, which in a row of three is one place along rather than a swap of
    /// the two ends.
    #[test]
    fn a_directional_move_lands_the_pane_on_the_far_side_of_its_neighbour() {
        let mut layout = Layout::single(pane("a"));
        let first = layout.panes()[0].id.clone();
        layout.split(&first, Direction::Horizontal, pane("b"));
        let second = layout.panes()[1].id.clone();
        layout.split(&second, Direction::Horizontal, pane("c"));
        let arrangement = arrange(&layout, area());
        let a = titled(&arrangement, "a").pane_id.clone();
        let b = titled(&arrangement, "b").pane_id.clone();
        let c = titled(&arrangement, "c").pane_id.clone();

        assert_eq!(
            relocation(&arrangement, &a, Side::Right),
            Some((b.clone(), DropZone::Right)),
            "the pane goes past its neighbour, not into where it already was"
        );
        assert_eq!(
            relocation(&arrangement, &c, Side::Left),
            Some((b, DropZone::Left))
        );
        assert_eq!(
            relocation(&arrangement, &c, Side::Right),
            None,
            "the rightmost pane of a row already spans that edge on its own"
        );
        assert_eq!(relocation(&arrangement, &a, Side::Left), None);
    }

    /// The reading that makes the keyboard able to change the shape of a layout at all.
    /// Nothing is above either pane of a row, and "nothing happens" would mean a row could
    /// never become a column without a pointer.
    #[test]
    fn moving_a_pane_of_a_row_upwards_takes_it_to_the_outer_edge_and_makes_a_column() {
        let mut layout = Layout::single(pane("left"));
        let first = layout.panes()[0].id.clone();
        layout.split(&first, Direction::Horizontal, pane("right"));
        let arrangement = arrange(&layout, area());
        let left = titled(&arrangement, "left").pane_id.clone();
        let right = titled(&arrangement, "right").pane_id.clone();

        assert_eq!(
            neighbour(&arrangement, &right, Side::Up),
            None,
            "there is nothing above it, which is the case under test"
        );
        assert_eq!(
            relocation(&arrangement, &right, Side::Up),
            Some((left.clone(), DropZone::Above)),
            "the pane becomes the top of the layout rather than doing nothing"
        );
        assert_eq!(
            relocation(&arrangement, &left, Side::Down),
            Some((right, DropZone::Below))
        );
    }

    /// A pane nested in a column, moved along the axis its column does not run in. It has
    /// to leave the column, and it attaches to the pane that owns the edge it is heading
    /// for.
    #[test]
    fn a_pane_nested_in_a_column_can_be_moved_out_of_it_by_keyboard() {
        let mut layout = Layout::single(pane("tall"));
        let tall = layout.panes()[0].id.clone();
        layout.split(&tall, Direction::Horizontal, pane("top"));
        let top = layout.panes()[1].id.clone();
        layout.split(&top, Direction::Vertical, pane("bottom"));
        let arrangement = arrange(&layout, area());
        let tall_id = titled(&arrangement, "tall").pane_id.clone();
        let top_id = titled(&arrangement, "top").pane_id.clone();
        let bottom_id = titled(&arrangement, "bottom").pane_id.clone();

        // Within the column, the neighbour reading applies and the two exchange places.
        assert_eq!(
            relocation(&arrangement, &bottom_id, Side::Up),
            Some((top_id.clone(), DropZone::Above))
        );
        // Downwards there is nothing below the bottom pane, and the pane that owns the
        // bottom edge of the layout is the tall one beside it.
        assert_eq!(
            relocation(&arrangement, &bottom_id, Side::Down),
            Some((tall_id.clone(), DropZone::Below))
        );
        // The tall pane is not the only pane touching the top of the layout — the column
        // beside it starts there too — so moving it up takes it out of the row and makes it
        // the top of the whole layout.
        assert_eq!(
            relocation(&arrangement, &tall_id, Side::Up),
            Some((top_id, DropZone::Above))
        );
        assert_eq!(
            relocation(&arrangement, &tall_id, Side::Down),
            Some((bottom_id, DropZone::Below))
        );
        // Leftwards it is the only pane against that edge, so there is genuinely nowhere
        // further to go.
        assert_eq!(relocation(&arrangement, &tall_id, Side::Left), None);
    }

    /// The move that the "outer edge" reading exists for, proved against the domain rather
    /// than described: the pane comes out of the row and spans the whole width. The window
    /// only names the pane and the zone, so this is the layout the daemon will answer with.
    #[test]
    fn a_move_to_the_outer_edge_produces_a_pane_that_spans_it() {
        let mut layout = Layout::single(pane("tall"));
        let tall = layout.panes()[0].id.clone();
        layout.split(&tall, Direction::Horizontal, pane("top"));
        let top = layout.panes()[1].id.clone();
        layout.split(&top, Direction::Vertical, pane("bottom"));
        let arrangement = arrange(&layout, area());
        let tall_id = titled(&arrangement, "tall").pane_id.clone();

        let (target, zone) =
            relocation(&arrangement, &tall_id, Side::Up).expect("there is an edge to move to");
        assert!(layout.relocate(&tall_id, &target, zone));
        assert!(layout.sizes_are_normalised());

        let moved = arrange(&layout, area());
        let tall_rect = moved
            .pane(&tall_id)
            .expect("the pane survived the move")
            .rect;
        assert!(
            (tall_rect.width() - area().width()).abs() < 0.01,
            "the pane should span the width of the layout, got {tall_rect:?}"
        );
        assert!(
            tall_rect.min.y - area().min.y < 0.01,
            "and sit against the top of it"
        );
        assert_eq!(moved.panes.len(), 3, "no pane was lost on the way");
    }

    #[test]
    fn a_session_of_one_pane_has_nowhere_to_move_it() {
        let layout = Layout::single(pane("only"));
        let arrangement = arrange(&layout, area());
        let only = arrangement.panes[0].pane_id.clone();
        for side in [Side::Left, Side::Right, Side::Up, Side::Down] {
            assert_eq!(relocation(&arrangement, &only, side), None, "{side:?}");
        }
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
            floating: Vec::new(),
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
