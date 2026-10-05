use std::fs;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use scanner::{flags, scan, Progress, ScanError, ScanOptions, ScanTree};

fn write(path: &Path, len: usize) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, vec![7u8; len]).unwrap();
}

/// Fixture:
/// root/
///   a/one.bin        1000
///   a/b/two.bin      5000
///   a/b/loop  ->  ..  (symlink to a folder, must not be followed)
///   c/empty.bin      0
///   c/hard.bin       hardlink to a/one.bin
///   top.txt          10
fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let r = dir.path();
    write(&r.join("a/one.bin"), 1000);
    write(&r.join("a/b/two.bin"), 5000);
    write(&r.join("c/empty.bin"), 0);
    write(&r.join("top.txt"), 10);
    fs::hard_link(r.join("a/one.bin"), r.join("c/hard.bin")).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("..", r.join("a/b/loop")).unwrap();
    dir
}

fn find(tree: &ScanTree, rel: &str) -> u32 {
    let mut id = tree.root();
    for part in rel.split('/') {
        id = tree
            .child_ids(id)
            .find(|&c| &*tree.node(c).name == part)
            .unwrap_or_else(|| panic!("{rel}: missing {part}"));
    }
    id
}

#[test]
fn totals_count_hardlinks_once() {
    let dir = fixture();
    let tree = scan(dir.path(), &ScanOptions::default()).unwrap();
    let root = tree.node(tree.root());
    assert_eq!(root.size, 1000 + 5000 + 10);
    assert_eq!(root.files, 4); // one, two, empty, top (hard.bin is a duplicate)
    assert_eq!(tree.stats().hardlink_dups, 1);

    // Exactly one of the two links is marked as the duplicate.
    let one = tree.node(find(&tree, "a/one.bin"));
    let hard = tree.node(find(&tree, "c/hard.bin"));
    assert!(one.has_flag(flags::HARDLINKED) && hard.has_flag(flags::HARDLINKED));
    assert_ne!(
        one.has_flag(flags::HARDLINK_DUP),
        hard.has_flag(flags::HARDLINK_DUP)
    );
    // Name order decides: "a" comes before "c", so the copy in c is the duplicate.
    assert!(hard.has_flag(flags::HARDLINK_DUP));
    assert_eq!(tree.node(find(&tree, "c")).size, 0);
    assert_eq!(tree.node(find(&tree, "a")).size, 6000);
}

#[test]
fn hardlinks_counted_twice_when_detection_off() {
    let dir = fixture();
    let opts = ScanOptions {
        detect_hardlinks: false,
        ..Default::default()
    };
    let tree = scan(dir.path(), &opts).unwrap();
    assert_eq!(tree.node(tree.root()).size, 7010);
    assert_eq!(tree.stats().hardlink_dups, 0);
}

#[cfg(unix)]
#[test]
fn symlinks_are_listed_not_followed() {
    let dir = fixture();
    let tree = scan(dir.path(), &ScanOptions::default()).unwrap();
    let link = tree.node(find(&tree, "a/b/loop"));
    assert_eq!(link.kind, scanner::NodeKind::Symlink);
    assert_eq!(link.child_count, 0);
    assert_eq!(link.size, 0);
    assert_eq!(tree.stats().symlinks, 1);
}

#[test]
fn children_are_contiguous_sorted_and_linked_to_parent() {
    let dir = fixture();
    let tree = scan(dir.path(), &ScanOptions::default()).unwrap();
    for id in 0..tree.len() as u32 {
        let names: Vec<&str> = tree.child_ids(id).map(|c| &*tree.node(c).name).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
        for c in tree.child_ids(id) {
            assert_eq!(tree.node(c).parent, id);
            assert!(c > id, "children must come after their parent");
        }
    }
    let root_names: Vec<&str> = tree.child_ids(0).map(|c| &*tree.node(c).name).collect();
    assert_eq!(root_names, ["a", "c", "top.txt"]);
}

#[test]
fn folder_totals_equal_sum_of_children() {
    let dir = fixture();
    let tree = scan(dir.path(), &ScanOptions::default()).unwrap();
    for id in 0..tree.len() as u32 {
        let n = tree.node(id);
        if !n.is_dir() {
            continue;
        }
        let (mut size, mut alloc, mut files) = (0, 0, 0);
        for c in tree.child_ids(id) {
            let c = tree.node(c);
            if !c.has_flag(flags::HARDLINK_DUP) {
                size += c.size;
                alloc += c.alloc;
                files += c.files;
            }
        }
        assert_eq!(
            (n.size, n.alloc, n.files),
            (size, alloc, files),
            "{}",
            n.name
        );
    }
}

#[test]
fn paths_round_trip() {
    let dir = fixture();
    let tree = scan(dir.path(), &ScanOptions::default()).unwrap();
    let id = find(&tree, "a/b/two.bin");
    assert_eq!(
        tree.path(id),
        dir.path().join("a").join("b").join("two.bin")
    );
    assert_eq!(tree.depth(id), 3);
    assert_eq!(tree.path(tree.root()), dir.path());
}

#[test]
fn largest_files_skips_duplicates() {
    let dir = fixture();
    let tree = scan(dir.path(), &ScanOptions::default()).unwrap();
    let names: Vec<&str> = tree
        .largest_files(3)
        .into_iter()
        .map(|id| &*tree.node(id).name)
        .collect();
    assert_eq!(names, ["two.bin", "one.bin", "top.txt"]);
    assert!(tree.largest_files(0).is_empty());
}

#[test]
fn children_by_size_is_descending() {
    let dir = fixture();
    let tree = scan(dir.path(), &ScanOptions::default()).unwrap();
    let names: Vec<&str> = tree
        .children_by_size(0)
        .into_iter()
        .map(|id| &*tree.node(id).name)
        .collect();
    assert_eq!(names[0], "a");
}

#[cfg(unix)]
#[test]
fn size_on_disk_comes_from_allocated_blocks() {
    let dir = tempfile::tempdir().unwrap();
    // A sparse file: 10 MB long, nothing written.
    let f = fs::File::create(dir.path().join("sparse.img")).unwrap();
    f.set_len(10 * 1024 * 1024).unwrap();
    drop(f);
    let tree = scan(dir.path(), &ScanOptions::default()).unwrap();
    let n = tree.node(find(&tree, "sparse.img"));
    assert_eq!(n.size, 10 * 1024 * 1024);
    assert!(n.alloc < n.size);
    assert!(n.has_flag(flags::SPARSE));
}

#[cfg(unix)]
#[test]
fn unreadable_folder_is_flagged_not_fatal() {
    use std::os::unix::fs::PermissionsExt;
    let dir = fixture();
    let locked = dir.path().join("locked");
    write(&locked.join("secret.bin"), 50);
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    // Root ignores permissions, so there is nothing to test there.
    let readable = fs::read_dir(&locked).is_ok();
    let tree = scan(dir.path(), &ScanOptions::default());
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    if readable {
        eprintln!("skipped: running with permission to read everything");
        return;
    }
    let tree = tree.unwrap();
    let n = tree.node(find(&tree, "locked"));
    assert!(n.has_flag(flags::READ_ERROR));
    assert_eq!(tree.errors().len(), 1);
    assert_eq!(tree.node(tree.root()).size, 6010);
}

#[test]
fn deep_nesting_does_not_overflow() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = dir.path().to_path_buf();
    for _ in 0..300 {
        p.push("d");
    }
    write(&p.join("leaf.bin"), 42);
    let tree = scan(dir.path(), &ScanOptions::default()).unwrap();
    assert_eq!(tree.node(0).size, 42);
    assert_eq!(tree.stats().dirs, 301);
}

#[test]
fn many_files_in_parallel() {
    let dir = tempfile::tempdir().unwrap();
    for d in 0..20 {
        for f in 0..50 {
            write(&dir.path().join(format!("d{d}/f{f}.bin")), f);
        }
    }
    let progress = Arc::new(Progress::default());
    let opts = ScanOptions {
        progress: Some(progress.clone()),
        threads: Some(4),
        ..Default::default()
    };
    let tree = scan(dir.path(), &opts).unwrap();
    let expected: u64 = 20 * (0..50u64).sum::<u64>();
    assert_eq!(tree.node(0).size, expected);
    assert_eq!(tree.node(0).files, 1000);
    let (files, dirs, bytes) = progress.snapshot();
    assert_eq!((files, dirs, bytes), (1000, 21, expected));
}

#[test]
fn cancelled_scan_returns_error() {
    let dir = fixture();
    let opts = ScanOptions {
        cancel: Some(Arc::new(AtomicBool::new(true))),
        ..Default::default()
    };
    assert!(matches!(scan(dir.path(), &opts), Err(ScanError::Cancelled)));
}

#[test]
fn bad_roots_are_errors() {
    let dir = fixture();
    assert!(matches!(
        scan(&dir.path().join("top.txt"), &ScanOptions::default()),
        Err(ScanError::NotADirectory)
    ));
    assert!(matches!(
        scan(&dir.path().join("nope"), &ScanOptions::default()),
        Err(ScanError::Root(_))
    ));
}
