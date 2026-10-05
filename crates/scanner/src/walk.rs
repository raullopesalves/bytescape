use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::Instant;

use rayon::prelude::*;

use crate::fsinfo::{self, FileKey, VolumeInfo};
use crate::tree::{flags, Node, NodeId, NodeKind, ScanStats, ScanTree};
use crate::{ScanError, ScanOptions};

/// Owned node built during the walk, flattened into the arena afterwards.
struct Raw {
    name: Box<str>,
    kind: NodeKind,
    flags: u8,
    size: u64,
    alloc: u64,
    modified: i64,
    key: Option<FileKey>,
    error: Option<Box<str>>,
    children: Vec<Raw>,
}

impl Raw {
    fn new(name: Box<str>, kind: NodeKind) -> Self {
        Raw {
            name,
            kind,
            flags: 0,
            size: 0,
            alloc: 0,
            modified: 0,
            key: None,
            error: None,
            children: Vec::new(),
        }
    }
}

struct Ctx<'a> {
    opts: &'a ScanOptions,
    vol: VolumeInfo,
}

impl Ctx<'_> {
    fn cancelled(&self) -> bool {
        self.opts
            .cancel
            .as_ref()
            .is_some_and(|c| c.load(Ordering::Relaxed))
    }
}

/// Scans `root` and everything below it.
///
/// Symlinks and junctions are listed but not followed. Unreadable entries
/// become error nodes instead of stopping the scan.
pub fn scan(root: &Path, opts: &ScanOptions) -> Result<ScanTree, ScanError> {
    let start = Instant::now();
    let md = fs::metadata(root).map_err(ScanError::Root)?;
    if !md.is_dir() {
        return Err(ScanError::NotADirectory);
    }
    let ctx = Ctx {
        opts,
        vol: fsinfo::volume_info(root),
    };

    let mut raw_root = Raw::new(root.to_string_lossy().into(), NodeKind::Dir);
    raw_root.modified = fsinfo::modified_secs(&md);

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(opts.threads.unwrap_or(0))
        // Deep folder trees recurse; give workers room.
        .stack_size(16 * 1024 * 1024)
        .thread_name(|i| format!("diskmap-scan-{i}"))
        .build()
        .expect("failed to start scan threads");
    pool.install(|| walk_dir(root, &mut raw_root, &ctx));

    if ctx.cancelled() {
        return Err(ScanError::Cancelled);
    }

    let mut tree = flatten(raw_root)?;
    let keys = std::mem::take(&mut tree.link_keys);
    tree.tree.compute_totals(&keys);
    tree.tree.stats.elapsed = start.elapsed();
    Ok(tree.tree)
}

/// Lists one folder into `dir.children`, then recurses into subfolders in parallel.
fn walk_dir(path: &Path, dir: &mut Raw, ctx: &Ctx) {
    if ctx.cancelled() {
        return;
    }
    if let Some(p) = &ctx.opts.progress {
        p.dirs.fetch_add(1, Ordering::Relaxed);
    }

    let listing = match fsinfo::list_dir(
        path,
        &ctx.vol,
        ctx.opts.detect_hardlinks,
        ctx.opts.fast_listing,
    ) {
        Ok(l) => l,
        Err(e) => {
            dir.flags |= flags::READ_ERROR;
            dir.error = Some(e.to_string().into());
            return;
        }
    };
    if let Some(err) = listing.error {
        // The listing broke part-way; keep what we have.
        dir.flags |= flags::READ_ERROR;
        dir.error = Some(err);
    }

    let mut subdirs: Vec<(Raw, PathBuf)> = Vec::new();
    let mut files: Vec<Raw> = Vec::new();
    let mut new_files = 0u64;
    let mut new_bytes = 0u64;

    for e in listing.entries {
        let mut r = Raw::new(e.name, e.kind);
        r.size = e.size;
        r.alloc = e.alloc;
        r.modified = e.modified;
        r.key = e.key;
        r.flags = e.flags;
        r.error = e.error;
        match e.kind {
            NodeKind::Dir => subdirs.push((r, e.path)),
            NodeKind::File => {
                new_files += 1;
                new_bytes += r.size;
                files.push(r);
            }
            _ => files.push(r),
        }
    }

    if let Some(p) = &ctx.opts.progress {
        p.files.fetch_add(new_files, Ordering::Relaxed);
        p.bytes.fetch_add(new_bytes, Ordering::Relaxed);
    }

    subdirs
        .par_iter_mut()
        .for_each(|(raw, p)| walk_dir(p, raw, ctx));

    let mut children = files;
    children.extend(subdirs.into_iter().map(|(r, _)| r));
    // Stable order makes results (and hardlink choice) repeatable.
    children.sort_unstable_by(|a, b| a.name.cmp(&b.name));
    dir.children = children;
}

struct Flat {
    tree: ScanTree,
    link_keys: Vec<(NodeId, FileKey)>,
}

/// Breadth-first copy into the arena so each folder's children are contiguous.
fn flatten(root: Raw) -> Result<Flat, ScanError> {
    let mut nodes: Vec<Node> = Vec::new();
    let mut errors = Vec::new();
    let mut link_keys = Vec::new();

    let mut queue: VecDeque<(Raw, NodeId)> = VecDeque::new();
    let mut root = root;
    nodes.push(to_node(&mut root, 0));
    queue.push_back((root, 0));

    while let Some((mut raw, id)) = queue.pop_front() {
        if let Some(err) = raw.error.take() {
            errors.push((id, err));
        }
        if let Some(key) = raw.key {
            link_keys.push((id, key));
        }
        let children = std::mem::take(&mut raw.children);
        if children.is_empty() {
            continue;
        }
        let first = u32::try_from(nodes.len()).map_err(|_| ScanError::TooManyNodes)?;
        let count = u32::try_from(children.len()).map_err(|_| ScanError::TooManyNodes)?;
        first.checked_add(count).ok_or(ScanError::TooManyNodes)?;
        {
            let n = &mut nodes[id as usize];
            n.first_child = first;
            n.child_count = count;
        }
        for (i, mut child) in children.into_iter().enumerate() {
            let cid = first + i as u32;
            nodes.push(to_node(&mut child, id));
            queue.push_back((child, cid));
        }
    }

    Ok(Flat {
        tree: ScanTree {
            nodes,
            errors,
            stats: ScanStats::default(),
        },
        link_keys,
    })
}

fn to_node(raw: &mut Raw, parent: NodeId) -> Node {
    Node {
        name: std::mem::take(&mut raw.name),
        parent,
        first_child: 0,
        child_count: 0,
        kind: raw.kind,
        flags: raw.flags,
        category: 0,
        size: raw.size,
        alloc: raw.alloc,
        modified: raw.modified,
        files: 0,
    }
}
