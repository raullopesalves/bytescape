//! Disk usage scanner.
//!
//! [`scan`] walks a folder in parallel and returns a [`ScanTree`]: a flat
//! arena of [`Node`]s where every folder's children sit next to each other.
//! Folder totals are computed once after the walk, counting each hardlinked
//! file only once.
//!
//! ```no_run
//! let tree = scanner::scan("C:\\Users".as_ref(), &scanner::ScanOptions::default()).unwrap();
//! let root = tree.node(tree.root());
//! println!("{} bytes in {} files", root.size, root.files);
//! ```

mod fsinfo;
mod tree;
mod walk;

pub use tree::{flags, Node, NodeId, NodeKind, ScanStats, ScanTree};
pub use walk::scan;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

/// Settings for a scan. `ScanOptions::default()` is right for most uses.
#[derive(Clone, Debug)]
pub struct ScanOptions {
    /// Worker threads. `None` uses one per logical CPU.
    pub threads: Option<usize>,
    /// Count hardlinked files once in folder totals. Costs almost nothing,
    /// except with `fast_listing` off on Windows, where each file is opened.
    pub detect_hardlinks: bool,
    /// Windows only: list folders with one system call per folder (fast).
    /// Turn off to use the portable listing, for comparison or troubleshooting.
    pub fast_listing: bool,
    /// Live counters the caller can read while the scan runs.
    pub progress: Option<Arc<Progress>>,
    /// Set to `true` from another thread to stop the scan early.
    pub cancel: Option<Arc<AtomicBool>>,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            threads: None,
            detect_hardlinks: true,
            fast_listing: true,
            progress: None,
            cancel: None,
        }
    }
}

/// Counters updated during a scan. Read them with [`Progress::snapshot`].
#[derive(Debug, Default)]
pub struct Progress {
    pub files: AtomicU64,
    pub dirs: AtomicU64,
    pub bytes: AtomicU64,
}

impl Progress {
    /// Returns `(files, dirs, bytes)` seen so far.
    pub fn snapshot(&self) -> (u64, u64, u64) {
        (
            self.files.load(Ordering::Relaxed),
            self.dirs.load(Ordering::Relaxed),
            self.bytes.load(Ordering::Relaxed),
        )
    }
}

/// Why a scan could not run or finish.
#[derive(Debug)]
pub enum ScanError {
    /// The starting path could not be read.
    Root(std::io::Error),
    /// The starting path is not a folder.
    NotADirectory,
    /// The scan was cancelled through [`ScanOptions::cancel`].
    Cancelled,
    /// More than `u32::MAX` entries; not expected on real drives.
    TooManyNodes,
}

impl std::fmt::Display for ScanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScanError::Root(e) => write!(f, "cannot read the starting folder: {e}"),
            ScanError::NotADirectory => write!(f, "the starting path is not a folder"),
            ScanError::Cancelled => write!(f, "scan cancelled"),
            ScanError::TooManyNodes => write!(f, "too many entries to scan"),
        }
    }
}

impl std::error::Error for ScanError {}
