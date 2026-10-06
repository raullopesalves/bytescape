//! Duplicate files: files with identical contents in a scanned tree.
//!
//! Found in stages so most files are never read:
//!
//! 1. Group by size (files under `min_size` are skipped): [`candidates`].
//! 2. For files that share a size, hash the first and last 16 KiB.
//! 3. Where those still match, hash the whole file: [`find`].
//!
//! BLAKE3 is a cryptographic hash, so equal hashes mean equal contents
//! without comparing bytes. Only one link of a hardlinked file is considered
//! (links to the same file are not duplicates), online-only cloud files are
//! never opened (that would download them), and a file whose size or
//! modified time differs from the scan, or changes while it is read, is
//! left out.
//!
//! [`candidates`] needs the tree; [`find`] doesn't, so a caller can release
//! the tree while files are read.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use rayon::prelude::*;
use scanner::{flags, NodeId, NodeKind, ScanTree};

/// Bytes hashed from each end of a file in the second stage.
pub const EDGE: u64 = 16 * 1024;

#[derive(Clone, Debug)]
pub struct Options {
    /// Worker threads (they run at background priority on Windows).
    pub threads: usize,
    pub progress: Option<Arc<Progress>>,
    pub cancel: Option<Arc<AtomicBool>>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            threads: 4,
            progress: None,
            cancel: None,
        }
    }
}

/// Live counters. `stage` is 2 (file ends) or 3 (whole files).
#[derive(Debug, Default)]
pub struct Progress {
    pub stage: AtomicU8,
    pub files_done: AtomicU64,
    pub files_total: AtomicU64,
    pub bytes_read: AtomicU64,
}

/// A file that shares its size with another one.
#[derive(Clone, Debug)]
pub struct Candidate {
    pub id: NodeId,
    pub size: u64,
    /// As the scan saw it, to notice files that changed since.
    pub modified: i64,
    pub path: PathBuf,
}

/// The result of stage 1.
#[derive(Clone, Debug, Default)]
pub struct Candidates {
    /// Files sharing a size with at least one other.
    pub files: Vec<Candidate>,
    /// All files at least the minimum size, and their bytes.
    pub count: u64,
    pub bytes: u64,
}

/// Files with the same contents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    /// Size of each copy.
    pub size: u64,
    pub files: Vec<NodeId>,
}

impl Group {
    /// Space the extra copies take.
    pub fn wasted(&self) -> u64 {
        self.size * (self.files.len() as u64).saturating_sub(1)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// Largest wasted space first.
    pub groups: Vec<Group>,
    /// Files at least the minimum size, and their bytes.
    pub candidates: u64,
    pub candidate_bytes: u64,
    /// Bytes actually read from disk.
    pub bytes_read: u64,
    /// Files left out: changed since the scan or while being read.
    pub changed: u64,
    /// Files that couldn't be read.
    pub unreadable: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum DupesError {
    Cancelled,
}

/// Stage 1: the files at least `min_size` that share a size with another.
pub fn candidates(tree: &ScanTree, min_size: u64) -> Candidates {
    let mut out = Candidates::default();
    let mut by_size: HashMap<u64, Vec<NodeId>> = HashMap::new();
    let skip = flags::HARDLINK_DUP | flags::REMOVED | flags::CLOUD_PLACEHOLDER;
    for (i, n) in tree.nodes().iter().enumerate() {
        if n.kind == NodeKind::File && n.flags & skip == 0 && n.size >= min_size.max(1) {
            out.count += 1;
            out.bytes += n.size;
            by_size.entry(n.size).or_default().push(i as NodeId);
        }
    }
    for ids in by_size.into_values().filter(|g| g.len() > 1) {
        for id in ids {
            let n = tree.node(id);
            out.files.push(Candidate {
                id,
                size: n.size,
                modified: n.modified,
                path: tree.path(id),
            });
        }
    }
    out
}

/// Stages 2 and 3: reads what is needed of `c.files` to find true copies.
pub fn find(c: Candidates, opts: &Options) -> Result<Report, DupesError> {
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(opts.threads.max(1))
        .thread_name(|i| format!("bytescape-dupes-{i}"))
        .start_handler(|_| background_priority())
        .build()
        .expect("failed to start duplicate-finder threads");
    pool.install(|| find_in(c, opts))
}

fn find_in(c: Candidates, opts: &Options) -> Result<Report, DupesError> {
    let progress = opts.progress.clone().unwrap_or_default();
    let cancelled = || {
        opts.cancel
            .as_ref()
            .is_some_and(|c| c.load(Ordering::Relaxed))
    };
    let mut report = Report {
        candidates: c.count,
        candidate_bytes: c.bytes,
        ..Report::default()
    };
    let files: HashMap<NodeId, Candidate> = c.files.into_iter().map(|f| (f.id, f)).collect();
    let mut by_size: HashMap<u64, Vec<NodeId>> = HashMap::new();
    for f in files.values() {
        by_size.entry(f.size).or_default().push(f.id);
    }
    let mut groups: Vec<Vec<NodeId>> = by_size.into_values().filter(|g| g.len() > 1).collect();

    for (stage, whole) in [(2u8, false), (3u8, true)] {
        if cancelled() {
            return Err(DupesError::Cancelled);
        }
        // Files up to two ends long are read whole in stage 2 already.
        let todo: Vec<&Candidate> = groups
            .iter()
            .flatten()
            .map(|id| &files[id])
            .filter(|f| !whole || f.size > 2 * EDGE)
            .collect();
        progress.stage.store(stage, Ordering::Relaxed);
        progress
            .files_total
            .store(todo.len() as u64, Ordering::Relaxed);
        progress.files_done.store(0, Ordering::Relaxed);
        let hashes: HashMap<NodeId, Result<[u8; 32], Skip>> = todo
            .par_iter()
            .map(|f| {
                let r = if cancelled() {
                    Err(Skip::Cancelled)
                } else {
                    hash_file(f, whole, &progress)
                };
                progress.files_done.fetch_add(1, Ordering::Relaxed);
                (f.id, r)
            })
            .collect();
        if cancelled() {
            return Err(DupesError::Cancelled);
        }
        let mut next = Vec::new();
        for g in groups {
            let mut by_hash: HashMap<[u8; 32], Vec<NodeId>> = HashMap::new();
            for id in g {
                match hashes.get(&id) {
                    // Not read in this stage: compared whole already.
                    None => by_hash.entry([0; 32]).or_default().push(id),
                    Some(Ok(h)) => by_hash.entry(*h).or_default().push(id),
                    Some(Err(Skip::Changed)) => report.changed += 1,
                    Some(Err(Skip::Unreadable)) => report.unreadable += 1,
                    Some(Err(Skip::Cancelled)) => return Err(DupesError::Cancelled),
                }
            }
            next.extend(by_hash.into_values().filter(|g| g.len() > 1));
        }
        groups = next;
    }

    report.bytes_read = progress.bytes_read.load(Ordering::Relaxed);
    report.groups = groups
        .into_iter()
        .map(|mut ids| {
            ids.sort_unstable();
            Group {
                size: files[&ids[0]].size,
                files: ids,
            }
        })
        .collect();
    report.groups.sort_by(|a, b| {
        b.wasted()
            .cmp(&a.wasted())
            .then(a.files[0].cmp(&b.files[0]))
    });
    Ok(report)
}

enum Skip {
    Changed,
    Unreadable,
    Cancelled,
}

/// Hashes a file's two ends (`whole == false`) or all of it, after checking
/// it is still what the scan saw, and again afterwards.
fn hash_file(f: &Candidate, whole: bool, progress: &Progress) -> Result<[u8; 32], Skip> {
    let before = stamp(&f.path).map_err(|_| Skip::Unreadable)?;
    if before.0 != f.size || before.1 != f.modified || before.2 {
        return Err(Skip::Changed);
    }
    let mut file = open(&f.path).map_err(|_| Skip::Unreadable)?;
    let mut h = blake3::Hasher::new();
    let mut read = 0u64;
    let result = if whole || f.size <= 2 * EDGE {
        copy_into(&mut file, &mut h, u64::MAX, &mut read)
    } else {
        copy_into(&mut file, &mut h, EDGE, &mut read)
            .and_then(|_| file.seek(SeekFrom::End(-(EDGE as i64))).map(|_| ()))
            .and_then(|_| copy_into(&mut file, &mut h, EDGE, &mut read))
    };
    progress.bytes_read.fetch_add(read, Ordering::Relaxed);
    result.map_err(|_| Skip::Unreadable)?;
    if stamp(&f.path).map_err(|_| Skip::Unreadable)? != before {
        return Err(Skip::Changed);
    }
    Ok(*h.finalize().as_bytes())
}

fn copy_into(f: &mut File, h: &mut blake3::Hasher, limit: u64, read: &mut u64) -> io::Result<()> {
    let mut buf = vec![0u8; 1 << 20];
    let mut left = limit;
    while left > 0 {
        let want = buf.len().min(left.min(usize::MAX as u64) as usize);
        let got = f.read(&mut buf[..want])?;
        if got == 0 {
            break;
        }
        h.update(&buf[..got]);
        *read += got as u64;
        left -= got as u64;
    }
    Ok(())
}

/// Size, modified time (seconds, as the scan stores it) and whether the file
/// is online-only now.
fn stamp(path: &Path) -> io::Result<(u64, i64, bool)> {
    let md = std::fs::symlink_metadata(path)?;
    let modified = md
        .modified()
        .ok()
        .map(|t| match t.duration_since(UNIX_EPOCH) {
            Ok(d) => d.as_secs() as i64,
            Err(e) => -(e.duration().as_secs() as i64),
        })
        .unwrap_or(0);
    Ok((md.len(), modified, online_only(&md)))
}

#[cfg(windows)]
fn online_only(md: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    md.file_attributes() & (0x0004_0000 | 0x0040_0000 | 0x0000_1000) != 0
}

#[cfg(not(windows))]
fn online_only(_md: &std::fs::Metadata) -> bool {
    false
}

#[cfg(windows)]
fn open(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_FLAG_SEQUENTIAL_SCAN: u32 = 0x0800_0000;
    OpenOptions::new()
        .read(true)
        .share_mode(7) // others may read, write and delete meanwhile
        .custom_flags(FILE_FLAG_SEQUENTIAL_SCAN)
        .open(path)
}

#[cfg(not(windows))]
fn open(path: &Path) -> io::Result<File> {
    OpenOptions::new().read(true).open(path)
}

/// Lowers this thread's CPU and disk priority, so hashing doesn't get in
/// the way of what the user is doing.
#[cfg(windows)]
fn background_priority() {
    use windows_sys::Win32::System::Threading::{
        GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN,
    };
    // SAFETY: the pseudo handle for the current thread needs no closing.
    unsafe { SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_BEGIN) };
}

#[cfg(not(windows))]
fn background_priority() {}
