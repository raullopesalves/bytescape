//! Filters over a scan tree: name, extension, size, age and category.
//!
//! A search checks every node in parallel and keeps the matches, largest
//! first, plus a mark per node saying whether it matches or has a match
//! below it (used to dim the treemap).

use rayon::prelude::*;
use scanner::{flags, Node, NodeId, NodeKind, ScanTree};
use serde::Deserialize;

/// What to look for. Empty fields match everything.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Filter {
    /// Part of the name, ignoring case; with `*` or `?`, a pattern for the
    /// whole name instead.
    pub name: String,
    /// Extensions without the dot (`"mp4"`, `"tar.gz"`), ignoring case.
    pub extensions: Vec<String>,
    pub min_size: Option<u64>,
    pub max_size: Option<u64>,
    /// Last modified at or after this time (seconds since 1970).
    pub modified_after: Option<i64>,
    /// Last modified before this time (seconds since 1970).
    pub modified_before: Option<i64>,
    pub category: Option<u16>,
    /// Match folders too (by name, total size, age and path category).
    /// Never with an extension filter.
    pub folders: bool,
}

/// [`Mark`] bits.
pub const SELF: u8 = 1;
pub const BELOW: u8 = 2;
/// Per node: `SELF` if it matches, `BELOW` if something under it does.
pub type Mark = u8;

pub struct Results {
    /// Matching nodes, largest first.
    pub ids: Vec<NodeId>,
    /// Bytes in matching files (each hardlinked file once).
    pub file_bytes: u64,
    pub marks: Vec<Mark>,
}

impl Filter {
    pub fn is_empty(&self) -> bool {
        self.name.trim().is_empty()
            && self.extensions.iter().all(|e| e.trim().is_empty())
            && self.min_size.is_none()
            && self.max_size.is_none()
            && self.modified_after.is_none()
            && self.modified_before.is_none()
            && self.category.is_none()
    }
}

/// Runs `filter` over every node except the root.
pub fn run(tree: &ScanTree, filter: &Filter) -> Results {
    let m = Matcher::new(filter);
    let mut hits: Vec<(u64, NodeId)> = tree
        .nodes()
        .par_iter()
        .enumerate()
        .skip(1)
        .filter(|(_, n)| m.matches(n))
        .map(|(i, n)| (n.size, i as NodeId))
        .collect();
    hits.par_sort_unstable_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));

    let mut marks = vec![0u8; tree.len()];
    let mut file_bytes = 0;
    for &(_, id) in &hits {
        let n = tree.node(id);
        if n.kind == NodeKind::File && !n.has_flag(flags::HARDLINK_DUP) {
            file_bytes += n.size;
        }
        marks[id as usize] |= SELF;
        // Mark ancestors until one is already marked: each is visited once.
        let mut cur = id;
        while cur != tree.root() {
            cur = tree.node(cur).parent;
            if marks[cur as usize] & BELOW != 0 {
                break;
            }
            marks[cur as usize] |= BELOW;
        }
    }
    Results {
        ids: hits.into_iter().map(|(_, id)| id).collect(),
        file_bytes,
        marks,
    }
}

enum NamePattern {
    Any,
    /// Lower-case substring.
    Contains(String),
    /// Lower-case wildcard pattern for the whole name.
    Wildcard(String),
}

struct Matcher<'a> {
    f: &'a Filter,
    name: NamePattern,
    /// Lower-case, each with a leading dot.
    extensions: Vec<String>,
}

impl<'a> Matcher<'a> {
    fn new(f: &'a Filter) -> Self {
        let n = f.name.trim().to_lowercase();
        let name = if n.is_empty() {
            NamePattern::Any
        } else if n.contains(['*', '?']) {
            NamePattern::Wildcard(n)
        } else {
            NamePattern::Contains(n)
        };
        let extensions = f
            .extensions
            .iter()
            .map(|e| e.trim().trim_start_matches('.').to_lowercase())
            .filter(|e| !e.is_empty())
            .map(|e| format!(".{e}"))
            .collect();
        Matcher {
            f,
            name,
            extensions,
        }
    }

    fn matches(&self, n: &Node) -> bool {
        if n.has_flag(flags::REMOVED) {
            return false;
        }
        match n.kind {
            NodeKind::File => {}
            NodeKind::Dir if self.f.folders && self.extensions.is_empty() => {}
            _ => return false,
        }
        let f = self.f;
        if f.min_size.is_some_and(|s| n.size < s) || f.max_size.is_some_and(|s| n.size > s) {
            return false;
        }
        if f.modified_after.is_some_and(|t| n.modified < t)
            || f.modified_before.is_some_and(|t| n.modified >= t)
        {
            return false;
        }
        if f.category.is_some_and(|c| n.category != c) {
            return false;
        }
        if !self.extensions.is_empty()
            && !self
                .extensions
                .iter()
                .any(|e| ends_with_ignore_case(&n.name, e))
        {
            return false;
        }
        match &self.name {
            NamePattern::Any => true,
            NamePattern::Contains(p) => contains_ignore_case(&n.name, p),
            NamePattern::Wildcard(p) => wildcard_ignore_case(&n.name, p),
        }
    }
}

/// `pattern` is already lower case.
fn contains_ignore_case(name: &str, pattern: &str) -> bool {
    if name.is_ascii() {
        // No allocation for the common case.
        let (h, p) = (name.as_bytes(), pattern.as_bytes());
        p.len() <= h.len() && h.windows(p.len()).any(|w| w.eq_ignore_ascii_case(p))
    } else {
        name.to_lowercase().contains(pattern)
    }
}

/// `suffix` is already lower case.
fn ends_with_ignore_case(name: &str, suffix: &str) -> bool {
    if name.is_ascii() {
        let (h, s) = (name.as_bytes(), suffix.as_bytes());
        s.len() <= h.len() && h[h.len() - s.len()..].eq_ignore_ascii_case(s)
    } else {
        name.to_lowercase().ends_with(suffix)
    }
}

/// `*` matches any run of characters and `?` any one; `pattern` is lower case.
fn wildcard_ignore_case(name: &str, pattern: &str) -> bool {
    if name.is_ascii() && pattern.is_ascii() {
        wildcard(name.as_bytes(), pattern.as_bytes(), |a, b| {
            a.eq_ignore_ascii_case(b)
        })
    } else {
        let n: Vec<char> = name.to_lowercase().chars().collect();
        let p: Vec<char> = pattern.chars().collect();
        wildcard(&n, &p, |a, b| a == b)
    }
}

/// Iterative wildcard match with backtracking to the last `*`.
fn wildcard<T: Copy + PartialEq + From<u8>>(s: &[T], p: &[T], eq: impl Fn(&T, &T) -> bool) -> bool {
    let (star, any) = (T::from(b'*'), T::from(b'?'));
    let (mut i, mut j) = (0, 0);
    let mut back: Option<(usize, usize)> = None;
    while i < s.len() {
        if j < p.len() && (p[j] == any || (p[j] != star && eq(&p[j], &s[i]))) {
            i += 1;
            j += 1;
        } else if j < p.len() && p[j] == star {
            back = Some((j, i));
            j += 1;
        } else if let Some((bj, bi)) = back {
            j = bj + 1;
            i = bi + 1;
            back = Some((bj, bi + 1));
        } else {
            return false;
        }
    }
    p[j..].iter().all(|&c| c == star)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn wildcards() {
        let cases = [
            ("report.pdf", "*.pdf", true),
            ("REPORT.PDF", "*.pdf", true),
            ("report.pdf", "rep*", true),
            ("report.pdf", "*port*", true),
            ("report.pdf", "r?port.pdf", true),
            ("report.pdf", "*.doc", false),
            ("report.pdf", "report", false),
            ("a", "*", true),
            ("", "*", true),
            ("abc", "a*b*c", true),
            ("abxc", "a*b?c", true),
            ("abc", "a*b?c", false),
            ("Résumé.docx", "résumé*", true),
            ("Résumé.docx", "*.DOCX", false), // pattern must be lower case
        ];
        for (name, pat, want) in cases {
            assert_eq!(wildcard_ignore_case(name, pat), want, "{name} vs {pat}");
        }
        assert!(contains_ignore_case("MyReport.PDF", "report"));
        assert!(contains_ignore_case("Ünïcode Name", "ïcode"));
        assert!(!contains_ignore_case("ab", "abc"));
        assert!(ends_with_ignore_case("Backup.TAR.GZ", ".tar.gz"));
        assert!(!ends_with_ignore_case("gz", ".gz"));
    }

    fn sample() -> (tempfile::TempDir, ScanTree) {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        fs::create_dir_all(r.join("videos/old")).unwrap();
        fs::create_dir_all(r.join("docs")).unwrap();
        fs::write(r.join("videos/clip.mp4"), vec![0u8; 5000]).unwrap();
        fs::write(r.join("videos/CLIP2.MKV"), vec![0u8; 3000]).unwrap();
        fs::write(r.join("videos/old/home.mp4"), vec![0u8; 8000]).unwrap();
        fs::write(r.join("docs/report.pdf"), vec![0u8; 700]).unwrap();
        fs::write(r.join("docs/report-old.pdf"), vec![0u8; 100]).unwrap();
        fs::write(r.join("site.tar.gz"), vec![0u8; 2000]).unwrap();
        let tree = scanner::scan(r, &scanner::ScanOptions::default()).unwrap();
        (dir, tree)
    }

    fn names(tree: &ScanTree, r: &Results) -> Vec<String> {
        r.ids
            .iter()
            .map(|&id| tree.node(id).name.to_string())
            .collect()
    }

    #[test]
    fn filters_combine_and_sort_by_size() {
        let (_d, tree) = sample();
        let run_names = |f: Filter| names(&tree, &run(&tree, &f));
        let f = |name: &str| Filter {
            name: name.into(),
            ..Filter::default()
        };

        assert_eq!(run_names(f("report")), ["report.pdf", "report-old.pdf"]);
        assert_eq!(run_names(f("*.MP4")), ["home.mp4", "clip.mp4"]);
        assert_eq!(run_names(f("clip")), ["clip.mp4", "CLIP2.MKV"]);
        // Folders only when asked for.
        assert_eq!(run_names(f("old")), ["report-old.pdf"]);
        assert_eq!(
            run_names(Filter {
                folders: true,
                ..f("old")
            }),
            ["old", "report-old.pdf"]
        );
        let exts = |e: &[&str]| Filter {
            extensions: e.iter().map(|s| s.to_string()).collect(),
            folders: true,
            ..Filter::default()
        };
        assert_eq!(
            run_names(exts(&["mp4", ".MKV"])),
            ["home.mp4", "clip.mp4", "CLIP2.MKV"]
        );
        assert_eq!(run_names(exts(&["tar.gz"])), ["site.tar.gz"]);
        assert_eq!(
            run_names(Filter {
                min_size: Some(3000),
                max_size: Some(5000),
                ..Filter::default()
            }),
            ["clip.mp4", "CLIP2.MKV"]
        );
        assert_eq!(
            run_names(Filter {
                min_size: Some(3000),
                ..exts(&["mp4"])
            }),
            ["home.mp4", "clip.mp4"]
        );
        // Everything was just written: nothing is older than an hour ago.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        assert!(run_names(Filter {
            modified_before: Some(now - 3600),
            ..Filter::default()
        })
        .is_empty());
        assert_eq!(
            run_names(Filter {
                modified_after: Some(now - 3600),
                ..Filter::default()
            })
            .len(),
            6
        );
    }

    #[test]
    fn category_filter_and_marks() {
        let (_d, mut tree) = sample();
        let find = |tree: &ScanTree, name: &str| {
            (0..tree.len() as NodeId)
                .find(|&i| tree.node(i).name.as_ref() == name)
                .unwrap()
        };
        let (home, clip) = (find(&tree, "home.mp4"), find(&tree, "clip.mp4"));
        tree.set_category(home, 3);
        tree.set_category(clip, 3);
        let r = run(
            &tree,
            &Filter {
                category: Some(3),
                ..Filter::default()
            },
        );
        assert_eq!(names(&tree, &r), ["home.mp4", "clip.mp4"]);
        assert_eq!(r.file_bytes, 13000);

        let mark = |name: &str| r.marks[find(&tree, name) as usize];
        assert_eq!(mark("home.mp4"), SELF);
        assert_eq!(mark("old"), BELOW);
        assert_eq!(mark("videos"), BELOW);
        assert_eq!(r.marks[0], BELOW);
        assert_eq!(mark("docs"), 0);
        assert_eq!(mark("CLIP2.MKV"), 0);
    }

    #[test]
    fn empty_filter() {
        assert!(Filter::default().is_empty());
        assert!(Filter {
            name: "  ".into(),
            extensions: vec![" ".into()],
            ..Filter::default()
        }
        .is_empty());
        assert!(!Filter {
            category: Some(1),
            ..Filter::default()
        }
        .is_empty());
        // `folders` alone is not a filter.
        assert!(Filter {
            folders: true,
            ..Filter::default()
        }
        .is_empty());
    }
}
