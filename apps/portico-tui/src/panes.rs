//! Split panes: the layout tree of one terminal tab (phase 6, ADR
//! "Split panes" of 2026-10-22 in `docs/STACK.md`).
//!
//! A tab is a binary tree: a node splits its area in two with a ratio, a
//! leaf is one pane (in the app, one terminal). The tree is pure geometry
//! and bookkeeping; what a leaf holds is generic so it is tested alone.
//!
//! Invariants:
//! - the tree is never empty (the app closes a tab when its last pane
//!   goes) and holds at most [`MAX_PANES`] leaves;
//! - exactly one leaf has the focus, and it always exists;
//! - a split is refused when either half would be smaller than
//!   [`MIN_COLS`] × [`MIN_ROWS`] cells of terminal (the title row aside);
//! - a divider moved by [`PaneTree::resize`] never makes a pane smaller
//!   than that minimum (when the area allows it at all);
//! - zoom shows the focused pane alone; any change of layout or focus ends
//!   it.
//!
//! Geometry: two children are separated by a one-cell line. When a tab
//! shows several panes, each has a one-row title above its grid; a tab
//! with a single pane (or a zoomed one) uses the area edge to edge, as a
//! plain tab always did.

use std::sync::atomic::{AtomicU64, Ordering};

use ratatui::layout::Rect;

/// Most panes in one tab.
pub(crate) const MAX_PANES: usize = 8;
/// Smallest terminal grid of a pane, in cells.
pub(crate) const MIN_COLS: u16 = 20;
pub(crate) const MIN_ROWS: u16 = 5;
/// Rows above each pane's grid when the tab shows several panes.
pub(crate) const TITLE_ROWS: u16 = 1;
/// Cells between two panes.
const SEPARATOR: u16 = 1;
/// Ratios are fixed-point: `first = available * ratio / RATIO_SCALE`.
const RATIO_SCALE: u32 = 10_000;
/// A fresh split gives each half the same room.
const HALF: u32 = RATIO_SCALE / 2;
/// Cells a divider moves per resize key, across and down.
pub(crate) const RESIZE_COLS: u16 = 4;
pub(crate) const RESIZE_ROWS: u16 = 2;

/// Process-unique pane identifier (stable while the pane lives, whatever
/// happens to the other panes and tabs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct PaneId(u64);

impl PaneId {
    pub(crate) fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

/// How a node divides its area.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Split {
    /// A horizontal divider: the new pane goes below (`"` / `-`).
    Horizontal,
    /// A vertical divider: the new pane goes to the right (`%` / `|`).
    Vertical,
}

/// A direction for focus moves and resizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Dir {
    Left,
    Down,
    Up,
    Right,
}

impl Dir {
    /// The dividers this direction moves.
    fn split(self) -> Split {
        match self {
            Self::Left | Self::Right => Split::Vertical,
            Self::Up | Self::Down => Split::Horizontal,
        }
    }
}

/// Why a split was not made (the app shows [`Self::message`] and
/// [`Self::hint`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// The tab already holds [`MAX_PANES`].
    TooMany,
    /// Each half would be smaller than the minimum.
    TooSmall,
}

impl Refusal {
    pub(crate) fn message(self) -> String {
        match self {
            Self::TooMany => format!("A tab holds at most {MAX_PANES} panes"),
            Self::TooSmall => "Not enough room to split this pane".to_owned(),
        }
    }

    pub(crate) fn hint(self) -> String {
        match self {
            Self::TooMany => "Close a pane, or open a new tab.".to_owned(),
            Self::TooSmall => format!(
                "Each pane needs at least {MIN_COLS}×{MIN_ROWS} cells: enlarge the window, zoom or close a pane."
            ),
        }
    }
}

/// Where a pane is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PaneArea {
    pub(crate) id: PaneId,
    /// Title row (absent for a tab showing one pane).
    pub(crate) title: Option<Rect>,
    /// The terminal grid.
    pub(crate) grid: Rect,
}

/// A divider line between two panes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Divider {
    pub(crate) split: Split,
    pub(crate) area: Rect,
}

/// What a tab draws.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Layout {
    pub(crate) panes: Vec<PaneArea>,
    pub(crate) dividers: Vec<Divider>,
}

#[derive(Debug)]
enum Node<T> {
    Leaf {
        id: PaneId,
        item: T,
    },
    Split {
        split: Split,
        /// Share of the first child, over [`RATIO_SCALE`].
        ratio: u32,
        first: Box<Node<T>>,
        second: Box<Node<T>>,
    },
}

/// The panes of one tab.
#[derive(Debug)]
pub(crate) struct PaneTree<T> {
    /// `None` only transiently, inside an operation.
    root: Option<Node<T>>,
    focus: PaneId,
    zoomed: bool,
}

/// `extent` cells minus the divider, shared by `ratio`: `(first, second)`.
fn share(extent: u16, ratio: u32) -> (u16, u16) {
    let available = extent.saturating_sub(SEPARATOR);
    let first = u32::from(available).saturating_mul(ratio) / RATIO_SCALE;
    let first = u16::try_from(first).unwrap_or(available).min(available);
    (first, available - first)
}

/// The two children's areas and the divider between them.
fn split_rect(area: Rect, split: Split, ratio: u32) -> (Rect, Rect, Rect) {
    match split {
        Split::Vertical => {
            let (a, b) = share(area.width, ratio);
            let first = Rect { width: a, ..area };
            let divider = Rect {
                x: area.x.saturating_add(a),
                width: SEPARATOR.min(area.width),
                ..area
            };
            let second = Rect {
                x: area.x.saturating_add(a).saturating_add(SEPARATOR),
                width: b,
                ..area
            };
            (first, divider, second)
        }
        Split::Horizontal => {
            let (a, b) = share(area.height, ratio);
            let first = Rect { height: a, ..area };
            let divider = Rect {
                y: area.y.saturating_add(a),
                height: SEPARATOR.min(area.height),
                ..area
            };
            let second = Rect {
                y: area.y.saturating_add(a).saturating_add(SEPARATOR),
                height: b,
                ..area
            };
            (first, divider, second)
        }
    }
}

/// A pane's title row and grid in `area`.
fn leaf_area(id: PaneId, area: Rect, titled: bool) -> PaneArea {
    if titled && area.height > TITLE_ROWS {
        PaneArea {
            id,
            title: Some(Rect {
                height: TITLE_ROWS,
                ..area
            }),
            grid: Rect {
                y: area.y.saturating_add(TITLE_ROWS),
                height: area.height - TITLE_ROWS,
                ..area
            },
        }
    } else {
        PaneArea {
            id,
            title: None,
            grid: area,
        }
    }
}

/// Smallest pane rectangle (title row included).
const fn min_leaf() -> (u16, u16) {
    (MIN_COLS, MIN_ROWS + TITLE_ROWS)
}

impl<T> Node<T> {
    fn count(&self) -> usize {
        match self {
            Self::Leaf { .. } => 1,
            Self::Split { first, second, .. } => first.count() + second.count(),
        }
    }

    fn ids(&self, out: &mut Vec<PaneId>) {
        match self {
            Self::Leaf { id, .. } => out.push(*id),
            Self::Split { first, second, .. } => {
                first.ids(out);
                second.ids(out);
            }
        }
    }

    fn first_leaf(&self) -> PaneId {
        match self {
            Self::Leaf { id, .. } => *id,
            Self::Split { first, .. } => first.first_leaf(),
        }
    }

    fn last_leaf(&self) -> PaneId {
        match self {
            Self::Leaf { id, .. } => *id,
            Self::Split { second, .. } => second.last_leaf(),
        }
    }

    fn get(&self, target: PaneId) -> Option<&T> {
        match self {
            Self::Leaf { id, item } => (*id == target).then_some(item),
            Self::Split { first, second, .. } => first.get(target).or_else(|| second.get(target)),
        }
    }

    fn get_mut(&mut self, target: PaneId) -> Option<&mut T> {
        match self {
            Self::Leaf { id, item } => (*id == target).then_some(item),
            Self::Split { first, second, .. } => match first.get_mut(target) {
                Some(item) => Some(item),
                None => second.get_mut(target),
            },
        }
    }

    fn items<'a>(&'a self, out: &mut Vec<(PaneId, &'a T)>) {
        match self {
            Self::Leaf { id, item } => out.push((*id, item)),
            Self::Split { first, second, .. } => {
                first.items(out);
                second.items(out);
            }
        }
    }

    fn items_mut<'a>(&'a mut self, out: &mut Vec<(PaneId, &'a mut T)>) {
        match self {
            Self::Leaf { id, item } => out.push((*id, item)),
            Self::Split { first, second, .. } => {
                first.items_mut(out);
                second.items_mut(out);
            }
        }
    }

    /// Smallest `(width, height)` this subtree can be drawn in.
    fn min_size(&self) -> (u16, u16) {
        match self {
            Self::Leaf { .. } => min_leaf(),
            Self::Split {
                split,
                first,
                second,
                ..
            } => {
                let (fw, fh) = first.min_size();
                let (sw, sh) = second.min_size();
                match split {
                    Split::Vertical => {
                        (fw.saturating_add(SEPARATOR).saturating_add(sw), fh.max(sh))
                    }
                    Split::Horizontal => {
                        (fw.max(sw), fh.saturating_add(SEPARATOR).saturating_add(sh))
                    }
                }
            }
        }
    }

    fn layout(&self, area: Rect, titled: bool, out: &mut Layout) {
        match self {
            Self::Leaf { id, .. } => out.panes.push(leaf_area(*id, area, titled)),
            Self::Split {
                split,
                ratio,
                first,
                second,
            } => {
                let (a, divider, b) = split_rect(area, *split, *ratio);
                first.layout(a, titled, out);
                out.dividers.push(Divider {
                    split: *split,
                    area: divider,
                });
                second.layout(b, titled, out);
            }
        }
    }

    /// Replaces leaf `target` by a split of it and `new`; gives `new`
    /// back when `target` is not in this subtree.
    fn split_leaf(
        self,
        target: PaneId,
        split: Split,
        new: (PaneId, T),
    ) -> (Self, Option<(PaneId, T)>) {
        match self {
            Self::Leaf { id, item } if id == target => {
                let (new_id, new_item) = new;
                (
                    Self::Split {
                        split,
                        ratio: HALF,
                        first: Box::new(Self::Leaf { id, item }),
                        second: Box::new(Self::Leaf {
                            id: new_id,
                            item: new_item,
                        }),
                    },
                    None,
                )
            }
            leaf @ Self::Leaf { .. } => (leaf, Some(new)),
            Self::Split {
                split: own,
                ratio,
                first,
                second,
            } => {
                let (first, rest) = first.split_leaf(target, split, new);
                let (second, rest) = match rest {
                    Some(new) => second.split_leaf(target, split, new),
                    None => (*second, None),
                };
                (
                    Self::Split {
                        split: own,
                        ratio,
                        first: Box::new(first),
                        second: Box::new(second),
                    },
                    rest,
                )
            }
        }
    }

    /// Removes leaf `target`: `(what is left, removed item, heir)`. The heir
    /// is the pane that takes the room (nearest the closed one).
    fn remove(self, target: PaneId) -> (Option<Self>, Option<(T, PaneId)>) {
        match self {
            Self::Leaf { id, item } if id == target => (None, Some((item, id))),
            leaf @ Self::Leaf { .. } => (Some(leaf), None),
            Self::Split {
                split,
                ratio,
                first,
                second,
            } => {
                let (rest, removed) = first.remove(target);
                if let Some((item, heir)) = removed {
                    let Some(rest) = rest else {
                        // `first` was the leaf: `second` takes the room.
                        let heir = second.first_leaf();
                        return (Some(*second), Some((item, heir)));
                    };
                    let node = Self::Split {
                        split,
                        ratio,
                        first: Box::new(rest),
                        second,
                    };
                    return (Some(node), Some((item, heir)));
                }
                // Not found in `first`: it came back whole (a subtree only
                // vanishes when the removed leaf was all of it).
                let Some(first) = rest else {
                    return (Some(*second), None);
                };
                let (rest, removed) = second.remove(target);
                match (rest, removed) {
                    (Some(rest), removed) => (
                        Some(Self::Split {
                            split,
                            ratio,
                            first: Box::new(first),
                            second: Box::new(rest),
                        }),
                        removed,
                    ),
                    (None, removed) => {
                        let heir = first.last_leaf();
                        (Some(first), removed.map(|(item, _)| (item, heir)))
                    }
                }
            }
        }
    }

    /// Moves the divider nearest `target` of the kind `dir` moves, by
    /// `step` cells. `None`: `target` is not in this subtree; `Some(true)`:
    /// a divider of that kind was found (moved, or clamped in place).
    fn resize(&mut self, area: Rect, target: PaneId, dir: Dir, step: u16) -> Option<bool> {
        let Self::Split {
            split,
            ratio,
            first,
            second,
        } = self
        else {
            return match self {
                Self::Leaf { id, .. } if *id == target => Some(false),
                _ => None,
            };
        };
        let (a, _, b) = split_rect(area, *split, *ratio);
        // Deepest first: the divider nearest the pane moves.
        let below = match first.resize(a, target, dir, step) {
            Some(done) => Some(done),
            None => second.resize(b, target, dir, step),
        };
        match below {
            None => None,
            Some(true) => Some(true),
            Some(false) if *split != dir.split() => Some(false),
            Some(false) => {
                let extent = match split {
                    Split::Vertical => area.width,
                    Split::Horizontal => area.height,
                };
                let available = extent.saturating_sub(SEPARATOR);
                let pick = |(w, h): (u16, u16)| match split {
                    Split::Vertical => w,
                    Split::Horizontal => h,
                };
                let low = pick(first.min_size());
                let high = available.saturating_sub(pick(second.min_size()));
                let (current, _) = share(extent, *ratio);
                let wanted = match dir {
                    Dir::Left | Dir::Up => current.saturating_sub(step),
                    Dir::Right | Dir::Down => current.saturating_add(step),
                };
                if low > high || available == 0 {
                    // Too small for both minimums: leave it.
                    return Some(true);
                }
                let clamped = wanted.clamp(low, high);
                if clamped != current {
                    *ratio = ratio_for(clamped, available);
                }
                Some(true)
            }
        }
    }
}

/// The ratio whose [`share`] gives exactly `first` cells out of `available`.
fn ratio_for(first: u16, available: u16) -> u32 {
    if available == 0 {
        return HALF;
    }
    let scaled = u32::from(first) * RATIO_SCALE;
    scaled.div_ceil(u32::from(available)).min(RATIO_SCALE)
}

impl<T> PaneTree<T> {
    /// A tab with one pane, focused.
    pub(crate) fn new(id: PaneId, item: T) -> Self {
        Self {
            root: Some(Node::Leaf { id, item }),
            focus: id,
            zoomed: false,
        }
    }

    pub(crate) fn focus(&self) -> PaneId {
        self.focus
    }

    pub(crate) fn zoomed(&self) -> bool {
        self.zoomed
    }

    pub(crate) fn len(&self) -> usize {
        self.root.as_ref().map_or(0, Node::count)
    }

    /// Pane identifiers, in reading order (depth first, first child first).
    pub(crate) fn ids(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        if let Some(root) = &self.root {
            root.ids(&mut out);
        }
        out
    }

    pub(crate) fn contains(&self, id: PaneId) -> bool {
        self.get(id).is_some()
    }

    pub(crate) fn get(&self, id: PaneId) -> Option<&T> {
        self.root.as_ref()?.get(id)
    }

    pub(crate) fn get_mut(&mut self, id: PaneId) -> Option<&mut T> {
        self.root.as_mut()?.get_mut(id)
    }

    pub(crate) fn focused(&self) -> Option<&T> {
        self.get(self.focus)
    }

    pub(crate) fn focused_mut(&mut self) -> Option<&mut T> {
        let focus = self.focus;
        self.get_mut(focus)
    }

    /// Every pane, in reading order.
    pub(crate) fn items(&self) -> Vec<(PaneId, &T)> {
        let mut out = Vec::new();
        if let Some(root) = &self.root {
            root.items(&mut out);
        }
        out
    }

    pub(crate) fn items_mut(&mut self) -> Vec<(PaneId, &mut T)> {
        let mut out = Vec::new();
        if let Some(root) = &mut self.root {
            root.items_mut(&mut out);
        }
        out
    }

    /// Gives the focus to `id` (ends the zoom when it moves).
    pub(crate) fn set_focus(&mut self, id: PaneId) -> bool {
        if !self.contains(id) {
            return false;
        }
        if id != self.focus {
            self.zoomed = false;
        }
        self.focus = id;
        true
    }

    /// Whether panes have a title row (several on screen).
    fn titled(&self) -> bool {
        self.len() > 1
    }

    /// Where everything goes in `area`. Zoomed: the focused pane alone,
    /// with its title (so the zoom stays visible).
    pub(crate) fn layout(&self, area: Rect) -> Layout {
        let mut out = Layout::default();
        let Some(root) = &self.root else {
            return out;
        };
        if self.zoomed {
            out.panes.push(leaf_area(self.focus, area, true));
            return out;
        }
        root.layout(area, self.titled(), &mut out);
        out
    }

    /// The pane areas without zoom (where each one sits in the split).
    fn full_layout(&self, area: Rect) -> Layout {
        let mut out = Layout::default();
        if let Some(root) = &self.root {
            root.layout(area, self.titled(), &mut out);
        }
        out
    }

    /// Grid size `(cols, rows)` of each pane in `area`, zoom included (a
    /// zoomed tab's hidden panes keep their split size).
    pub(crate) fn grid_sizes(&self, area: Rect) -> Vec<(PaneId, u16, u16)> {
        let mut sizes: Vec<(PaneId, u16, u16)> = self
            .full_layout(area)
            .panes
            .iter()
            .map(|p| (p.id, p.grid.width, p.grid.height))
            .collect();
        if self.zoomed {
            let zoomed = self.layout(area);
            for pane in zoomed.panes {
                if let Some(slot) = sizes.iter_mut().find(|(id, ..)| *id == pane.id) {
                    *slot = (pane.id, pane.grid.width, pane.grid.height);
                }
            }
        }
        sizes
    }

    /// Splits the focused pane: `item` takes the second half (below or to
    /// the right) and the focus. Refused when the tab is full or a half
    /// would be under the minimum size in `area`.
    pub(crate) fn split(
        &mut self,
        area: Rect,
        split: Split,
        id: PaneId,
        item: T,
    ) -> Result<(), (Refusal, T)> {
        if let Err(refusal) = self.can_split(area, split) {
            return Err((refusal, item));
        }
        let Some(root) = self.root.take() else {
            return Err((Refusal::TooSmall, item));
        };
        let (root, rest) = root.split_leaf(self.focus, split, (id, item));
        self.root = Some(root);
        if let Some((_, item)) = rest {
            return Err((Refusal::TooSmall, item));
        }
        self.focus = id;
        self.zoomed = false;
        Ok(())
    }

    /// Whether [`Self::split`] would accept, without doing it.
    pub(crate) fn can_split(&self, area: Rect, split: Split) -> Result<(), Refusal> {
        if self.len() >= MAX_PANES {
            return Err(Refusal::TooMany);
        }
        let layout = self.full_layout(area);
        let Some(pane) = layout.panes.iter().find(|p| p.id == self.focus) else {
            return Err(Refusal::TooSmall);
        };
        // The pane's whole rectangle (its title comes back if it had one).
        let whole = pane.title.map_or(pane.grid, |t| t.union(pane.grid));
        let (a, _, b) = split_rect(whole, split, HALF);
        let (min_w, min_h) = min_leaf();
        let fits = |r: Rect| r.width >= min_w && r.height >= min_h;
        if fits(a) && fits(b) {
            Ok(())
        } else {
            Err(Refusal::TooSmall)
        }
    }

    /// The split that suits the focused pane best (side by side when it is
    /// wide, cells being about twice as tall as wide), then the other one.
    pub(crate) fn preferred_splits(&self, area: Rect) -> [Split; 2] {
        let layout = self.full_layout(area);
        let wide = layout
            .panes
            .iter()
            .find(|p| p.id == self.focus)
            .is_some_and(|p| p.grid.width >= p.grid.height.saturating_mul(2));
        if wide {
            [Split::Vertical, Split::Horizontal]
        } else {
            [Split::Horizontal, Split::Vertical]
        }
    }

    /// Removes pane `id`; the pane that takes its room gets the focus if
    /// `id` had it. `None` when `id` is not here. The tree may become
    /// empty: see [`Self::is_empty`].
    pub(crate) fn remove(&mut self, id: PaneId) -> Option<T> {
        let root = self.root.take()?;
        let (rest, removed) = root.remove(id);
        self.root = rest;
        let (item, heir) = removed?;
        self.zoomed = false;
        if self.focus == id || !self.contains(self.focus) {
            self.focus = if self.contains(heir) {
                heir
            } else {
                self.ids().first().copied().unwrap_or(heir)
            };
        }
        Some(item)
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.root.is_none()
    }

    /// Moves the focus to the nearest pane in `dir` (no wrap); `false` at
    /// the edge.
    pub(crate) fn focus_dir(&mut self, area: Rect, dir: Dir) -> bool {
        let layout = self.full_layout(area);
        let whole = |p: &PaneArea| p.title.map_or(p.grid, |t| t.union(p.grid));
        let Some(current) = layout.panes.iter().find(|p| p.id == self.focus).map(whole) else {
            return false;
        };
        let overlap = |a0: u16, a1: u16, b0: u16, b1: u16| a1.min(b1).saturating_sub(a0.max(b0));
        let best = layout
            .panes
            .iter()
            .filter(|p| p.id != self.focus)
            .filter_map(|p| {
                let r = whole(p);
                // (distance, overlap) of a pane past the edge in `dir`.
                let (distance, shared) = match dir {
                    Dir::Left if r.right() <= current.left() => (
                        current.left() - r.right(),
                        overlap(r.top(), r.bottom(), current.top(), current.bottom()),
                    ),
                    Dir::Right if r.left() >= current.right() => (
                        r.left() - current.right(),
                        overlap(r.top(), r.bottom(), current.top(), current.bottom()),
                    ),
                    Dir::Up if r.bottom() <= current.top() => (
                        current.top() - r.bottom(),
                        overlap(r.left(), r.right(), current.left(), current.right()),
                    ),
                    Dir::Down if r.top() >= current.bottom() => (
                        r.top() - current.bottom(),
                        overlap(r.left(), r.right(), current.left(), current.right()),
                    ),
                    _ => return None,
                };
                (shared > 0).then_some((distance, std::cmp::Reverse(shared), p.id))
            })
            .min_by_key(|(distance, shared, _)| (*distance, *shared));
        match best {
            Some((_, _, id)) => self.set_focus(id),
            None => false,
        }
    }

    /// Focuses the next pane in reading order (wraps).
    pub(crate) fn focus_next(&mut self) -> bool {
        let ids = self.ids();
        let Some(position) = ids.iter().position(|id| *id == self.focus) else {
            return false;
        };
        match ids.get((position + 1) % ids.len().max(1)) {
            Some(next) if *next != self.focus => self.set_focus(*next),
            _ => false,
        }
    }

    /// Moves the divider next to the focused pane toward `dir` by a
    /// step, clamped so no pane gets under the minimum; `false` when no
    /// divider of that kind borders the pane (or the tab is zoomed).
    pub(crate) fn resize(&mut self, area: Rect, dir: Dir) -> bool {
        if self.zoomed {
            return false;
        }
        let step = match dir.split() {
            Split::Vertical => RESIZE_COLS,
            Split::Horizontal => RESIZE_ROWS,
        };
        let focus = self.focus;
        let before = self.full_layout(area);
        let Some(root) = &mut self.root else {
            return false;
        };
        root.resize(area, focus, dir, step);
        self.full_layout(area) != before
    }

    /// Zoom on / off; refused (`false`) with a single pane.
    pub(crate) fn toggle_zoom(&mut self) -> bool {
        if self.len() < 2 {
            self.zoomed = false;
            return false;
        }
        self.zoomed = !self.zoomed;
        true
    }
}

#[cfg(test)]
// Panes are named after their position (`a` | `b` over `c` | `d`).
#[allow(clippy::many_single_char_names)]
mod tests {
    use super::*;

    const AREA: Rect = Rect {
        x: 0,
        y: 1,
        width: 80,
        height: 22,
    };

    fn tree() -> (PaneTree<&'static str>, PaneId) {
        let id = PaneId::next();
        (PaneTree::new(id, "a"), id)
    }

    fn area_of(tree: &PaneTree<&str>, id: PaneId) -> PaneArea {
        tree.layout(AREA)
            .panes
            .into_iter()
            .find(|p| p.id == id)
            .unwrap()
    }

    #[test]
    fn one_pane_fills_the_area_without_title() {
        let (t, a) = tree();
        let layout = t.layout(AREA);
        assert_eq!(layout.panes.len(), 1);
        assert_eq!(area_of(&t, a).grid, AREA);
        assert_eq!(area_of(&t, a).title, None);
        assert!(layout.dividers.is_empty());
    }

    #[test]
    fn split_vertical_then_horizontal() {
        let (mut t, a) = tree();
        let b = PaneId::next();
        t.split(AREA, Split::Vertical, b, "b").unwrap();
        assert_eq!(t.focus(), b, "the new pane takes the focus");
        assert_eq!(t.len(), 2);
        let (pa, pb) = (area_of(&t, a), area_of(&t, b));
        // 80 = 39 + divider + 40, a title row each.
        assert_eq!((pa.grid.width, pb.grid.width), (39, 40));
        assert_eq!(pa.grid.height, 21);
        assert_eq!(pa.title.unwrap().y, 1);
        assert_eq!(pb.grid.x, 40);
        let layout = t.layout(AREA);
        assert_eq!(
            layout.dividers,
            vec![Divider {
                split: Split::Vertical,
                area: Rect::new(39, 1, 1, 22)
            }]
        );

        let c = PaneId::next();
        t.split(AREA, Split::Horizontal, c, "c").unwrap();
        let (pb, pc) = (area_of(&t, b), area_of(&t, c));
        assert_eq!(pb.grid.x, pc.grid.x);
        assert!(pc.title.unwrap().y > pb.grid.bottom() - 1);
        assert_eq!(t.ids(), vec![a, b, c]);
        assert_eq!(t.get(c), Some(&"c"));
    }

    #[test]
    fn close_collapses_into_the_sibling_and_moves_focus() {
        let (mut t, a) = tree();
        let b = PaneId::next();
        let c = PaneId::next();
        t.split(AREA, Split::Vertical, b, "b").unwrap();
        t.split(AREA, Split::Horizontal, c, "c").unwrap();
        // Closing `c` (focused): `b` takes its room and the focus.
        assert_eq!(t.remove(c), Some("c"));
        assert_eq!(t.focus(), b);
        assert_eq!(area_of(&t, b).grid.height, 21);
        // Closing `a`, unfocused: focus stays, `b` fills the tab, no title.
        assert_eq!(t.remove(a), Some("a"));
        assert_eq!(t.focus(), b);
        assert_eq!(area_of(&t, b).grid, AREA);
        assert_eq!(t.remove(PaneId::next()), None);
        assert_eq!(t.remove(b), Some("b"));
        assert!(t.is_empty());
        assert_eq!(t.len(), 0);
    }

    #[test]
    fn closing_a_first_child_gives_focus_to_the_nearest_pane() {
        let (mut t, a) = tree();
        let b = PaneId::next();
        let c = PaneId::next();
        t.split(AREA, Split::Vertical, b, "b").unwrap();
        t.split(AREA, Split::Horizontal, c, "c").unwrap();
        t.set_focus(a);
        t.remove(a);
        // The right column took the room; its first pane is the nearest.
        assert_eq!(t.focus(), b);
        assert_eq!(t.ids(), vec![b, c]);
    }

    /// 2×2: a | b over c | d.
    fn grid() -> (PaneTree<&'static str>, [PaneId; 4]) {
        let (mut t, a) = tree();
        let (b, c, d) = (PaneId::next(), PaneId::next(), PaneId::next());
        t.split(AREA, Split::Horizontal, c, "c").unwrap();
        t.set_focus(a);
        t.split(AREA, Split::Vertical, b, "b").unwrap();
        t.set_focus(c);
        t.split(AREA, Split::Vertical, d, "d").unwrap();
        (t, [a, b, c, d])
    }

    #[test]
    fn focus_moves_in_each_direction_without_wrapping() {
        let (mut t, [a, b, c, d]) = grid();
        t.set_focus(a);
        assert!(t.focus_dir(AREA, Dir::Right));
        assert_eq!(t.focus(), b);
        assert!(t.focus_dir(AREA, Dir::Down));
        assert_eq!(t.focus(), d);
        assert!(t.focus_dir(AREA, Dir::Left));
        assert_eq!(t.focus(), c);
        assert!(t.focus_dir(AREA, Dir::Up));
        assert_eq!(t.focus(), a);
        // Edges: nothing to the left or above.
        assert!(!t.focus_dir(AREA, Dir::Left));
        assert!(!t.focus_dir(AREA, Dir::Up));
        assert_eq!(t.focus(), a);
        // Next pane, in reading order, wraps.
        for expected in [b, c, d, a] {
            assert!(t.focus_next());
            assert_eq!(t.focus(), expected);
        }
    }

    #[test]
    fn focus_prefers_the_pane_alongside() {
        // a | (b over c): from c, left is a; from a, right is the one
        // sharing the most rows (b and c tie: the nearer, then the first).
        let (mut t, a) = tree();
        let (b, c) = (PaneId::next(), PaneId::next());
        t.split(AREA, Split::Vertical, b, "b").unwrap();
        t.split(AREA, Split::Horizontal, c, "c").unwrap();
        assert!(t.focus_dir(AREA, Dir::Left));
        assert_eq!(t.focus(), a);
        assert!(t.focus_dir(AREA, Dir::Right));
        assert!(t.focus() == b || t.focus() == c);
    }

    #[test]
    fn resize_moves_the_nearest_divider_and_clamps() {
        let (mut t, a) = tree();
        let b = PaneId::next();
        t.split(AREA, Split::Vertical, b, "b").unwrap();
        t.set_focus(a);
        assert!(t.resize(AREA, Dir::Right));
        assert_eq!(area_of(&t, a).grid.width, 39 + RESIZE_COLS);
        assert!(t.resize(AREA, Dir::Left));
        assert_eq!(area_of(&t, a).grid.width, 39);
        // Far left: clamped at the minimum width, then nothing moves.
        for _ in 0..20 {
            t.resize(AREA, Dir::Left);
        }
        assert_eq!(area_of(&t, a).grid.width, MIN_COLS);
        assert!(!t.resize(AREA, Dir::Left));
        // Far right: the other pane keeps its minimum.
        for _ in 0..30 {
            t.resize(AREA, Dir::Right);
        }
        assert_eq!(area_of(&t, b).grid.width, MIN_COLS);
        assert_eq!(area_of(&t, a).grid.width, 80 - 1 - MIN_COLS);
        // No horizontal divider here.
        assert!(!t.resize(AREA, Dir::Down));
    }

    #[test]
    fn resize_respects_nested_minimums() {
        let (mut t, [a, _, c, _]) = grid();
        t.set_focus(a);
        for _ in 0..20 {
            t.resize(AREA, Dir::Down);
        }
        // `c` keeps its 5 rows of grid plus its title.
        assert_eq!(area_of(&t, c).grid.height, MIN_ROWS);
        for _ in 0..20 {
            t.resize(AREA, Dir::Up);
        }
        assert_eq!(area_of(&t, a).grid.height, MIN_ROWS);
    }

    #[test]
    fn at_most_eight_panes() {
        let big = Rect::new(0, 0, 400, 200);
        let (mut t, _) = tree();
        for i in 0..7 {
            let split = if i % 2 == 0 {
                Split::Vertical
            } else {
                Split::Horizontal
            };
            t.split(big, split, PaneId::next(), "x").unwrap();
        }
        assert_eq!(t.len(), MAX_PANES);
        let refused = t.split(big, Split::Vertical, PaneId::next(), "y");
        assert!(matches!(refused, Err((Refusal::TooMany, "y"))));
        assert_eq!(t.len(), MAX_PANES);
        assert!(Refusal::TooMany.message().contains('8'));
    }

    #[test]
    fn splits_too_small_are_refused() {
        // 80×24 body: a vertical split leaves 39 and 40 columns; a third
        // in the same column cannot keep 20.
        let (mut t, _) = tree();
        t.split(AREA, Split::Vertical, PaneId::next(), "b").unwrap();
        let refused = t.split(AREA, Split::Vertical, PaneId::next(), "c");
        assert!(matches!(refused, Err((Refusal::TooSmall, "c"))));
        assert_eq!(t.len(), 2);
        // Stacking: 22 rows → 10 + 11 (titles included), then 5 + 5 is too
        // tight once titles are counted.
        t.split(AREA, Split::Horizontal, PaneId::next(), "d")
            .unwrap();
        assert_eq!(t.can_split(AREA, Split::Horizontal), Err(Refusal::TooSmall));
        let tiny = Rect::new(0, 0, 30, 8);
        let (small, _) = tree();
        assert_eq!(
            small.can_split(tiny, Split::Vertical),
            Err(Refusal::TooSmall)
        );
        assert_eq!(
            small.can_split(tiny, Split::Horizontal),
            Err(Refusal::TooSmall)
        );
        assert!(Refusal::TooSmall.hint().contains("20×5"));
    }

    #[test]
    fn zoom_shows_the_focused_pane_and_ends_on_changes() {
        let (mut t, a) = tree();
        assert!(!t.toggle_zoom(), "nothing to zoom with one pane");
        let b = PaneId::next();
        t.split(AREA, Split::Vertical, b, "b").unwrap();
        assert!(t.toggle_zoom());
        assert!(t.zoomed());
        let layout = t.layout(AREA);
        assert_eq!(layout.panes.len(), 1);
        assert_eq!(layout.panes.first().map(|p| p.id), Some(b));
        assert!(layout.dividers.is_empty());
        // The zoomed pane gets the whole area (below its title); the other
        // keeps its split size for its PTY.
        let sizes = t.grid_sizes(AREA);
        assert!(sizes.contains(&(b, 80, 21)), "{sizes:?}");
        assert!(sizes.contains(&(a, 39, 21)), "{sizes:?}");
        assert!(!t.resize(AREA, Dir::Left), "no resize while zoomed");
        // Moving the focus ends the zoom.
        assert!(t.focus_dir(AREA, Dir::Left));
        assert!(!t.zoomed());
        assert!(t.toggle_zoom());
        // So does a split, and a close.
        t.split(AREA, Split::Horizontal, PaneId::next(), "c")
            .unwrap();
        assert!(!t.zoomed());
        assert!(t.toggle_zoom());
        t.remove(b);
        assert!(!t.zoomed());
        assert!(t.toggle_zoom() || t.len() < 2);
        assert!(t.toggle_zoom() || t.len() < 2);
    }

    #[test]
    fn preferred_split_follows_the_shape() {
        let (t, _) = tree();
        assert_eq!(t.preferred_splits(AREA)[0], Split::Vertical);
        let tall = Rect::new(0, 0, 40, 40);
        assert_eq!(t.preferred_splits(tall)[0], Split::Horizontal);
    }

    #[test]
    fn ratios_round_trip_exactly() {
        for available in [1_u16, 19, 39, 79, 199, 999, 4000] {
            for first in 0..=available.min(300) {
                let ratio = ratio_for(first, available);
                assert_eq!(share(available + 1, ratio).0, first, "{first}/{available}");
            }
        }
    }
}
