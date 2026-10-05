//! Squarified treemap layout (Bruls, Huizing and van Wijk) over a scan tree.
//!
//! Folders are emitted before their contents, so the last rectangle that
//! contains a point is the deepest one there. Once a folder's remaining
//! children are each smaller than `min_area`, they are emitted as one
//! "smaller items" rectangle instead, which keeps the output bounded by the
//! screen area rather than the tree size.

use scanner::{flags, Node, NodeId, NodeKind, ScanTree};

pub const FILE: u8 = 0;
pub const DIR: u8 = 1;
/// The remaining small children of folder `id`, drawn as one block.
pub const REST: u8 = 2;
/// Links and unreadable entries.
pub const OTHER: u8 = 3;

/// Bytes per rectangle in [`encode`]'s output.
pub const RECORD_BYTES: usize = 24;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub id: NodeId,
    /// Category to colour by: a file's own, or the dominant one below a folder.
    pub category: u16,
    /// Depth below the layout root (saturates at 255).
    pub depth: u8,
    pub kind: u8,
}

pub struct Options {
    /// Rectangles smaller than this (in square pixels) are merged.
    pub min_area: f64,
    /// Gap between a folder's edge and its contents.
    pub pad: f64,
    /// Stop after this many rectangles.
    pub max_rects: usize,
}

/// Lays out the subtree under `root` in a `w` x `h` area. `dominant` gives a
/// colour category per node (see [`dominant_categories`]).
pub fn layout(
    tree: &ScanTree,
    dominant: &[u16],
    root: NodeId,
    w: f64,
    h: f64,
    opts: &Options,
) -> Vec<Rect> {
    let mut l = Layout {
        tree,
        dominant,
        opts,
        out: Vec::new(),
    };
    if w > 0.0 && h > 0.0 {
        l.node(
            root,
            Area {
                x: 0.0,
                y: 0.0,
                w,
                h,
            },
            0,
        );
    }
    l.out
}

/// Packs rectangles little-endian: x, y, w, h as f32, id as u32, category
/// as u16, then depth and kind as u8.
pub fn encode(rects: &[Rect]) -> Vec<u8> {
    let mut b = Vec::with_capacity(rects.len() * RECORD_BYTES);
    for r in rects {
        for v in [r.x, r.y, r.w, r.h] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.extend_from_slice(&r.id.to_le_bytes());
        b.extend_from_slice(&r.category.to_le_bytes());
        b.push(r.depth);
        b.push(r.kind);
    }
    b
}

/// For every node, the category holding the most bytes at or below it: a
/// file's own category, or for a folder the largest category among its files.
pub fn dominant_categories(tree: &ScanTree, categories: usize) -> Vec<u16> {
    let n = tree.len();
    let width = categories + 1;
    let mut dominant = vec![0u16; n];
    // Per-folder byte totals by category, in one flat array.
    let mut slot = vec![u32::MAX; n];
    let mut folders = 0u32;
    for (i, node) in tree.nodes().iter().enumerate() {
        if node.kind == NodeKind::Dir {
            slot[i] = folders;
            folders += 1;
        }
    }
    let mut hist = vec![0u64; folders as usize * width];
    let row = |s: u32| s as usize * width..(s as usize + 1) * width;

    // Children have larger ids than their parents, so walking backwards
    // finishes each folder before it is added to its parent.
    for i in (0..n).rev() {
        let node = tree.node(i as NodeId);
        if node.kind == NodeKind::Dir {
            let r = row(slot[i]);
            dominant[i] = argmax(&hist[r.clone()]);
            if i > 0 {
                let p = row(slot[node.parent as usize]);
                for k in 0..width {
                    hist[p.start + k] += hist[r.start + k];
                }
            }
        } else {
            dominant[i] = node.category;
            if node.kind == NodeKind::File && i > 0 {
                let c = node.category as usize;
                if c < width {
                    hist[row(slot[node.parent as usize]).start + c] += effective_size(node);
                }
            }
        }
    }
    dominant
}

/// Index of the largest value, or 0 if all are 0.
fn argmax(v: &[u64]) -> u16 {
    let mut best = 0;
    for (i, &x) in v.iter().enumerate() {
        if x > v[best] {
            best = i;
        }
    }
    if v.get(best).copied().unwrap_or(0) == 0 {
        0
    } else {
        best as u16
    }
}

/// Bytes a node adds to its folder's total (0 for repeated hardlinks).
fn effective_size(n: &Node) -> u64 {
    if n.has_flag(flags::HARDLINK_DUP) {
        0
    } else {
        n.size
    }
}

#[derive(Clone, Copy, Debug)]
struct Area {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

struct Layout<'a> {
    tree: &'a ScanTree,
    dominant: &'a [u16],
    opts: &'a Options,
    out: Vec<Rect>,
}

impl Layout<'_> {
    fn push(&mut self, a: Area, id: NodeId, category: u16, depth: usize, kind: u8) {
        self.out.push(Rect {
            x: a.x as f32,
            y: a.y as f32,
            w: a.w as f32,
            h: a.h as f32,
            id,
            category,
            depth: depth.min(255) as u8,
            kind,
        });
    }

    fn node(&mut self, id: NodeId, a: Area, depth: usize) {
        if self.out.len() >= self.opts.max_rects {
            return;
        }
        let n = self.tree.node(id);
        match n.kind {
            NodeKind::File => self.push(a, id, n.category, depth, FILE),
            NodeKind::Dir => {
                self.push(a, id, self.dominant[id as usize], depth, DIR);
                let pad = self.opts.pad;
                let inner = Area {
                    x: a.x + pad,
                    y: a.y + pad,
                    w: a.w - 2.0 * pad,
                    h: a.h - 2.0 * pad,
                };
                if inner.w >= 1.0 && inner.h >= 1.0 && inner.w * inner.h >= self.opts.min_area {
                    self.children(id, inner, depth + 1);
                }
            }
            NodeKind::Symlink | NodeKind::Error => self.push(a, id, 0, depth, OTHER),
        }
    }

    /// Squarified layout of `dir`'s children inside `a`.
    fn children(&mut self, dir: NodeId, a: Area, depth: usize) {
        let mut kids: Vec<(NodeId, u64)> = self
            .tree
            .child_ids(dir)
            .map(|c| (c, effective_size(self.tree.node(c))))
            .filter(|&(_, s)| s > 0)
            .collect();
        kids.sort_unstable_by(|x, y| y.1.cmp(&x.1).then(x.0.cmp(&y.0)));
        let total: u64 = kids.iter().map(|k| k.1).sum();
        if total == 0 {
            return;
        }
        let scale = a.w * a.h / total as f64;
        let area = |i: usize| kids[i].1 as f64 * scale;
        let min_area = self.opts.min_area;

        let mut rect = a;
        let mut i = 0;
        while i < kids.len() {
            if area(i) < min_area {
                let category = self.dominant_of(&kids[i..]);
                self.push(rect, dir, category, depth, REST);
                return;
            }
            // Grow the row while its worst aspect ratio improves.
            let side = rect.w.min(rect.h);
            let mut sum = area(i);
            let mut worst = worst_ratio(sum, area(i), area(i), side);
            let mut j = i + 1;
            while j < kids.len() && area(j) >= min_area {
                let s = sum + area(j);
                let w = worst_ratio(s, area(i), area(j), side);
                if w > worst {
                    break;
                }
                sum = s;
                worst = w;
                j += 1;
            }

            // Place the row along the short side.
            let thick = if side > 0.0 { sum / side } else { 0.0 };
            let vertical = rect.w >= rect.h; // the row is a column on the left
            let mut offset = 0.0;
            for &(id, size) in &kids[i..j] {
                let len = if thick > 0.0 {
                    size as f64 * scale / thick
                } else {
                    0.0
                };
                let cell = if vertical {
                    Area {
                        x: rect.x,
                        y: rect.y + offset,
                        w: thick,
                        h: len,
                    }
                } else {
                    Area {
                        x: rect.x + offset,
                        y: rect.y,
                        w: len,
                        h: thick,
                    }
                };
                self.node(id, cell, depth);
                offset += len;
            }
            if vertical {
                rect.x += thick;
                rect.w = (rect.w - thick).max(0.0);
            } else {
                rect.y += thick;
                rect.h = (rect.h - thick).max(0.0);
            }
            i = j;
        }
    }

    /// The category with the most bytes among these nodes.
    fn dominant_of(&self, nodes: &[(NodeId, u64)]) -> u16 {
        let mut totals: Vec<u64> = Vec::new();
        for &(id, size) in nodes {
            let c = self.dominant[id as usize] as usize;
            if totals.len() <= c {
                totals.resize(c + 1, 0);
            }
            totals[c] += size;
        }
        argmax(&totals)
    }
}

/// Worst aspect ratio in a row with total area `sum`, largest item `max`
/// and smallest `min`, laid along a side of length `side`.
fn worst_ratio(sum: f64, max: f64, min: f64, side: f64) -> f64 {
    let s2 = sum * sum;
    let l2 = side * side;
    (l2 * max / s2).max(s2 / (l2 * min))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const SIZES: [usize; 7] = [6000, 6000, 4000, 3000, 2000, 2000, 1000];

    fn sample() -> (tempfile::TempDir, ScanTree) {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        for (i, s) in SIZES.iter().enumerate() {
            fs::write(r.join(format!("f{i}.bin")), vec![0u8; *s]).unwrap();
        }
        fs::create_dir_all(r.join("sub/deeper")).unwrap();
        fs::write(r.join("sub/a.bin"), vec![0u8; 3000]).unwrap();
        fs::write(r.join("sub/deeper/b.bin"), vec![0u8; 1000]).unwrap();
        for i in 0..50 {
            fs::write(r.join(format!("sub/tiny{i}.txt")), b"x").unwrap();
        }
        let tree = scanner::scan(r, &scanner::ScanOptions::default()).unwrap();
        (dir, tree)
    }

    fn opts(min_area: f64, pad: f64) -> Options {
        Options {
            min_area,
            pad,
            max_rects: 100_000,
        }
    }

    fn contains(outer: &Rect, inner: &Rect) -> bool {
        let e = 1e-3;
        inner.x >= outer.x - e
            && inner.y >= outer.y - e
            && inner.x + inner.w <= outer.x + outer.w + e
            && inner.y + inner.h <= outer.y + outer.h + e
    }

    fn overlap(a: &Rect, b: &Rect) -> bool {
        let e = 1e-3;
        a.x + e < b.x + b.w && b.x + e < a.x + a.w && a.y + e < b.y + b.h && b.y + e < a.y + a.h
    }

    #[test]
    fn areas_are_proportional_and_rects_tile() {
        let (_d, tree) = sample();
        let dom = vec![0; tree.len()];
        let (w, h) = (800.0, 500.0);
        let rects = layout(&tree, &dom, tree.root(), w, h, &opts(0.0, 0.0));
        let total = tree.node(tree.root()).size as f64;

        // Every node with bytes gets a rectangle, sized by its share.
        let with_bytes = tree.nodes().iter().filter(|n| n.size > 0).count();
        assert_eq!(rects.len(), with_bytes);
        for r in &rects {
            let n = tree.node(r.id);
            let want = n.size as f64 / total * w * h;
            let got = r.w as f64 * r.h as f64;
            assert!(
                (got - want).abs() <= want * 1e-3 + 1e-3,
                "{}: {got} vs {want}",
                n.name
            );
            assert!(contains(&rects[0], r));
        }
        // Siblings never overlap.
        for (i, a) in rects.iter().enumerate() {
            for b in &rects[i + 1..] {
                let pa = tree.node(a.id).parent;
                if a.id != 0 && b.id != 0 && pa == tree.node(b.id).parent {
                    assert!(!overlap(a, b), "{} overlaps {}", a.id, b.id);
                }
            }
        }
    }

    #[test]
    fn children_sit_inside_their_folder() {
        let (_d, tree) = sample();
        let dom = vec![0; tree.len()];
        let rects = layout(&tree, &dom, tree.root(), 600.0, 400.0, &opts(0.0, 2.0));
        let by_id = |id| rects.iter().find(|r| r.id == id && r.kind != REST);
        for r in &rects[1..] {
            let parent = by_id(tree.node(r.id).parent).unwrap();
            assert!(contains(parent, r));
            assert!(r.depth == parent.depth + 1);
        }
    }

    #[test]
    fn small_items_are_merged() {
        let (_d, tree) = sample();
        let dom = dominant_categories(&tree, 0);
        // Each tiny file is ~1/23000 of the area: far below 50 px.
        let rects = layout(&tree, &dom, tree.root(), 400.0, 300.0, &opts(50.0, 0.0));
        let rest: Vec<_> = rects.iter().filter(|r| r.kind == REST).collect();
        assert_eq!(rest.len(), 1);
        assert_eq!(tree.node(rest[0].id).name.as_ref(), "sub");
        assert!(!rects
            .iter()
            .any(|r| tree.node(r.id).name.starts_with("tiny")));
        assert!(rects
            .iter()
            .all(|r| r.kind == REST || r.w * r.h >= 50.0 - 1e-3));
    }

    #[test]
    fn rect_limit_and_encoding() {
        let (_d, tree) = sample();
        let dom = vec![0; tree.len()];
        let limited = Options {
            max_rects: 3,
            ..opts(0.0, 0.0)
        };
        let rects = layout(&tree, &dom, tree.root(), 100.0, 100.0, &limited);
        assert_eq!(rects.len(), 3);
        let bytes = encode(&rects);
        assert_eq!(bytes.len(), 3 * RECORD_BYTES);
        assert_eq!(&bytes[8..12], &100f32.to_le_bytes());
        assert!(layout(&tree, &dom, tree.root(), 0.0, 10.0, &limited).is_empty());
    }

    #[test]
    fn dominant_category_is_the_largest_by_bytes() {
        let (_d, mut tree) = sample();
        fn find(tree: &ScanTree, name: &str) -> NodeId {
            (0..tree.len() as NodeId)
                .find(|&i| tree.node(i).name.as_ref() == name)
                .unwrap()
        }
        // f0, f1 (12000 B) are category 2; everything else stays 1.
        for i in 0..tree.len() as NodeId {
            if tree.node(i).kind == NodeKind::File {
                tree.set_category(i, 1);
            }
        }
        tree.set_category(find(&tree, "f0.bin"), 2);
        tree.set_category(find(&tree, "f1.bin"), 2);
        let dom = dominant_categories(&tree, 2);
        assert_eq!(dom[find(&tree, "f0.bin") as usize], 2);
        assert_eq!(dom[find(&tree, "sub") as usize], 1);
        // Root: category 2 has 12000 B, category 1 has 12000 + 4050 B.
        assert_eq!(dom[0], 1);
        tree.set_category(find(&tree, "f2.bin"), 2);
        tree.set_category(find(&tree, "f3.bin"), 2);
        let dom = dominant_categories(&tree, 2);
        assert_eq!(dom[0], 2);
        assert_eq!(dom[find(&tree, "deeper") as usize], 1);
    }
}
