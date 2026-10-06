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
mod mft;
mod tree;
mod walk;

pub use fsinfo::local_drives;
pub use tree::{flags, Node, NodeId, NodeKind, ScanStats, ScanTree};
pub use walk::{scan, scan_drives};

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

/// How to read the folder tree.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ScanMethod {
    /// The MFT for a whole NTFS drive when that works (administrator
    /// rights), otherwise the folder walk.
    #[default]
    Auto,
    /// List folders one by one, in parallel. Works everywhere.
    Walk,
    /// Read the NTFS Master File Table, for any folder on an NTFS drive;
    /// falls back to the walk if it can't. Needs administrator rights.
    Mft,
}

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
    pub method: ScanMethod,
    /// Folders to leave out: listed, flagged
    /// [`EXCLUDED`](flags::EXCLUDED), their contents not read. An entry
    /// with a `\` or `/` is a full path; anything else is a folder name,
    /// matched anywhere. Case is ignored. The scanned folder itself is
    /// never left out.
    pub exclude: Vec<String>,
}

/// [`ScanOptions::exclude`], ready for matching.
#[derive(Default)]
pub(crate) struct Exclude {
    names: Vec<String>,
    /// Lower case, `\` separators, no trailing separator.
    paths: Vec<String>,
}

impl Exclude {
    pub(crate) fn new(list: &[String]) -> Self {
        let mut e = Exclude::default();
        for item in list.iter().map(|s| s.trim()).filter(|s| !s.is_empty()) {
            let s = item.to_lowercase().replace('/', "\\");
            if s.contains('\\') {
                e.paths.push(s.trim_end_matches('\\').to_string());
            } else {
                e.names.push(s);
            }
        }
        e
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.names.is_empty() && self.paths.is_empty()
    }

    /// Whether the folder `name` at `path` is left out. `path` is only
    /// built when there are path entries.
    pub(crate) fn matches(&self, name: &str, path: impl FnOnce() -> String) -> bool {
        if self
            .names
            .iter()
            .any(|n| name.eq_ignore_ascii_case(n) || name.to_lowercase() == *n)
        {
            return true;
        }
        if self.paths.is_empty() {
            return false;
        }
        let p = path().to_lowercase().replace('/', "\\");
        let p = p.strip_prefix(r"\\?\").unwrap_or(&p);
        self.paths.iter().any(|x| p.trim_end_matches('\\') == x)
    }
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            threads: None,
            detect_hardlinks: true,
            fast_listing: true,
            progress: None,
            cancel: None,
            method: ScanMethod::Auto,
            exclude: Vec::new(),
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
