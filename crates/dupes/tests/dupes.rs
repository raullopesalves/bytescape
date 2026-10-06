use std::fs;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use dupes::{candidates, find, Options, EDGE};

/// Deterministic pseudo-random bytes.
fn bytes(seed: u64, len: usize) -> Vec<u8> {
    let mut x = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    (0..len)
        .map(|_| {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (x >> 33) as u8
        })
        .collect()
}

fn put(root: &Path, rel: &str, data: &[u8]) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, data).unwrap();
}

fn scan(root: &Path) -> scanner::ScanTree {
    let opts = scanner::ScanOptions {
        method: scanner::ScanMethod::Walk,
        ..Default::default()
    };
    scanner::scan(root, &opts).unwrap()
}

fn names(tree: &scanner::ScanTree, ids: &[u32]) -> Vec<String> {
    let mut v: Vec<String> = ids.iter().map(|&i| tree.node(i).name.to_string()).collect();
    v.sort();
    v
}

const SIZE: usize = 100_000;

#[test]
fn stages_find_true_copies_and_read_little() {
    let dir = tempfile::tempdir().unwrap();
    let r = dir.path();
    let same = bytes(1, SIZE);
    put(r, "a/x.bin", &same);
    put(r, "b/x-copy.bin", &same);
    put(r, "c/third.bin", &same);
    // Same ends, different middle: only a full read tells them apart.
    let mut m2 = bytes(2, SIZE);
    put(r, "d/m1.bin", &m2);
    m2[SIZE / 2] ^= 0xFF;
    put(r, "d/m2.bin", &m2);
    // Different first byte: told apart by the ends.
    let mut h = bytes(3, SIZE);
    put(r, "e/h1.bin", &h);
    h[0] ^= 0xFF;
    put(r, "e/h2.bin", &h);
    // A hardlink is the same file, not a copy.
    put(r, "g/orig.bin", &bytes(4, SIZE));
    fs::hard_link(r.join("g/orig.bin"), r.join("g/link.bin")).unwrap();
    // Two copies, one of which changes after the scan.
    let c = bytes(5, SIZE);
    put(r, "h/c1.bin", &c);
    put(r, "h/c2.bin", &c);
    // Too small to count.
    put(r, "f/small1.bin", &bytes(6, 10_000));
    put(r, "f/small2.bin", &bytes(6, 10_000));

    let tree = scan(r);
    // Same length, new contents, later time: changed since the scan.
    fs::write(r.join("h/c2.bin"), bytes(9, SIZE)).unwrap();
    let f = fs::File::options()
        .write(true)
        .open(r.join("h/c2.bin"))
        .unwrap();
    f.set_modified(std::time::SystemTime::now() + Duration::from_secs(100))
        .unwrap();
    drop(f);

    let report = find(candidates(&tree, 50_000), &Options::default())
        .ok()
        .unwrap();
    assert_eq!(report.groups.len(), 1);
    let g = &report.groups[0];
    assert_eq!(names(&tree, &g.files), ["third.bin", "x-copy.bin", "x.bin"]);
    assert_eq!((g.size, g.wasted()), (SIZE as u64, 2 * SIZE as u64));
    // Ten files of that size (one link of g/orig.bin), one changed.
    assert_eq!(report.candidates, 10);
    assert_eq!(report.changed, 1);
    assert_eq!(report.unreadable, 0);
    // Stage 2: the ends of the 9 unchanged files. Stage 3: whole files for
    // the two groups whose ends matched (the three copies, d/m1 and d/m2).
    assert_eq!(report.bytes_read, 9 * 2 * EDGE + 5 * SIZE as u64);
}

#[test]
fn small_files_are_read_whole_once() {
    let dir = tempfile::tempdir().unwrap();
    let r = dir.path();
    let n = 20_000; // under two ends' worth
    put(r, "one.dat", &bytes(7, n));
    put(r, "two.dat", &bytes(7, n));
    put(r, "other.dat", &bytes(8, n));
    let tree = scan(r);
    let report = find(candidates(&tree, 1), &Options::default())
        .ok()
        .unwrap();
    assert_eq!(report.groups.len(), 1);
    assert_eq!(
        names(&tree, &report.groups[0].files),
        ["one.dat", "two.dat"]
    );
    assert_eq!(report.bytes_read, 3 * n as u64);
}

#[test]
fn cancelling_stops() {
    let dir = tempfile::tempdir().unwrap();
    put(dir.path(), "a", &bytes(1, 1000));
    put(dir.path(), "b", &bytes(1, 1000));
    let tree = scan(dir.path());
    let opts = Options {
        cancel: Some(Arc::new(AtomicBool::new(true))),
        ..Default::default()
    };
    assert_eq!(
        find(candidates(&tree, 1), &opts),
        Err(dupes::DupesError::Cancelled)
    );
}
