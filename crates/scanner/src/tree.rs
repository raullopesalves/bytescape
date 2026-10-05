use std::cmp::Reverse;
use std::collections::hash_map::Entry;
use std::collections::{BinaryHeap, HashMap};
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

    /// Fills in folder totals and marks duplicate hardlinks.
    ///
    /// Children always have larger ids than their parent, so walking the
    /// arena backwards finishes every subtree before its parent is read.
    pub(crate) fn compute_totals(&mut self, link_keys: &[(NodeId, FileKey)]) {
        // Files sharing a key are one physical file. The first in id order
        // counts; the others are marked as duplicates.
        let mut first: HashMap<FileKey, NodeId> = HashMap::with_capacity(link_keys.len());
        let mut sorted = link_keys.to_vec();
        sorted.sort_unstable_by_key(|(id, _)| *id);
        for (id, key) in sorted {
            match first.entry(key) {
                Entry::Vacant(v) => {
                    v.insert(id);
                }
                Entry::Occupied(o) => {
                    self.nodes[*o.get() as usize].flags |= flags::HARDLINKED;
                    let n = &mut self.nodes[id as usize];
                    n.flags |= flags::HARDLINKED | flags::HARDLINK_DUP;
                }
            }
        }

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
