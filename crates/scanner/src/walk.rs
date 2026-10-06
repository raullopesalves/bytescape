use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Mutex;
use std::time::Instant;

use rayon::prelude::*;

use crate::fsinfo::{self, FileKey, VolumeInfo};
use crate::mft::{self, MftError};
use crate::tree::{flags, Node, NodeId, NodeKind, ScanStats, ScanTree};
use crate::{ScanError, ScanMethod, ScanOptions};

/// The flat arena, filled while the walk runs: when a folder is listed, its
/// children are appended as one block (in name order), so every folder's
/// children are contiguous and come after it. Building it directly, instead
/// of an owned tree copied over afterwards, keeps one copy of the nodes in
/// memory instead of two. Node ids depend on the order the threads get
/// there; nothing relies on them beyond those two rules.
#[derive(Default)]
struct Arena {
    nodes: Vec<Node>,
    errors: Vec<(NodeId, Box<str>)>,
    link_keys: Vec<(NodeId, FileKey)>,
    too_many: bool,
}

struct Ctx<'a> {
    opts: &'a ScanOptions,
    exclude: crate::Exclude,
    vol: VolumeInfo,
    arena: Mutex<Arena>,
}

impl Ctx<'_> {
    fn cancelled(&self) -> bool {
        self.opts
            .cancel
            .as_ref()
            .is_some_and(|c| c.load(Ordering::Relaxed))
    }
}

fn new_node(name: Box<str>, kind: NodeKind, parent: NodeId) -> Node {
    Node {
        name,
        parent,
        first_child: 0,
        child_count: 0,
        kind,
        flags: 0,
        category: 0,
        size: 0,
        alloc: 0,
        modified: 0,
        files: 0,
    }
}

/// Scans `root` and everything below it.
///
/// Symlinks and junctions are listed but not followed. Unreadable entries
/// become error nodes instead of stopping the scan. With
/// [`ScanMethod::Auto`] a whole NTFS drive is read from its MFT when that
/// works, and walked otherwise (`stats().mft_skipped` says why).
pub fn scan(root: &Path, opts: &ScanOptions) -> Result<ScanTree, ScanError> {
    let start = Instant::now();
    let md = fs::metadata(root).map_err(ScanError::Root)?;
    if !md.is_dir() {
        return Err(ScanError::NotADirectory);
    }
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(opts.threads.unwrap_or(0))
        // Deep folder trees recurse; give workers room.
        .stack_size(16 * 1024 * 1024)
        .thread_name(|i| format!("bytescape-scan-{i}"))
        .build()
        .expect("failed to start scan threads");

    let try_mft = match opts.method {
        ScanMethod::Walk => false,
        ScanMethod::Mft => true,
        ScanMethod::Auto => mft::is_volume_root(root),
    };
    let mut mft_skipped = None;
    let mut mft_needs_admin = false;
    if try_mft {
        match pool.install(|| mft::scan(root, opts)) {
            Ok(mut tree) => {
                tree.leave_out(&crate::Exclude::new(&opts.exclude));
                tree.stats.method = ScanMethod::Mft;
                tree.stats.elapsed = start.elapsed();
                return Ok(tree);
            }
            Err(MftError::Cancelled) => return Err(ScanError::Cancelled),
            Err(e) => {
                mft_needs_admin = matches!(e, MftError::NeedsAdmin);
                mft_skipped = Some(e.to_string());
                if let Some(p) = &opts.progress {
                    for c in [&p.files, &p.dirs, &p.bytes] {
                        c.store(0, Ordering::Relaxed);
                    }
                }
            }
        }
    }

    let mut root_node = new_node(root.to_string_lossy().into(), NodeKind::Dir, 0);
    root_node.modified = fsinfo::modified_secs(&md);
    let mut arena = Arena::default();
    arena.nodes.reserve(1 << 16);
    arena.nodes.push(root_node);
    let ctx = Ctx {
        opts,
        exclude: crate::Exclude::new(&opts.exclude),
        vol: fsinfo::volume_info(root),
        arena: Mutex::new(arena),
    };
    pool.install(|| walk_dir(root, 0, &ctx));

    if ctx.cancelled() {
        return Err(ScanError::Cancelled);
    }
    let arena = ctx.arena.into_inner().unwrap();
    if arena.too_many {
        return Err(ScanError::TooManyNodes);
    }
    let mut tree = ScanTree {
        nodes: arena.nodes,
        errors: arena.errors,
        stats: ScanStats::default(),
    };
    tree.compute_totals(arena.link_keys);
    tree.stats.elapsed = start.elapsed();
    tree.stats.method = ScanMethod::Walk;
    tree.stats.mft_skipped = mft_skipped;
    tree.stats.mft_needs_admin = mft_needs_admin;
    Ok(tree)
}

/// Scans each of `drives` in turn (see [`local_drives`](crate::local_drives))
/// and puts them under one root named "All drives"
/// ([`ScanTree::combine`]). `on_start(k, drive)` is called before each one.
/// Drives that can't be read are left out; it fails only if none can be.
pub fn scan_drives(
    drives: &[PathBuf],
    opts: &ScanOptions,
    mut on_start: impl FnMut(usize, &Path),
) -> Result<ScanTree, ScanError> {
    let mut trees = Vec::new();
    let mut last = None;
    for (k, d) in drives.iter().enumerate() {
        on_start(k, d);
        match scan(d, opts) {
            Ok(t) => trees.push(t),
            Err(ScanError::Cancelled) => return Err(ScanError::Cancelled),
            Err(e) => last = Some(e),
        }
    }
    if trees.is_empty() {
        return Err(
            last.unwrap_or_else(|| ScanError::Root(std::io::Error::other("no drive to scan")))
        );
    }
    Ok(ScanTree::combine("All drives", trees))
}

/// Lists folder `id` (at `path`) into the arena, then walks its subfolders
/// in parallel.
fn walk_dir(path: &Path, id: NodeId, ctx: &Ctx) {
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
            let mut a = ctx.arena.lock().unwrap();
            a.nodes[id as usize].flags |= flags::READ_ERROR;
            a.errors.push((id, e.to_string().into()));
            return;
        }
    };
    let mut entries = listing.entries;
    // Stable order makes results (and hardlink choice) repeatable.
    entries.sort_unstable_by(|a, b| a.name.cmp(&b.name));

    let mut subdirs: Vec<(NodeId, PathBuf)> = Vec::new();
    let (mut new_files, mut new_bytes) = (0u64, 0u64);
    {
        let mut a = ctx.arena.lock().unwrap();
        if let Some(err) = listing.error {
            // The listing broke part-way; keep what we have.
            a.nodes[id as usize].flags |= flags::READ_ERROR;
            a.errors.push((id, err));
        }
        let first = a.nodes.len();
        if first + entries.len() > NodeId::MAX as usize {
            a.too_many = true;
            return;
        }
        let dir = &mut a.nodes[id as usize];
        dir.first_child = first as NodeId;
        dir.child_count = entries.len() as u32;
        for (k, e) in entries.into_iter().enumerate() {
            let cid = (first + k) as NodeId;
            if let Some(key) = e.key {
                a.link_keys.push((cid, key));
            }
            if let Some(err) = e.error {
                a.errors.push((cid, err));
            }
            match e.kind {
                NodeKind::Dir => subdirs.push((cid, e.path)),
                NodeKind::File => {
                    new_files += 1;
                    new_bytes += e.size;
                }
                _ => {}
            }
            let mut n = new_node(e.name, e.kind, id);
            n.flags = e.flags;
            n.size = e.size;
            n.alloc = e.alloc;
            n.modified = e.modified;
            a.nodes.push(n);
        }
    }
    if let Some(p) = &ctx.opts.progress {
        p.files.fetch_add(new_files, Ordering::Relaxed);
        p.bytes.fetch_add(new_bytes, Ordering::Relaxed);
    }

    subdirs.par_iter().for_each(|(cid, p)| {
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy())
            .unwrap_or_default();
        if ctx.exclude.matches(&name, || p.display().to_string()) {
            ctx.arena.lock().unwrap().nodes[*cid as usize].flags |= flags::EXCLUDED;
        } else {
            walk_dir(p, *cid, ctx);
        }
    });
}
