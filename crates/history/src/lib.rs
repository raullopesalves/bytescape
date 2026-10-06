//! Scan history: compact snapshots of past scans in SQLite, and "what grew"
//! comparisons against the current scan.
//!
//! A snapshot keeps every folder down to a size threshold (1 MiB, or 1/20,000
//! of the total if that is larger), so a whole drive takes a few thousand
//! rows. A comparison takes an old snapshot and the current, complete tree:
//! each folder is looked up in the tree by name, so a folder that merely fell
//! under the threshold is not reported as gone.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension};
use scanner::{flags, NodeId, NodeKind, ScanTree};

/// Snapshots kept per scanned folder; older ones are deleted.
pub const KEEP_PER_FOLDER: usize = 20;
/// Separator in stored relative paths.
const SEP: char = '\\';

/// Totals of one category at the time of a scan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CategoryRow {
    pub key: String,
    pub name: String,
    pub files: u64,
    pub size: u64,
}

/// One folder in a snapshot. `path` is relative to the scanned folder ("" for itself).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderRow {
    pub path: String,
    pub size: u64,
    pub alloc: u64,
    pub files: u64,
}

/// A past scan, summarised.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanInfo {
    pub id: i64,
    pub root: String,
    /// Seconds since 1970-01-01 UTC.
    pub taken: i64,
    pub method: String,
    pub size: u64,
    pub alloc: u64,
    pub files: u64,
    pub dirs: u64,
    /// Folders smaller than this were not stored.
    pub threshold: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub info: ScanInfo,
    pub folders: Vec<FolderRow>,
    pub categories: Vec<CategoryRow>,
}

/// The storage threshold for a scan of `total` bytes.
pub fn threshold_for(total: u64) -> u64 {
    (total / 20_000).max(1 << 20)
}

/// Builds a snapshot of `tree`. `id` is filled in by [`History::save`].
pub fn snapshot(
    tree: &ScanTree,
    taken: i64,
    method: &str,
    categories: Vec<CategoryRow>,
) -> Snapshot {
    let root = tree.node(tree.root());
    let st = tree.stats();
    let threshold = threshold_for(root.size);
    let mut folders = vec![FolderRow {
        path: String::new(),
        size: root.size,
        alloc: root.alloc,
        files: root.files,
    }];
    // A folder is never bigger than its parent, so only folders over the
    // threshold need to be opened.
    let mut stack: Vec<(NodeId, String)> = vec![(tree.root(), String::new())];
    while let Some((id, path)) = stack.pop() {
        for c in tree.child_ids(id) {
            let n = tree.node(c);
            if n.kind != NodeKind::Dir || n.has_flag(flags::REMOVED) || n.size < threshold {
                continue;
            }
            let p = join(&path, &n.name);
            folders.push(FolderRow {
                path: p.clone(),
                size: n.size,
                alloc: n.alloc,
                files: n.files,
            });
            stack.push((c, p));
        }
    }
    Snapshot {
        info: ScanInfo {
            id: 0,
            root: root.name.to_string(),
            taken,
            method: method.to_string(),
            size: root.size,
            alloc: root.alloc,
            files: st.files,
            dirs: st.dirs,
            threshold,
        },
        folders,
        categories,
    }
}

fn join(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_string()
    } else {
        format!("{parent}{SEP}{name}")
    }
}

/// How a folder changed between an old snapshot and now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub path: String,
    /// The folder in the current tree, if it still exists.
    pub node: Option<NodeId>,
    pub old: u64,
    pub new: u64,
    /// It was not in the old snapshot: new, or under the old threshold then.
    pub was_missing: bool,
}

impl Change {
    pub fn delta(&self) -> i64 {
        self.new as i64 - self.old as i64
    }
}

/// The folders that changed most between `old` and the current `tree`,
/// largest change first. A folder is left out when one of its subfolders
/// accounts for at least 90% of its change, so the list points at the most
/// specific places. Changes smaller than the old threshold are ignored.
pub fn compare(old: &Snapshot, tree: &ScanTree, limit: usize) -> Vec<Change> {
    let min_change = old.info.threshold;
    let mut changes: HashMap<String, Change> = HashMap::new();
    for f in &old.folders {
        let node = find(tree, &f.path);
        let new = node.map_or(0, |id| tree.node(id).size);
        changes.insert(
            f.path.clone(),
            Change {
                path: f.path.clone(),
                node,
                old: f.size,
                new,
                was_missing: false,
            },
        );
    }
    // Folders big enough now that weren't stored then.
    let mut stack: Vec<(NodeId, String)> = vec![(tree.root(), String::new())];
    while let Some((id, path)) = stack.pop() {
        for c in tree.child_ids(id) {
            let n = tree.node(c);
            if n.kind != NodeKind::Dir || n.has_flag(flags::REMOVED) || n.size < min_change {
                continue;
            }
            let p = join(&path, &n.name);
            changes.entry(p.clone()).or_insert_with(|| Change {
                path: p.clone(),
                node: Some(c),
                old: 0,
                new: n.size,
                was_missing: true,
            });
            stack.push((c, p));
        }
    }

    // A folder is "explained" when a subfolder changed the same way by at
    // least 90% as much. Each change only needs to look at its ancestors.
    let mut explained: HashSet<&str> = HashSet::new();
    for k in changes.values() {
        let d = k.delta();
        for anc in ancestors(&k.path) {
            if let Some(a) = changes.get(anc) {
                let ad = a.delta();
                if d.signum() == ad.signum() && d.unsigned_abs() * 10 >= ad.unsigned_abs() * 9 {
                    explained.insert(anc);
                }
            }
        }
    }
    let mut list: Vec<Change> = changes
        .values()
        .filter(|c| !c.path.is_empty()) // the total change is shown on its own
        .filter(|c| c.delta().unsigned_abs() >= min_change && !explained.contains(c.path.as_str()))
        .cloned()
        .collect();
    list.sort_by(|a, b| {
        b.delta()
            .unsigned_abs()
            .cmp(&a.delta().unsigned_abs())
            .then_with(|| a.path.cmp(&b.path))
    });
    list.truncate(limit);
    list
}

/// The folders above relative `path`, nearest first, ending with "" (the root).
fn ancestors(path: &str) -> impl Iterator<Item = &str> {
    let cuts = path.match_indices(SEP).map(|(i, _)| i).collect::<Vec<_>>();
    let root = (!path.is_empty()).then_some("");
    cuts.into_iter().rev().map(move |i| &path[..i]).chain(root)
}

/// The folder at relative `path` in `tree`. Children are sorted by name, so
/// each step is a binary search.
pub fn find(tree: &ScanTree, path: &str) -> Option<NodeId> {
    let mut cur = tree.root();
    if path.is_empty() {
        return Some(cur);
    }
    for part in path.split(SEP) {
        let ids = tree.child_ids(cur);
        let (mut lo, mut hi) = (ids.start, ids.end);
        let mut found = None;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            match (*tree.node(mid).name).cmp(part) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => {
                    found = Some(mid);
                    break;
                }
            }
        }
        cur = found?;
        let n = tree.node(cur);
        if n.kind != NodeKind::Dir || n.has_flag(flags::REMOVED) {
            return None;
        }
    }
    Some(cur)
}

/// Lower case, `/` as `\`, no trailing separator: one key per scanned folder.
fn root_key(root: &str) -> String {
    root.replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase()
}

/// The history database.
pub struct History {
    conn: Connection,
}

impl History {
    pub fn open(path: &Path) -> rusqlite::Result<History> {
        Self::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> rusqlite::Result<History> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> rusqlite::Result<History> {
        conn.execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS scans (
                 id INTEGER PRIMARY KEY,
                 root TEXT NOT NULL,
                 root_key TEXT NOT NULL,
                 taken INTEGER NOT NULL,
                 method TEXT NOT NULL,
                 size INTEGER NOT NULL,
                 alloc INTEGER NOT NULL,
                 files INTEGER NOT NULL,
                 dirs INTEGER NOT NULL,
                 threshold INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS scans_by_root ON scans (root_key, taken);
             CREATE TABLE IF NOT EXISTS folders (
                 scan INTEGER NOT NULL REFERENCES scans (id) ON DELETE CASCADE,
                 path TEXT NOT NULL,
                 size INTEGER NOT NULL,
                 alloc INTEGER NOT NULL,
                 files INTEGER NOT NULL,
                 PRIMARY KEY (scan, path)
             ) WITHOUT ROWID;
             CREATE TABLE IF NOT EXISTS categories (
                 scan INTEGER NOT NULL REFERENCES scans (id) ON DELETE CASCADE,
                 key TEXT NOT NULL,
                 name TEXT NOT NULL,
                 files INTEGER NOT NULL,
                 size INTEGER NOT NULL,
                 PRIMARY KEY (scan, key)
             ) WITHOUT ROWID;",
        )?;
        Ok(History { conn })
    }

    /// Stores a snapshot, keeps only the newest [`KEEP_PER_FOLDER`] of its
    /// folder, and returns its id.
    pub fn save(&mut self, snap: &Snapshot) -> rusqlite::Result<i64> {
        let i = &snap.info;
        let key = root_key(&i.root);
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO scans (root, root_key, taken, method, size, alloc, files, dirs, threshold)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                i.root,
                key,
                i.taken,
                i.method,
                i.size as i64,
                i.alloc as i64,
                i.files as i64,
                i.dirs as i64,
                i.threshold as i64
            ],
        )?;
        let id = tx.last_insert_rowid();
        {
            let mut f = tx.prepare(
                "INSERT INTO folders (scan, path, size, alloc, files) VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            for r in &snap.folders {
                f.execute(params![
                    id,
                    r.path,
                    r.size as i64,
                    r.alloc as i64,
                    r.files as i64
                ])?;
            }
            let mut c = tx.prepare(
                "INSERT INTO categories (scan, key, name, files, size) VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            for r in &snap.categories {
                c.execute(params![id, r.key, r.name, r.files as i64, r.size as i64])?;
            }
        }
        tx.execute(
            "DELETE FROM scans WHERE root_key = ?1 AND id NOT IN
               (SELECT id FROM scans WHERE root_key = ?1 ORDER BY taken DESC, id DESC LIMIT ?2)",
            params![key, KEEP_PER_FOLDER as i64],
        )?;
        tx.commit()?;
        Ok(id)
    }

    /// Past scans of the folder `root`, newest first.
    pub fn list(&self, root: &str) -> rusqlite::Result<Vec<ScanInfo>> {
        let mut q = self.conn.prepare(
            "SELECT id, root, taken, method, size, alloc, files, dirs, threshold
             FROM scans WHERE root_key = ?1 ORDER BY taken DESC, id DESC",
        )?;
        let rows = q.query_map(params![root_key(root)], row_info)?;
        rows.collect()
    }

    pub fn load(&self, id: i64) -> rusqlite::Result<Option<Snapshot>> {
        let info = self
            .conn
            .query_row(
                "SELECT id, root, taken, method, size, alloc, files, dirs, threshold
                 FROM scans WHERE id = ?1",
                params![id],
                row_info,
            )
            .optional()?;
        let Some(info) = info else { return Ok(None) };
        let mut q = self
            .conn
            .prepare("SELECT path, size, alloc, files FROM folders WHERE scan = ?1")?;
        let folders = q
            .query_map(params![id], |r| {
                Ok(FolderRow {
                    path: r.get(0)?,
                    size: r.get::<_, i64>(1)? as u64,
                    alloc: r.get::<_, i64>(2)? as u64,
                    files: r.get::<_, i64>(3)? as u64,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        let mut q = self.conn.prepare(
            "SELECT key, name, files, size FROM categories WHERE scan = ?1 ORDER BY size DESC",
        )?;
        let categories = q
            .query_map(params![id], |r| {
                Ok(CategoryRow {
                    key: r.get(0)?,
                    name: r.get(1)?,
                    files: r.get::<_, i64>(2)? as u64,
                    size: r.get::<_, i64>(3)? as u64,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(Some(Snapshot {
            info,
            folders,
            categories,
        }))
    }

    /// Bytes the database takes on disk, and how many scans it holds.
    pub fn usage(&self) -> rusqlite::Result<(u64, u64)> {
        let pages: i64 = self.conn.query_row("PRAGMA page_count", [], |r| r.get(0))?;
        let size: i64 = self.conn.query_row("PRAGMA page_size", [], |r| r.get(0))?;
        let scans: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM scans", [], |r| r.get(0))?;
        Ok(((pages * size) as u64, scans as u64))
    }

    /// Deletes all history and gives the space back.
    pub fn clear(&mut self) -> rusqlite::Result<()> {
        self.conn.execute("DELETE FROM scans", [])?;
        self.conn.execute_batch("VACUUM")
    }
}

fn row_info(r: &rusqlite::Row) -> rusqlite::Result<ScanInfo> {
    Ok(ScanInfo {
        id: r.get(0)?,
        root: r.get(1)?,
        taken: r.get(2)?,
        method: r.get(3)?,
        size: r.get::<_, i64>(4)? as u64,
        alloc: r.get::<_, i64>(5)? as u64,
        files: r.get::<_, i64>(6)? as u64,
        dirs: r.get::<_, i64>(7)? as u64,
        threshold: r.get::<_, i64>(8)? as u64,
    })
}
