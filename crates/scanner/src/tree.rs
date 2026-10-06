use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::ops::Range;
use std::path::PathBuf;
use std::time::Duration;

use crate::fsinfo::FileKey;

/// Index of a node in a [`ScanTree`]. The root is always 0.
pub type NodeId = u32;

/// What a node is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum NodeKind {
    File,
    Dir,
    /// A symlink or junction. Listed but never followed.
    Symlink,
    /// An entry whose details could not be read.
    Error,
}

/// Bit flags stored in [`Node::flags`].
pub mod flags {
    /// Another link to this same file was already counted; this one adds 0 to totals.
    pub const HARDLINK_DUP: u8 = 1 << 0;
    /// The file has more than one hardlink inside the scanned folder.
    pub const HARDLINKED: u8 = 1 << 1;
    /// Online-only cloud file (for example OneDrive); takes no local space.
    pub const CLOUD_PLACEHOLDER: u8 = 1 << 2;
    /// NTFS-compressed file.
    pub const COMPRESSED: u8 = 1 << 3;
    /// Sparse file.
    pub const SPARSE: u8 = 1 << 4;
    /// The folder could not be listed (fully or partly); its totals are incomplete.
    pub const READ_ERROR: u8 = 1 << 5;
    /// Deleted after the scan (see [`ScanTree::remove`](crate::ScanTree::remove)):
    /// sizes are zero and the node should be hidden.
    pub const REMOVED: u8 = 1 << 6;
    /// A folder left out on purpose (`ScanOptions::exclude`): listed, but
    /// its contents weren't read and add nothing to the totals.
    pub const EXCLUDED: u8 = 1 << 7;
}

/// One file, folder, link or error entry.
///
/// For folders, `size`, `alloc` and `files` are totals of everything below.
#[derive(Clone, Debug)]
pub struct Node {
    /// Entry name. The root's name is the full starting path.
    pub name: Box<str>,
    pub parent: NodeId,
    /// First child id; children are `first_child .. first_child + child_count`.
    pub first_child: NodeId,
    pub child_count: u32,
    pub kind: NodeKind,
    pub flags: u8,
    /// Category id, filled in by the categories crate (0 = uncategorised).
    pub category: u16,
    /// Logical size in bytes.
    pub size: u64,
    /// Size on disk in bytes.
    pub alloc: u64,
    /// Last write time, seconds since 1970-01-01 UTC (0 if unknown).
    pub modified: i64,
    /// Number of files at or below this node.
    pub files: u64,
}

impl Node {
    pub fn is_dir(&self) -> bool {
        self.kind == NodeKind::Dir
    }
    pub fn has_flag(&self, flag: u8) -> bool {
        self.flags & flag != 0
    }
}

/// Totals and timing for a finished scan.
#[derive(Clone, Debug, Default)]
pub struct ScanStats {
    pub files: u64,
    pub dirs: u64,
    pub symlinks: u64,
    pub errors: u64,
    pub hardlink_dups: u64,
    pub elapsed: Duration,
    /// How the tree was read: [`ScanMethod::Walk`](crate::ScanMethod) or `Mft`.
    pub method: crate::ScanMethod,
    /// Why the MFT was tried but not used, if it was.
    pub mft_skipped: Option<String>,
    /// The MFT was tried and only administrator rights were missing.
    pub mft_needs_admin: bool,
}

/// The result of a scan: a flat arena of nodes.
#[derive(Debug)]
pub struct ScanTree {
    pub(crate) nodes: Vec<Node>,
    pub(crate) errors: Vec<(NodeId, Box<str>)>,
    pub(crate) stats: ScanStats,
}

impl ScanTree {
    pub fn root(&self) -> NodeId {
        0
    }

    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id as usize]
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    pub fn stats(&self) -> &ScanStats {
        &self.stats
    }

    /// Entries that could not be read, with the reason.
    pub fn errors(&self) -> &[(NodeId, Box<str>)] {
        &self.errors
    }

    /// Sets a node's category id (see [`Node::category`]).
    pub fn set_category(&mut self, id: NodeId, category: u16) {
        self.nodes[id as usize].category = category;
    }

    /// Records that `id` and everything below it were deleted: they are
    /// marked [`flags::REMOVED`] with zero sizes, and their bytes and counts
    /// are subtracted from every ancestor and from the stats. The nodes stay
    /// in the arena so ids don't change. Returns `false` (and does nothing)
    /// for the root or an already removed node.
    ///
    /// If the removed part held the counted link of a hardlinked file whose
    /// other links remain, those stay marked as duplicates until a rescan.
    pub fn remove(&mut self, id: NodeId) -> bool {
        let n = &self.nodes[id as usize];
        if id == self.root() || n.has_flag(flags::REMOVED) {
            return false;
        }
        // What the subtree adds to its parent (a duplicate link adds nothing).
        let (size, alloc, files) = if n.has_flag(flags::HARDLINK_DUP) {
            (0, 0, 0)
        } else {
            (n.size, n.alloc, n.files)
        };
        let root = self.root();
        let mut cur = n.parent;
        loop {
            let a = &mut self.nodes[cur as usize];
            a.size -= size;
            a.alloc -= alloc;
            a.files -= files;
            if cur == root {
                break;
            }
            cur = a.parent;
        }

        let mut stack = vec![id];
        while let Some(i) = stack.pop() {
            stack.extend(self.child_ids(i));
            let n = &mut self.nodes[i as usize];
            match n.kind {
                NodeKind::File => {
                    self.stats.files -= 1;
                    if n.has_flag(flags::HARDLINK_DUP) {
                        self.stats.hardlink_dups -= 1;
                    }
                }
                NodeKind::Dir => self.stats.dirs -= 1,
                NodeKind::Symlink => self.stats.symlinks -= 1,
                NodeKind::Error => self.stats.errors = self.stats.errors.saturating_sub(1),
            }
            n.flags |= flags::REMOVED;
            n.size = 0;
            n.alloc = 0;
            n.files = 0;
        }
        true
    }

    /// Replaces what folder `id` holds with `fresh`, a new scan of that same
    /// folder. The old contents stay in the arena flagged
    /// [`REMOVED`](flags::REMOVED), so ids never change; the new ones are
    /// appended, so children still come after their parents. The folder and
    /// its ancestors get the new totals. Hardlinks between the folder and the
    /// rest of the tree aren't matched up again. Returns false (and changes
    /// nothing) for a file, a removed node, or if the arena would overflow.
    pub fn replace_contents(&mut self, id: NodeId, fresh: ScanTree) -> bool {
        let n = &self.nodes[id as usize];
        if n.kind != NodeKind::Dir
            || n.has_flag(flags::REMOVED)
            || fresh.nodes.is_empty()
            || (self.nodes.len() + fresh.nodes.len()) as u64 > NodeId::MAX as u64
        {
            return false;
        }
        let (old_size, old_alloc, old_files) = (n.size, n.alloc, n.files);

        let mut stack: Vec<NodeId> = self.child_ids(id).collect();
        while let Some(i) = stack.pop() {
            stack.extend(self.child_ids(i));
            let n = &mut self.nodes[i as usize];
            if n.has_flag(flags::REMOVED) {
                continue;
            }
            match n.kind {
                NodeKind::File => {
                    self.stats.files -= 1;
                    if n.has_flag(flags::HARDLINK_DUP) {
                        self.stats.hardlink_dups -= 1;
                    }
                }
                NodeKind::Dir => self.stats.dirs -= 1,
                NodeKind::Symlink => self.stats.symlinks -= 1,
                NodeKind::Error => self.stats.errors = self.stats.errors.saturating_sub(1),
            }
            n.flags |= flags::REMOVED;
            n.size = 0;
            n.alloc = 0;
            n.files = 0;
        }

        let base = self.nodes.len() as NodeId;
        let map = |k: NodeId| if k == 0 { id } else { base + k - 1 };
        let r = &fresh.nodes[0];
        let (size, alloc, files) = (r.size, r.alloc, r.files);
        let n = &mut self.nodes[id as usize];
        n.first_child = if r.child_count > 0 {
            map(r.first_child)
        } else {
            base
        };
        n.child_count = r.child_count;
        n.size = size;
        n.alloc = alloc;
        n.files = files;
        n.modified = r.modified;
        n.flags = (n.flags & !flags::READ_ERROR) | (r.flags & flags::READ_ERROR);
        let root = self.root();
        if id != root {
            let mut cur = self.nodes[id as usize].parent;
            loop {
                let a = &mut self.nodes[cur as usize];
                a.size = a.size + size - old_size;
                a.alloc = a.alloc + alloc - old_alloc;
                a.files = a.files + files - old_files;
                if cur == root {
                    break;
                }
                cur = a.parent;
            }
        }

        let s = &fresh.stats;
        self.stats.files += s.files;
        self.stats.dirs += s.dirs.saturating_sub(1); // its root is `id`, still counted
        self.stats.symlinks += s.symlinks;
        self.stats.errors += s.errors;
        self.stats.hardlink_dups += s.hardlink_dups;
        self.errors
            .extend(fresh.errors.into_iter().map(|(i, e)| (map(i), e)));
        self.nodes
            .extend(fresh.nodes.into_iter().skip(1).map(|mut n| {
                n.parent = map(n.parent);
                n.first_child = map(n.first_child);
                n
            }));
        true
    }

    /// Leaves out the folders `exclude` matches, for a tree read all at once
    /// (the MFT): their contents are removed and they are flagged
    /// [`EXCLUDED`](flags::EXCLUDED).
    pub(crate) fn leave_out(&mut self, exclude: &crate::Exclude) {
        if exclude.is_empty() {
            return;
        }
        // Parents come first, so a folder inside a left-out one is
        // already removed when it's reached.
        for id in 1..self.nodes.len() as NodeId {
            let n = &self.nodes[id as usize];
            if n.kind != NodeKind::Dir || n.flags & (flags::REMOVED | flags::EXCLUDED) != 0 {
                continue;
            }
            if exclude.matches(&n.name, || self.path(id).display().to_string()) {
                for c in self.child_ids(id) {
                    self.remove(c);
                }
                self.nodes[id as usize].flags |= flags::EXCLUDED;
            }
        }
    }

    /// Ids of the direct children of `id`, in name order.
    pub fn child_ids(&self, id: NodeId) -> Range<NodeId> {
        let n = self.node(id);
        n.first_child..n.first_child + n.child_count
    }

    /// Ids of the direct children of `id`, largest first.
    pub fn children_by_size(&self, id: NodeId) -> Vec<NodeId> {
        let mut ids: Vec<NodeId> = self.child_ids(id).collect();
        ids.sort_by_key(|&c| Reverse(self.node(c).size));
        ids
    }

    /// Full path of a node, rebuilt from its ancestors.
    pub fn path(&self, id: NodeId) -> PathBuf {
        let mut parts = Vec::new();
        let mut cur = id;
        while cur != 0 {
            let n = self.node(cur);
            parts.push(&*n.name);
            cur = n.parent;
        }
        let mut p = PathBuf::from(&*self.nodes[0].name);
        for part in parts.iter().rev() {
            p.push(part);
        }
        p
    }

    /// Depth below the root (root = 0).
    pub fn depth(&self, id: NodeId) -> usize {
        let mut d = 0;
        let mut cur = id;
        while cur != 0 {
            cur = self.node(cur).parent;
            d += 1;
        }
        d
    }

    /// The `n` largest files anywhere in the tree, largest first.
    /// Duplicate hardlinks are skipped.
    pub fn largest_files(&self, n: usize) -> Vec<NodeId> {
        if n == 0 {
            return Vec::new();
        }
        let mut heap: BinaryHeap<Reverse<(u64, NodeId)>> = BinaryHeap::with_capacity(n + 1);
        for (i, node) in self.nodes.iter().enumerate() {
            if node.kind != NodeKind::File
                || node.flags & (flags::HARDLINK_DUP | flags::REMOVED) != 0
            {
                continue;
            }
            heap.push(Reverse((node.size, i as NodeId)));
            if heap.len() > n {
                heap.pop();
            }
        }
        let mut out: Vec<(u64, NodeId)> = heap.into_iter().map(|Reverse(x)| x).collect();
        out.sort_by(|a, b| b.cmp(a));
        out.into_iter().map(|(_, id)| id).collect()
    }

    /// Puts several scans under one made-up folder named `name` (for example
    /// "All drives"): each scan's root becomes one of its children, in the
    /// order given. The roots keep their full paths as names, so
    /// [`path`](Self::path) still gives real paths below the new root. Each
    /// scan keeps its own hardlink accounting (they are separate drives).
    pub fn combine(name: &str, trees: Vec<ScanTree>) -> ScanTree {
        let k = trees.len() as NodeId;
        let total = 1 + trees.iter().map(|t| t.nodes.len()).sum::<usize>();
        // The scans' roots go first (ids 1..=k) so the new root's children
        // are contiguous; then each scan's other nodes, in their order.
        let mut starts = Vec::with_capacity(trees.len());
        let mut next = 1 + k;
        for t in &trees {
            starts.push(next);
            next += t.nodes.len() as NodeId - 1;
        }
        let map = |j: usize, id: NodeId| {
            if id == 0 {
                1 + j as NodeId
            } else {
                starts[j] + id - 1
            }
        };

        let mut nodes = Vec::with_capacity(total);
        nodes.push(Node {
            name: name.into(),
            parent: 0,
            first_child: 1,
            child_count: k,
            kind: NodeKind::Dir,
            flags: 0,
            category: 0,
            size: 0,
            alloc: 0,
            modified: 0,
            files: 0,
        });
        for (j, t) in trees.iter().enumerate() {
            let mut r = t.nodes[0].clone();
            r.parent = 0;
            r.first_child = map(j, r.first_child);
            nodes.push(r);
        }
        let mut stats = ScanStats {
            dirs: 1,
            method: crate::ScanMethod::Mft,
            ..Default::default()
        };
        let mut errors = Vec::new();
        let mut skipped = Vec::new();
        for (j, t) in trees.into_iter().enumerate() {
            let root = &nodes[1 + j];
            let (size, alloc, files, modified) = (root.size, root.alloc, root.files, root.modified);
            let r = &mut nodes[0];
            r.size += size;
            r.alloc += alloc;
            r.files += files;
            r.modified = r.modified.max(modified);
            let s = &t.stats;
            stats.files += s.files;
            stats.dirs += s.dirs;
            stats.symlinks += s.symlinks;
            stats.errors += s.errors;
            stats.hardlink_dups += s.hardlink_dups;
            stats.elapsed += s.elapsed;
            if s.method != crate::ScanMethod::Mft {
                stats.method = crate::ScanMethod::Walk;
            }
            stats.mft_needs_admin |= s.mft_needs_admin;
            if let Some(why) = &s.mft_skipped {
                skipped.push(format!("{}: {why}", t.nodes[0].name));
            }
            errors.extend(t.errors.into_iter().map(|(id, e)| (map(j, id), e)));
            nodes.extend(t.nodes.into_iter().skip(1).map(|mut n| {
                n.parent = map(j, n.parent);
                n.first_child = map(j, n.first_child);
                n
            }));
        }
        if !skipped.is_empty() {
            stats.mft_skipped = Some(skipped.join("; "));
        }
        ScanTree {
            nodes,
            errors,
            stats,
        }
    }

    /// Fills in folder totals and marks duplicate hardlinks.
    ///
    /// Children always have larger ids than their parent, so walking the
    /// arena backwards finishes every subtree before its parent is read.
    pub(crate) fn compute_totals(&mut self, mut link_keys: Vec<(NodeId, FileKey)>) {
        // Files sharing a key are one physical file. The first in id order
        // counts; the others are marked as duplicates. Sorting by (key, id)
        // puts each file's links side by side, lowest id first, so no map
        // is needed (there can be a key for every file).
        link_keys.sort_unstable_by_key(|&(id, key)| (key, id));
        for links in link_keys.chunk_by(|a, b| a.1 == b.1) {
            let [(first, _), rest @ ..] = links else {
                continue;
            };
            if rest.is_empty() {
                continue;
            }
            self.nodes[*first as usize].flags |= flags::HARDLINKED;
            for (id, _) in rest {
                self.nodes[*id as usize].flags |= flags::HARDLINKED | flags::HARDLINK_DUP;
            }
        }
        drop(link_keys);

        let mut stats = ScanStats {
            elapsed: self.stats.elapsed,
            ..Default::default()
        };
        for node in &mut self.nodes {
            match node.kind {
                NodeKind::Dir => {
                    stats.dirs += 1;
                    node.size = 0;
                    node.alloc = 0;
                    node.files = 0;
                }
                NodeKind::File => {
                    stats.files += 1;
                    node.files = 1;
                    if node.has_flag(flags::HARDLINK_DUP) {
                        stats.hardlink_dups += 1;
                    }
                }
                NodeKind::Symlink => stats.symlinks += 1,
                NodeKind::Error => stats.errors += 1,
            }
        }
        stats.errors = stats.errors.max(self.errors.len() as u64);

        for i in (1..self.nodes.len()).rev() {
            let n = &self.nodes[i];
            let parent = n.parent as usize;
            let (size, alloc, files) = if n.has_flag(flags::HARDLINK_DUP) {
                (0, 0, 0)
            } else {
                (n.size, n.alloc, n.files)
            };
            let p = &mut self.nodes[parent];
            p.size += size;
            p.alloc += alloc;
            p.files += files;
        }
        self.stats = stats;
    }
}
