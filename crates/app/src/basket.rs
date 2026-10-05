//! The cleanup basket: items picked for deletion, how risky each one is,
//! and deleting them (to the Recycle Bin unless asked otherwise).
//!
//! Safety rules: links, unreadable entries, online-only cloud files and the
//! scanned folder itself are never deleted. Before deleting, the path is
//! checked again; if it changed kind since the scan, became a link, or is now
//! online-only, it is left alone.

use std::fs;
use std::io;
use std::path::Path;

use categories::{Cleanup, Rules};
use scanner::{flags, NodeId, NodeKind, ScanTree};
use serde::Serialize;

/// How careful to be with an item, from least to most.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    /// In a category that is usually safe to clean.
    Safe,
    Ok,
    /// Usually kept (a `never` category), or hardlinked.
    Warn,
    /// System or program files, or synced with OneDrive.
    Danger,
    /// Will not be deleted.
    Blocked,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Assessment {
    pub level: Level,
    pub reason: Option<String>,
}

/// Picked items in the order they were added. No item is inside another.
#[derive(Debug, Default)]
pub struct Basket {
    items: Vec<NodeId>,
}

impl Basket {
    pub fn items(&self) -> &[NodeId] {
        &self.items
    }

    /// Adds `id` unless it, or a folder above it, is already in. Items below
    /// it are dropped, since deleting it covers them. The scanned folder
    /// itself is never added. Returns whether it was added.
    pub fn add(&mut self, tree: &ScanTree, id: NodeId) -> bool {
        if id == tree.root()
            || id as usize >= tree.len()
            || tree.node(id).has_flag(flags::REMOVED)
            || self.items.iter().any(|&i| i == id || is_below(tree, id, i))
        {
            return false;
        }
        self.items.retain(|&i| !is_below(tree, i, id));
        self.items.push(id);
        true
    }

    pub fn remove(&mut self, id: NodeId) {
        self.items.retain(|&i| i != id);
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }
}

/// True if `id` is strictly below `folder`.
fn is_below(tree: &ScanTree, id: NodeId, folder: NodeId) -> bool {
    let mut cur = id;
    while cur != tree.root() {
        cur = tree.node(cur).parent;
        if cur == folder {
            return true;
        }
    }
    false
}

/// How risky deleting node `id` is, and why. `dominant` gives each folder's
/// main category.
pub fn assess(tree: &ScanTree, rules: &Rules, dominant: &[u16], id: NodeId) -> Assessment {
    let n = tree.node(id);
    let block = |r: &str| Assessment {
        level: Level::Blocked,
        reason: Some(r.into()),
    };
    if id == tree.root() {
        return block("This is the folder you scanned. Pick what's inside it instead.");
    }
    match n.kind {
        NodeKind::Symlink => {
            return block(
                "Links and junctions aren't deleted here, so what they point to stays safe.",
            )
        }
        NodeKind::Error => return block("This couldn't be read during the scan."),
        _ => {}
    }
    if n.has_flag(flags::CLOUD_PLACEHOLDER) {
        return block("Online-only file: deleting it would also delete the copy in the cloud.");
    }

    let path = tree.path(id).display().to_string();
    if in_recycle_bin(&path) {
        return block("This is in the Recycle Bin. Empty it from Windows instead.");
    }
    let danger = |r: &str| Assessment {
        level: Level::Danger,
        reason: Some(r.into()),
    };
    if is_system_path(&path) {
        return danger(
            "Part of Windows or an installed program. Deleting it can break Windows or apps.",
        );
    }
    if in_onedrive(&path) || has_placeholder_below(tree, id) {
        return danger("Synced with OneDrive: deleting it here also deletes it from OneDrive.");
    }

    let category = if n.kind == NodeKind::Dir {
        dominant.get(id as usize).copied().unwrap_or(0)
    } else {
        n.category
    };
    let cat = rules.category(category);
    if let Some(c) = cat.filter(|c| c.cleanup == Cleanup::Never) {
        let what = if n.kind == NodeKind::Dir {
            "Mostly "
        } else {
            ""
        };
        return Assessment {
            level: Level::Warn,
            reason: Some(format!("{what}{}, which is usually kept.", c.name)),
        };
    }
    if n.has_flag(flags::HARDLINKED) {
        return Assessment {
            level: Level::Warn,
            reason: Some(
                "Hardlinked: the data stays on disk while other links to it exist.".into(),
            ),
        };
    }
    if cat.is_some_and(|c| c.cleanup == Cleanup::Safe) {
        return Assessment {
            level: Level::Safe,
            reason: Some("Usually safe to clean.".into()),
        };
    }
    Assessment {
        level: Level::Ok,
        reason: None,
    }
}

/// Windows, program and system folders, and the system files at a drive's root.
fn is_system_path(path: &str) -> bool {
    let p = path.strip_prefix(r"\\?\").unwrap_or(path);
    let b = p.as_bytes();
    if b.len() < 3 || b[1] != b':' || b[2] != b'\\' {
        return false;
    }
    let rest = p[3..].to_lowercase();
    const AREAS: [&str; 6] = [
        "windows",
        "program files",
        "program files (x86)",
        "programdata",
        "system volume information",
        "recovery",
    ];
    const FILES: [&str; 4] = ["pagefile.sys", "hiberfil.sys", "swapfile.sys", "bootmgr"];
    AREAS
        .iter()
        .any(|a| rest == *a || rest.strip_prefix(a).is_some_and(|r| r.starts_with('\\')))
        || FILES.contains(&rest.as_str())
}

/// The Recycle Bin folder at a drive's root, or anything in it.
fn in_recycle_bin(path: &str) -> bool {
    let p = path.strip_prefix(r"\\?\").unwrap_or(path);
    p.get(1..3) == Some(":\\")
        && p.get(3..).is_some_and(|rest| {
            let rest = rest.to_lowercase();
            rest == "$recycle.bin" || rest.starts_with("$recycle.bin\\")
        })
}

/// Inside a OneDrive folder ("OneDrive", "OneDrive - Company", ...).
fn in_onedrive(path: &str) -> bool {
    path.split(['\\', '/'])
        .any(|part| part.to_lowercase().starts_with("onedrive"))
}

fn has_placeholder_below(tree: &ScanTree, id: NodeId) -> bool {
    let mut stack = vec![id];
    while let Some(i) = stack.pop() {
        if tree.node(i).has_flag(flags::CLOUD_PLACEHOLDER) {
            return true;
        }
        stack.extend(tree.child_ids(i));
    }
    false
}

/// Items worth suggesting for cleanup: the topmost nodes in a `safe`
/// category (a folder's contents come with it), leaving out anything rated
/// danger or blocked. Returns the `limit` largest, with the count and bytes
/// of all of them.
pub fn cleanup_candidates(
    tree: &ScanTree,
    rules: &Rules,
    dominant: &[u16],
    limit: usize,
) -> (Vec<NodeId>, usize, u64) {
    let safe = |c: u16| {
        rules
            .category(c)
            .is_some_and(|c| c.cleanup == Cleanup::Safe)
    };
    let root = tree.root();
    let mut found: Vec<(u64, NodeId)> = Vec::new();
    for (i, n) in tree.nodes().iter().enumerate().skip(1) {
        if n.flags & (flags::REMOVED | flags::HARDLINK_DUP) != 0 || n.size == 0 || !safe(n.category)
        {
            continue;
        }
        // Inside a safe folder: that folder is the candidate instead.
        if n.parent != root && safe(tree.node(n.parent).category) {
            continue;
        }
        let id = i as NodeId;
        if assess(tree, rules, dominant, id).level < Level::Danger {
            found.push((n.size, id));
        }
    }
    found.sort_unstable_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let total = found.iter().map(|f| f.0).sum();
    let count = found.len();
    found.truncate(limit);
    (found.into_iter().map(|f| f.1).collect(), count, total)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    RecycleBin,
    Permanent,
}

/// Deletes one scanned item at `path`, which was a folder if `was_dir`.
/// `owner` is the window that owns any dialog Windows shows (0 for none).
pub fn delete(path: &Path, was_dir: bool, method: Method, owner: isize) -> Result<(), String> {
    let md = fs::symlink_metadata(path).map_err(|e| match e.kind() {
        io::ErrorKind::NotFound => "It's no longer there.".to_string(),
        _ => e.to_string(),
    })?;
    if md.file_type().is_symlink() {
        return Err("It's a link now, so it was left alone. Rescan first.".into());
    }
    if md.is_dir() != was_dir {
        return Err("It changed since the scan, so it was left alone. Rescan first.".into());
    }
    if is_online_only(&md) {
        return Err("It's online-only now, so it was left alone.".into());
    }

    match method {
        Method::RecycleBin => recycle(path, owner)?,
        Method::Permanent => {
            if was_dir {
                // Removes links inside without following them.
                fs::remove_dir_all(path)
            } else {
                fs::remove_file(path)
            }
            .map_err(|e| e.to_string())?
        }
    }
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        _ => Err("It's still there after deleting (in use, or protected).".into()),
    }
}

#[cfg(windows)]
fn is_online_only(md: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    // Recall on open / on data access, or offline.
    md.file_attributes() & (0x0004_0000 | 0x0040_0000 | 0x0000_1000) != 0
}

#[cfg(not(windows))]
fn is_online_only(_md: &fs::Metadata) -> bool {
    false
}

#[cfg(windows)]
fn recycle(path: &Path, owner: isize) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::{
        SHFileOperationW, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT,
        FOF_WANTNUKEWARNING, FO_DELETE, SHFILEOPSTRUCTW,
    };

    // The shell wants a plain path, ending in two NULs, under MAX_PATH.
    let p = path.to_string_lossy();
    let p = p.strip_prefix(r"\\?\").unwrap_or(&p);
    let mut from: Vec<u16> = std::ffi::OsStr::new(p).encode_wide().collect();
    if from.len() >= 260 {
        return Err("The path is too long for the Recycle Bin.".into());
    }
    from.extend([0, 0]);
    // FOF_WANTNUKEWARNING: if something can't go to the Recycle Bin (too big,
    // or no Recycle Bin on that drive), Windows asks before deleting it for good.
    let flags =
        FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_WANTNUKEWARNING | FOF_NOERRORUI | FOF_SILENT;
    let mut op = SHFILEOPSTRUCTW {
        hwnd: owner as _,
        wFunc: FO_DELETE,
        pFrom: from.as_ptr(),
        pTo: std::ptr::null(),
        fFlags: flags as u16,
        fAnyOperationsAborted: 0,
        hNameMappings: std::ptr::null_mut(),
        lpszProgressTitle: std::ptr::null(),
    };
    // SAFETY: `op` is fully initialised and `from` outlives the call.
    let code = unsafe { SHFileOperationW(&mut op) };
    if op.fAnyOperationsAborted != 0 {
        return Err("Cancelled in the Windows prompt.".into());
    }
    match code {
        0 => Ok(()),
        5 | 0x78 => Err("Access denied.".into()),
        32 => Err("It's in use by another program.".into()),
        c => Err(format!("Windows couldn't delete it (code {c:#x}).")),
    }
}

#[cfg(not(windows))]
fn recycle(_path: &Path, _owner: isize) -> Result<(), String> {
    Err("The Recycle Bin is only supported on Windows so far.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test rules: the temp folder is under AppData\Local\Temp, which the
    /// default rules would put entirely in "Caches and temp".
    fn rules() -> Rules {
        let text = r#"
            [[category]]
            id = "pictures"
            name = "Pictures"
            cleanup = "never"
            extensions = ["jpg"]

            [[category]]
            id = "temp"
            name = "Temp"
            cleanup = "safe"
            extensions = ["tmp"]
        "#;
        Rules::load(&[("test rules", text)]).unwrap()
    }

    fn tree_of(dir: &Path) -> ScanTree {
        let mut tree = scanner::scan(dir, &scanner::ScanOptions::default()).unwrap();
        rules().apply(&mut tree);
        tree
    }

    fn find(tree: &ScanTree, rel: &str) -> NodeId {
        let mut id = tree.root();
        for part in rel.split('/') {
            id = tree
                .child_ids(id)
                .find(|&c| &*tree.node(c).name == part)
                .unwrap_or_else(|| panic!("{rel}: no {part}"));
        }
        id
    }

    #[test]
    fn adding_folds_into_folders() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        fs::create_dir_all(r.join("a/b")).unwrap();
        fs::write(r.join("a/b/x.txt"), b"x").unwrap();
        fs::write(r.join("a/y.txt"), b"y").unwrap();
        fs::write(r.join("z.txt"), b"z").unwrap();
        let tree = tree_of(r);
        let (a, b, x, y, z) = (
            find(&tree, "a"),
            find(&tree, "a/b"),
            find(&tree, "a/b/x.txt"),
            find(&tree, "a/y.txt"),
            find(&tree, "z.txt"),
        );

        let mut basket = Basket::default();
        assert!(basket.add(&tree, x));
        assert!(basket.add(&tree, y));
        assert!(basket.add(&tree, z));
        assert!(!basket.add(&tree, z), "already in");
        assert!(basket.add(&tree, a), "covers x and y");
        assert_eq!(basket.items(), [z, a]);
        assert!(!basket.add(&tree, b), "inside a");
        assert!(!basket.add(&tree, tree.root()), "never the scanned folder");
        assert_eq!(basket.items(), [z, a]);
        basket.remove(a);
        assert!(basket.add(&tree, b));
        assert_eq!(basket.items(), [z, b]);
    }

    #[test]
    fn assessments() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        fs::create_dir_all(r.join("photos")).unwrap();
        fs::create_dir_all(r.join("cache/Temp")).unwrap();
        fs::write(r.join("photos/a.jpg"), vec![0u8; 100]).unwrap();
        fs::write(r.join("cache/x.tmp"), vec![0u8; 100]).unwrap();
        fs::write(r.join("notes"), b"n").unwrap();
        fs::hard_link(r.join("notes"), r.join("notes-link")).unwrap();
        let tree = tree_of(r);
        let rules = rules();
        let dom = crate::treemap::dominant_categories(&tree, rules.categories().len());
        let level = |rel: &str| assess(&tree, &rules, &dom, find(&tree, rel)).level;

        assert_eq!(
            assess(&tree, &rules, &dom, tree.root()).level,
            Level::Blocked
        );
        assert_eq!(level("photos/a.jpg"), Level::Warn);
        assert_eq!(level("photos"), Level::Warn);
        assert_eq!(level("cache/x.tmp"), Level::Safe);
        assert_eq!(level("notes"), Level::Warn); // hardlinked
        assert_eq!(level("cache/Temp"), Level::Ok); // empty folder

        assert!(is_system_path(r"C:\Windows"));
        assert!(is_system_path(r"C:\Windows\System32\x.dll"));
        assert!(is_system_path(r"\\?\D:\Program Files (x86)\App"));
        assert!(is_system_path(r"C:\pagefile.sys"));
        assert!(!is_system_path(r"C:\WindowsApps-backup"));
        assert!(!is_system_path(r"C:\Users\r\Windows"));
        assert!(!is_system_path(r"C:\Users\r\pagefile.sys"));
        assert!(in_recycle_bin(r"D:\$Recycle.Bin"));
        assert!(in_recycle_bin(r"C:\$RECYCLE.BIN\S-1-5-21\$R1.txt"));
        assert!(!in_recycle_bin(r"C:\Users\r\$Recycle.Bin"));
        assert!(in_onedrive(r"C:\Users\r\OneDrive - Work\a.docx"));
        assert!(!in_onedrive(r"C:\Users\r\Documents\a.docx"));
    }

    #[test]
    fn cleanup_candidates_are_topmost_and_not_risky() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        fs::create_dir_all(r.join("junk/deeper")).unwrap();
        fs::create_dir_all(r.join("work")).unwrap();
        fs::create_dir_all(r.join("OneDrive/stuff")).unwrap();
        fs::write(r.join("junk/a.tmp"), vec![0u8; 300]).unwrap();
        fs::write(r.join("junk/deeper/b.tmp"), vec![0u8; 200]).unwrap();
        fs::write(r.join("work/c.tmp"), vec![0u8; 100]).unwrap();
        fs::write(r.join("work/keep.jpg"), vec![0u8; 900]).unwrap();
        fs::write(r.join("top.tmp"), vec![0u8; 50]).unwrap();
        fs::write(r.join("OneDrive/stuff/d.tmp"), vec![0u8; 999]).unwrap();
        // Rules where the "junk" folder is a safe category by path.
        let text = r#"
            [[category]]
            id = "temp"
            name = "Temp"
            cleanup = "safe"
            paths = ["**/junk/**"]
            extensions = ["tmp"]

            [[category]]
            id = "pictures"
            name = "Pictures"
            cleanup = "never"
            extensions = ["jpg"]
        "#;
        let rules = Rules::load(&[("test rules", text)]).unwrap();
        let mut tree = scanner::scan(r, &scanner::ScanOptions::default()).unwrap();
        rules.apply(&mut tree);
        let dom = crate::treemap::dominant_categories(&tree, rules.categories().len());

        let (ids, count, bytes) = cleanup_candidates(&tree, &rules, &dom, 10);
        let names: Vec<&str> = ids.iter().map(|&i| &*tree.node(i).name).collect();
        // junk (500 B) covers its contents; OneDrive's file is left out.
        assert_eq!(names, ["junk", "c.tmp", "top.tmp"]);
        assert_eq!((count, bytes), (3, 650));
        let (ids, count, _) = cleanup_candidates(&tree, &rules, &dom, 1);
        assert_eq!((ids.len(), count), (1, 3));
    }

    #[test]
    fn permanent_delete_leaves_link_targets_alone() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        fs::create_dir_all(r.join("victim/sub")).unwrap();
        fs::create_dir_all(r.join("target")).unwrap();
        fs::write(r.join("victim/sub/a.bin"), vec![1u8; 10]).unwrap();
        fs::write(r.join("target/keep.txt"), b"keep").unwrap();
        make_dir_link(&r.join("victim").join("link"), &r.join("target"));
        fs::write(r.join("file.txt"), b"f").unwrap();

        delete(&r.join("victim"), true, Method::Permanent, 0).unwrap();
        assert!(!r.join("victim").exists());
        assert_eq!(fs::read(r.join("target/keep.txt")).unwrap(), b"keep");

        // The item must still be what the scan saw.
        assert!(delete(&r.join("file.txt"), true, Method::Permanent, 0).is_err());
        assert!(r.join("file.txt").exists());
        make_dir_link(&r.join("now-a-link"), &r.join("target"));
        let err = delete(&r.join("now-a-link"), true, Method::Permanent, 0).unwrap_err();
        assert!(err.contains("link"), "{err}");
        assert!(r.join("now-a-link").exists());
        assert!(delete(&r.join("missing"), false, Method::Permanent, 0).is_err());

        delete(&r.join("file.txt"), false, Method::Permanent, 0).unwrap();
        assert!(!r.join("file.txt").exists());
    }

    /// Run by hand (`cargo test -p diskmap-app -- --ignored`): it puts a small
    /// test folder in the Recycle Bin.
    #[test]
    #[ignore]
    #[cfg(windows)]
    fn recycle_bin_delete_leaves_link_targets_alone() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        fs::create_dir_all(r.join("diskmap-recycle-test")).unwrap();
        fs::create_dir_all(r.join("target")).unwrap();
        fs::write(r.join("diskmap-recycle-test/a.bin"), vec![1u8; 10]).unwrap();
        fs::write(r.join("target/keep.txt"), b"keep").unwrap();
        make_dir_link(
            &r.join("diskmap-recycle-test").join("link"),
            &r.join("target"),
        );

        delete(&r.join("diskmap-recycle-test"), true, Method::RecycleBin, 0).unwrap();
        assert!(!r.join("diskmap-recycle-test").exists());
        assert_eq!(fs::read(r.join("target/keep.txt")).unwrap(), b"keep");
    }

    /// A junction on Windows (no admin rights needed), a symlink elsewhere.
    fn make_dir_link(link: &Path, target: &Path) {
        #[cfg(windows)]
        {
            let out = std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .output()
                .unwrap();
            assert!(out.status.success(), "mklink failed: {out:?}");
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, link).unwrap();
    }
}
