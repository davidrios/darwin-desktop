//! The dwindle layout: a binary tree of splits, each leaf a window.
//!
//! Ported from Hyprland's `DwindleAlgorithm` (src/layout/algorithm/tiled/
//! dwindle, v0.57.2; Copyright (c) 2022-2026 vaxerski, BSD 3-Clause, see
//! tests/HYPRLAND-LICENSE): a new window splits the leaf it opens on, side by
//! side when that leaf is wider than tall (`split_width_multiplier`), with the
//! new window first or second by pointer position (`force_split` 0) or fixed
//! (1: first, 2: second). Without `preserve_split` the split direction follows
//! each node's aspect ratio on every relayout. `split_ratio` is the first
//! child's share times two (1.0 is half).

use crate::config::DwindleConfig;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// The edges a mouse resize drags (the corner nearest the pointer); none
/// for a resize without a corner.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Edges {
    pub left: bool,
    pub top: bool,
    pub right: bool,
    pub bottom: bool,
}

impl Rect {
    pub fn new(x: f64, y: f64, w: f64, h: f64) -> Rect {
        Rect { x, y, w: w.max(0.0), h: h.max(0.0) }
    }
}

#[derive(Debug)]
enum Kind<T> {
    Leaf(T),
    Split { children: [usize; 2], split_top: bool, ratio: f64 },
}

#[derive(Debug)]
struct Node<T> {
    parent: Option<usize>,
    kind: Kind<T>,
    rect: Rect,
}

#[derive(Debug)]
pub struct Dwindle<T> {
    nodes: Vec<Option<Node<T>>>,
    root: Option<usize>,
}

impl<T> Default for Dwindle<T> {
    fn default() -> Self {
        Dwindle { nodes: Vec::new(), root: None }
    }
}

impl<T: Clone + PartialEq> Dwindle<T> {
    pub fn is_empty(&self) -> bool {
        self.root.is_none()
    }

    pub fn contains(&self, item: &T) -> bool {
        self.leaf_of(item).is_some()
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.leaves().len()
    }

    fn node(&self, i: usize) -> &Node<T> {
        self.nodes[i].as_ref().expect("live node")
    }

    fn node_mut(&mut self, i: usize) -> &mut Node<T> {
        self.nodes[i].as_mut().expect("live node")
    }

    fn alloc(&mut self, node: Node<T>) -> usize {
        if let Some(i) = self.nodes.iter().position(Option::is_none) {
            self.nodes[i] = Some(node);
            i
        } else {
            self.nodes.push(Some(node));
            self.nodes.len() - 1
        }
    }

    fn leaf_of(&self, item: &T) -> Option<usize> {
        self.nodes.iter().position(|n| matches!(n, Some(Node { kind: Kind::Leaf(t), .. }) if t == item))
    }

    /// Leaves in tree order (left/top first).
    fn leaves(&self) -> Vec<usize> {
        let mut out = Vec::new();
        let mut stack: Vec<usize> = self.root.into_iter().collect();
        while let Some(i) = stack.pop() {
            match &self.node(i).kind {
                Kind::Leaf(_) => out.push(i),
                Kind::Split { children, .. } => {
                    stack.push(children[1]);
                    stack.push(children[0]);
                }
            }
        }
        out
    }

    /// The tile a window had at the last `recalc`.
    pub fn rect_of(&self, item: &T) -> Option<Rect> {
        self.leaf_of(item).map(|l| self.node(l).rect)
    }

    /// Adds a window, splitting `opening_on` (or the first window).
    pub fn insert(&mut self, item: T, opening_on: Option<&T>, pointer: Option<(f64, f64)>, area: Rect, cfg: &DwindleConfig) {
        self.insert_split(item, opening_on, pointer, area, cfg, None);
    }

    /// `insert` with the split direction forced: `Some(true)` stacks the two
    /// windows, `Some(false)` puts them side by side.
    pub fn insert_split(
        &mut self,
        item: T,
        opening_on: Option<&T>,
        pointer: Option<(f64, f64)>,
        area: Rect,
        cfg: &DwindleConfig,
        stacked: Option<bool>,
    ) {
        self.recalc(area, cfg);
        let leaf = Node { parent: None, kind: Kind::Leaf(item), rect: area };
        let Some(root) = self.root else {
            self.root = Some(self.alloc(leaf));
            return;
        };
        let target = opening_on.and_then(|t| self.leaf_of(t)).unwrap_or_else(|| self.leaves().first().copied().unwrap_or(root));
        let target_rect = self.node(target).rect;
        let target_parent = self.node(target).parent;

        let side_by_side = match stacked {
            Some(stacked) => !stacked,
            None => target_rect.w > target_rect.h * cfg.split_width_multiplier,
        };
        let new_first = match cfg.force_split {
            1 => true,
            2 => false,
            _ => pointer.is_some_and(|(px, py)| {
                if side_by_side { px < target_rect.x + target_rect.w / 2.0 } else { py < target_rect.y + target_rect.h / 2.0 }
            }),
        };

        let new_leaf = self.alloc(leaf);
        let children = if new_first { [new_leaf, target] } else { [target, new_leaf] };
        let split = self.alloc(Node {
            parent: target_parent,
            kind: Kind::Split { children, split_top: !side_by_side, ratio: cfg.default_split_ratio },
            rect: target_rect,
        });
        self.node_mut(new_leaf).parent = Some(split);
        self.node_mut(target).parent = Some(split);
        match target_parent {
            None => self.root = Some(split),
            Some(p) => {
                if let Kind::Split { children, .. } = &mut self.node_mut(p).kind {
                    for c in children.iter_mut() {
                        if *c == target {
                            *c = split;
                        }
                    }
                }
            }
        }
        self.recalc(area, cfg);
    }

    /// Removes a window; its sibling takes the parent's place.
    pub fn remove(&mut self, item: &T) -> bool {
        let Some(leaf) = self.leaf_of(item) else { return false };
        let parent = self.node(leaf).parent;
        self.nodes[leaf] = None;
        let Some(parent) = parent else {
            self.root = None;
            return true;
        };
        let (sibling, grand) = match &self.node(parent).kind {
            Kind::Split { children, .. } => (if children[0] == leaf { children[1] } else { children[0] }, self.node(parent).parent),
            Kind::Leaf(_) => unreachable!("a leaf's parent is a split"),
        };
        self.nodes[parent] = None;
        self.node_mut(sibling).parent = grand;
        match grand {
            None => self.root = Some(sibling),
            Some(g) => {
                if let Kind::Split { children, .. } = &mut self.node_mut(g).kind {
                    for c in children.iter_mut() {
                        if *c == parent {
                            *c = sibling;
                        }
                    }
                }
            }
        }
        true
    }

    /// Lays the tree out in `area`.
    pub fn recalc(&mut self, area: Rect, cfg: &DwindleConfig) {
        if let Some(root) = self.root {
            self.node_mut(root).rect = area;
            self.recalc_node(root, cfg);
        }
    }

    fn recalc_node(&mut self, i: usize, cfg: &DwindleConfig) {
        let rect = self.node(i).rect;
        let (children, split_top, ratio) = match &mut self.node_mut(i).kind {
            Kind::Leaf(_) => return,
            Kind::Split { children, split_top, ratio } => {
                if !cfg.preserve_split {
                    *split_top = rect.h * cfg.split_width_multiplier > rect.w;
                }
                (*children, *split_top, *ratio)
            }
        };
        let (a, b) = if split_top {
            let first = rect.h / 2.0 * ratio;
            (Rect::new(rect.x, rect.y, rect.w, first), Rect::new(rect.x, rect.y + first, rect.w, rect.h - first))
        } else {
            let first = rect.w / 2.0 * ratio;
            (Rect::new(rect.x, rect.y, first, rect.h), Rect::new(rect.x + first, rect.y, rect.w - first, rect.h))
        };
        self.node_mut(children[0]).rect = a;
        self.node_mut(children[1]).rect = b;
        self.recalc_node(children[0], cfg);
        self.recalc_node(children[1], cfg);
    }

    /// Every window and its tile, as of the last `recalc`.
    pub fn tiles(&self) -> Vec<(T, Rect)> {
        self.leaves()
            .into_iter()
            .filter_map(|i| match &self.node(i).kind {
                Kind::Leaf(t) => Some((t.clone(), self.node(i).rect)),
                Kind::Split { .. } => None,
            })
            .collect()
    }

    /// Flips the split the window sits in (`togglesplit`).
    pub fn toggle_split(&mut self, item: &T) {
        let Some(parent) = self.leaf_of(item).and_then(|l| self.node(l).parent) else { return };
        if let Kind::Split { split_top, .. } = &mut self.node_mut(parent).kind {
            *split_top = !*split_top;
        }
    }

    /// Exchanges two windows' places.
    pub fn swap(&mut self, a: &T, b: &T) {
        let (Some(la), Some(lb)) = (self.leaf_of(a), self.leaf_of(b)) else { return };
        let ta = a.clone();
        let tb = b.clone();
        self.node_mut(la).kind = Kind::Leaf(tb);
        self.node_mut(lb).kind = Kind::Leaf(ta);
    }

    /// Resizes a window's tile by moving split ratios, as Hyprland's
    /// `resizeTarget` with `smart_resizing`: for each axis, the nearest split
    /// on the dragged side ("outer") takes the movement, and the nearest one
    /// on the other side ("inner") is adjusted so the opposite edge stays put.
    pub fn resize(&mut self, item: &T, delta: (f64, f64), edges: Edges, area: Rect, cfg: &DwindleConfig) {
        let Some(leaf) = self.leaf_of(item) else { return };
        let b = self.node(leaf).rect;
        let sticks = |a: f64, b: f64| (a - b).abs() < 2.0;
        let (d_left, d_right) = (sticks(b.x, area.x), sticks(b.x + b.w, area.x + area.w));
        let (d_top, d_bottom) = (sticks(b.y, area.y), sticks(b.y + b.h, area.y + area.h));
        let mut mv = delta;
        if d_left && d_right {
            mv.0 = 0.0;
        }
        if d_top && d_bottom {
            mv.1 = 0.0;
        }
        let none = edges == Edges::default();
        let left = edges.left || d_right;
        let top = edges.top || d_bottom;
        let right = edges.right || d_left;
        let bottom = edges.bottom || d_top;
        let (mut v_outer, mut v_inner, mut h_outer, mut h_inner) = (None, None, None, None);
        let mut cur = leaf;
        while let Some(parent) = self.node(cur).parent {
            let Kind::Split { children, split_top, .. } = self.node(parent).kind else { break };
            if v_outer.is_none() && split_top && (none || (top && children[1] == cur) || (bottom && children[0] == cur)) {
                v_outer = Some(cur);
            } else if v_outer.is_none() && v_inner.is_none() && split_top {
                v_inner = Some(cur);
            } else if h_outer.is_none()
                && !split_top
                && (none || (left && children[1] == cur) || (right && children[0] == cur))
            {
                h_outer = Some(cur);
            } else if h_outer.is_none() && h_inner.is_none() && !split_top {
                h_inner = Some(cur);
            }
            if v_outer.is_some() && h_outer.is_some() {
                break;
            }
            cur = parent;
        }
        if let Some(outer) = h_outer {
            self.resize_axis(outer, h_inner, mv.0, |r| r.w, area, cfg);
        }
        if let Some(outer) = v_outer {
            self.resize_axis(outer, v_inner, mv.1, |r| r.h, area, cfg);
        }
    }

    fn resize_axis(&mut self, outer: usize, inner: Option<usize>, d: f64, size: fn(&Rect) -> f64, area: Rect, cfg: &DwindleConfig) {
        let Some(op) = self.node(outer).parent else { return };
        let op_size = size(&self.node(op).rect);
        if let Kind::Split { ratio, .. } = &mut self.node_mut(op).kind {
            *ratio = (*ratio + d * 2.0 / op_size.max(1.0)).clamp(0.1, 1.9);
        }
        if let Some(inner) = inner {
            let original = size(&self.node(inner).rect);
            self.recalc(area, cfg);
            let Some(ip) = self.node(inner).parent else { return };
            let ip_size = size(&self.node(ip).rect).max(1.0);
            if let Kind::Split { children, ratio, .. } = &mut self.node_mut(ip).kind {
                *ratio = if children[0] == inner {
                    (original - d) / ip_size * 2.0
                } else {
                    2.0 - (original + d) / ip_size * 2.0
                }
                .clamp(0.1, 1.9);
            }
        }
        self.recalc(area, cfg);
    }

    /// The window whose tile contains a point.
    pub fn at(&self, x: f64, y: f64) -> Option<T> {
        self.tiles().into_iter().find(|(_, r)| x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h).map(|(t, _)| t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resize_moves_the_shared_edge() {
        let area = Rect::new(0.0, 0.0, 1000.0, 500.0);
        let mut d = Dwindle::default();
        d.insert(1, None, None, area, &cfg());
        d.insert(2, Some(&1), None, area, &cfg());
        d.recalc(area, &cfg());
        assert_eq!(d.rect_of(&1).unwrap().w, 500.0);
        // Dragging the left window's right edge 100 to the right.
        d.resize(&1, (100.0, 0.0), Edges { right: true, bottom: true, ..Default::default() }, area, &cfg());
        assert_eq!(d.rect_of(&1).unwrap().w, 600.0);
        assert_eq!(d.rect_of(&2).unwrap().x, 600.0);
        // The right window's left edge, dragged left, grows it.
        d.resize(&2, (-200.0, 0.0), Edges { left: true, top: true, ..Default::default() }, area, &cfg());
        assert!((d.rect_of(&2).unwrap().x - 400.0).abs() < 1e-9);
    }

    fn cfg() -> DwindleConfig {
        DwindleConfig {
            preserve_split: false,
            force_split: 2,
            default_split_ratio: 1.0,
            split_width_multiplier: 1.0,
            use_active_for_splits: true,
        }
    }

    const AREA: Rect = Rect { x: 0.0, y: 0.0, w: 1000.0, h: 600.0 };

    #[test]
    fn one_window_fills() {
        let mut d = Dwindle::default();
        d.insert(1, None, None, AREA, &cfg());
        assert_eq!(d.tiles(), vec![(1, AREA)]);
    }

    #[test]
    fn dwindles() {
        let mut d = Dwindle::default();
        d.insert(1, None, None, AREA, &cfg());
        d.insert(2, Some(&1), None, AREA, &cfg());
        // Wider than tall: side by side, new window second.
        assert_eq!(d.tiles(), vec![(1, Rect::new(0.0, 0.0, 500.0, 600.0)), (2, Rect::new(500.0, 0.0, 500.0, 600.0))]);
        d.insert(3, Some(&2), None, AREA, &cfg());
        // 500x600 is taller than wide: 2 and 3 stacked.
        assert_eq!(
            d.tiles(),
            vec![
                (1, Rect::new(0.0, 0.0, 500.0, 600.0)),
                (2, Rect::new(500.0, 0.0, 500.0, 300.0)),
                (3, Rect::new(500.0, 300.0, 500.0, 300.0)),
            ]
        );
        assert_eq!(d.at(700.0, 450.0), Some(3));
        // Removing doesn't lay out; the caller recalculates.
        d.remove(&2);
        d.recalc(AREA, &cfg());
        assert_eq!(d.tiles(), vec![(1, Rect::new(0.0, 0.0, 500.0, 600.0)), (3, Rect::new(500.0, 0.0, 500.0, 600.0))]);
        d.remove(&1);
        d.recalc(AREA, &cfg());
        assert_eq!(d.tiles(), vec![(3, AREA)]);
        d.remove(&3);
        assert!(d.is_empty());
    }

    #[test]
    fn pointer_picks_side() {
        let mut c = cfg();
        c.force_split = 0;
        let mut d = Dwindle::default();
        d.insert(1, None, None, AREA, &c);
        d.insert(2, Some(&1), Some((100.0, 300.0)), AREA, &c);
        assert_eq!(d.tiles()[0].0, 2, "pointer on the left half puts the new window first");
    }

    #[test]
    fn toggle_split_with_preserve() {
        let mut c = cfg();
        c.preserve_split = true;
        let mut d = Dwindle::default();
        d.insert(1, None, None, AREA, &c);
        d.insert(2, Some(&1), None, AREA, &c);
        d.toggle_split(&2);
        d.recalc(AREA, &c);
        assert_eq!(d.tiles()[1].1, Rect::new(0.0, 300.0, 1000.0, 300.0));
        d.swap(&1, &2);
        assert_eq!(d.tiles()[0].0, 2);
    }
}
