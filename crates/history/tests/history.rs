use std::fs;
use std::path::Path;

use history::{compare, find, snapshot, threshold_for, CategoryRow, History, KEEP_PER_FOLDER};

const MB: usize = 1 << 20;

fn write(path: &Path, len: usize) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, vec![1u8; len]).unwrap();
}

fn scan(r: &Path) -> scanner::ScanTree {
    let opts = scanner::ScanOptions {
        method: scanner::ScanMethod::Walk,
        ..Default::default()
    };
    scanner::scan(r, &opts).unwrap()
}

fn cats(size: u64) -> Vec<CategoryRow> {
    vec![CategoryRow {
        key: "videos".into(),
        name: "Videos".into(),
        files: 1,
        size,
    }]
}

#[test]
fn what_grew_points_at_the_most_specific_folders() {
    let dir = tempfile::tempdir().unwrap();
    let r = dir.path();
    write(&r.join("a/one.bin"), 3 * MB);
    write(&r.join("b/two.bin"), 2 * MB);
    write(&r.join("b/inner/three.bin"), 2 * MB);
    write(&r.join("c/small.bin"), MB / 2);
    let before = snapshot(&scan(r), 1000, "walk", cats(5));
    assert_eq!(before.info.threshold, MB as u64);
    // c is under the threshold, so it isn't stored.
    let mut paths: Vec<&str> = before.folders.iter().map(|f| f.path.as_str()).collect();
    paths.sort();
    assert_eq!(paths, ["", "a", "b", r"b\inner"]);

    write(&r.join("a/more.bin"), 3 * MB); // a grew by 3 MB
    fs::remove_dir_all(r.join("b/inner")).unwrap(); // b shrank, all of it in b\inner
    write(&r.join("c/big.bin"), 2 * MB); // c crossed the threshold
    write(&r.join("d/new.bin"), 2 * MB); // a new folder
    let now = scan(r);
    let changes = compare(&before, &now, 10);
    let summary: Vec<(&str, i64, bool, bool)> = changes
        .iter()
        .map(|c| (c.path.as_str(), c.delta(), c.was_missing, c.node.is_some()))
        .collect();
    let mb = MB as i64;
    assert_eq!(
        summary,
        [
            ("a", 3 * mb, false, true),
            // Not stored before, so counted from 0: "new, or under the threshold".
            ("c", 2 * mb + mb / 2, true, true),
            // b is left out: b\inner explains all of its change.
            (r"b\inner", -2 * mb, false, false),
            ("d", 2 * mb, true, true),
        ]
    );
    assert_eq!(compare(&before, &now, 2).len(), 2);
}

#[test]
fn find_walks_sorted_children() {
    let dir = tempfile::tempdir().unwrap();
    let r = dir.path();
    for name in ["m", "a", "z", "b"] {
        write(&r.join(name).join("x/f.txt"), 10);
    }
    write(&r.join("file.txt"), 10);
    let tree = scan(r);
    let id = find(&tree, r"z\x").unwrap();
    assert_eq!(&*tree.node(id).name, "x");
    assert_eq!(&*tree.node(tree.node(id).parent).name, "z");
    assert_eq!(find(&tree, ""), Some(tree.root()));
    assert!(find(&tree, "q").is_none());
    assert!(find(&tree, "file.txt").is_none(), "files are not folders");
    assert_eq!(threshold_for(10), MB as u64);
    assert_eq!(threshold_for(100_000 * MB as u64), 5 * MB as u64);
}

#[test]
fn storage_round_trip_and_retention() {
    let dir = tempfile::tempdir().unwrap();
    write(&dir.path().join("a/f.bin"), 2 * MB);
    let tree = scan(dir.path());
    let db = dir.path().join("h.sqlite");
    let mut h = History::open(&db).unwrap();

    let mut snap = snapshot(&tree, 1000, "mft", cats(7));
    snap.info.root = r"C:\Data".into();
    let id = h.save(&snap).unwrap();
    let back = h.load(id).unwrap().unwrap();
    let mut want = snap.clone();
    want.info.id = id;
    assert_eq!(back.info, want.info);
    let mut a = back.folders.clone();
    let mut b = want.folders.clone();
    a.sort_by(|x, y| x.path.cmp(&y.path));
    b.sort_by(|x, y| x.path.cmp(&y.path));
    assert_eq!(a, b);
    assert_eq!(back.categories, want.categories);
    assert!(h.load(id + 100).unwrap().is_none());

    // The same folder whatever the case and separators; only the newest are kept.
    for t in 1..=(KEEP_PER_FOLDER as i64 + 5) {
        let mut s = snap.clone();
        s.info.taken = 1000 + t;
        s.info.root = if t % 2 == 0 {
            "c:/data/".into()
        } else {
            r"C:\DATA".into()
        };
        h.save(&s).unwrap();
    }
    let list = h.list(r"C:\Data").unwrap();
    assert_eq!(list.len(), KEEP_PER_FOLDER);
    assert_eq!(
        list[0].taken,
        1000 + KEEP_PER_FOLDER as i64 + 5,
        "newest first"
    );
    let mut other = snap.clone();
    other.info.root = r"D:\".into();
    h.save(&other).unwrap();
    assert_eq!(h.list(r"D:\").unwrap().len(), 1);

    let (bytes, scans) = h.usage().unwrap();
    assert!(bytes > 0);
    assert_eq!(scans, KEEP_PER_FOLDER as u64 + 1);
    // Reopening keeps everything; clearing removes it.
    drop(h);
    let mut h = History::open(&db).unwrap();
    assert_eq!(h.list(r"C:\Data").unwrap().len(), KEEP_PER_FOLDER);
    h.clear().unwrap();
    assert_eq!(h.usage().unwrap().1, 0);
    assert!(h.list(r"C:\Data").unwrap().is_empty());
}
