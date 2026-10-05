//! CSV and JSON export of a folder's contents or of search results.

use std::io::{self, Write};

use categories::Rules;
use scanner::{flags, NodeId, NodeKind, ScanTree};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Csv,
    Json,
}

const HEADER: [&str; 9] = [
    "path",
    "name",
    "type",
    "size",
    "size_on_disk",
    "files",
    "modified_utc",
    "category",
    "notes",
];

/// Writes the given `(id, full path)` rows. Returns how many were written.
pub fn write<W: Write>(
    out: &mut W,
    tree: &ScanTree,
    rules: &Rules,
    rows: impl Iterator<Item = (NodeId, String)>,
    format: Format,
) -> io::Result<usize> {
    let mut count = 0;
    match format {
        Format::Csv => writeln!(out, "{}", HEADER.join(","))?,
        Format::Json => write!(out, "[")?,
    }
    for (id, path) in rows {
        let n = tree.node(id);
        let kind = match n.kind {
            NodeKind::File => "file",
            NodeKind::Dir => "folder",
            NodeKind::Symlink => "link",
            NodeKind::Error => "unreadable",
        };
        let category = rules.category(n.category).map_or("", |c| c.name.as_str());
        let notes = notes(n.flags);
        let modified = iso_utc(n.modified);
        match format {
            Format::Csv => {
                let fields = [
                    csv(&path),
                    csv(&n.name),
                    kind.into(),
                    n.size.to_string(),
                    n.alloc.to_string(),
                    n.files.to_string(),
                    modified,
                    csv(category),
                    csv(&notes),
                ];
                writeln!(out, "{}", fields.join(","))?;
            }
            Format::Json => {
                let row = serde_json::json!({
                    "path": path,
                    "name": &*n.name,
                    "type": kind,
                    "size": n.size,
                    "size_on_disk": n.alloc,
                    "files": n.files,
                    "modified_utc": modified,
                    "category": category,
                    "notes": notes,
                });
                if count > 0 {
                    write!(out, ",")?;
                }
                write!(out, "\n  ")?;
                serde_json::to_writer(&mut *out, &row)?;
            }
        }
        count += 1;
    }
    if format == Format::Json {
        writeln!(out, "\n]")?;
    }
    Ok(count)
}

/// Everything below folder `id` (not the folder itself), depth first, with
/// full paths. Deleted entries are left out.
pub fn subtree(tree: &ScanTree, id: NodeId) -> impl Iterator<Item = (NodeId, String)> + '_ {
    let base = tree.path(id).display().to_string();
    let mut stack: Vec<(NodeId, String)> = tree
        .child_ids(id)
        .rev()
        .map(|c| (c, base.clone()))
        .collect();
    std::iter::from_fn(move || {
        while let Some((c, parent)) = stack.pop() {
            let n = tree.node(c);
            if n.has_flag(flags::REMOVED) {
                continue;
            }
            let path = if parent.ends_with(['\\', '/']) {
                format!("{parent}{}", n.name)
            } else {
                format!("{parent}{}{}", std::path::MAIN_SEPARATOR, n.name)
            };
            stack.extend(tree.child_ids(c).rev().map(|k| (k, path.clone())));
            return Some((c, path));
        }
        None
    })
}

fn notes(f: u8) -> String {
    let mut v = Vec::new();
    if f & flags::HARDLINK_DUP != 0 {
        v.push("hardlink counted elsewhere");
    }
    if f & flags::CLOUD_PLACEHOLDER != 0 {
        v.push("online-only");
    }
    if f & flags::COMPRESSED != 0 {
        v.push("compressed");
    }
    if f & flags::SPARSE != 0 {
        v.push("sparse");
    }
    if f & flags::READ_ERROR != 0 {
        v.push("partly unreadable");
    }
    v.join("; ")
}

/// Quotes a CSV field when needed.
fn csv(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// `YYYY-MM-DD HH:MM:SS` in UTC, or empty for 0 (unknown).
fn iso_utc(secs: i64) -> String {
    if secs == 0 {
        return String::new();
    }
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn dates_and_quoting() {
        assert_eq!(iso_utc(0), "");
        assert_eq!(iso_utc(1), "1970-01-01 00:00:01");
        assert_eq!(iso_utc(951_782_400), "2000-02-29 00:00:00");
        assert_eq!(iso_utc(1_791_244_800), "2026-10-06 00:00:00");
        assert_eq!(iso_utc(-86_400), "1969-12-31 00:00:00");
        assert_eq!(csv("plain"), "plain");
        assert_eq!(csv("a,b"), "\"a,b\"");
        assert_eq!(csv("say \"hi\""), "\"say \"\"hi\"\"\"");
    }

    #[test]
    fn exports_a_folder_depth_first() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        fs::create_dir_all(r.join("a/b")).unwrap();
        fs::write(r.join("a/b/x, y.txt"), b"12345").unwrap();
        fs::write(r.join("a/z.txt"), b"1").unwrap();
        fs::write(r.join("top.txt"), b"12").unwrap();
        let tree = scanner::scan(r, &scanner::ScanOptions::default()).unwrap();
        let rules = Rules::load(&[("none", "")]).unwrap();
        let root = r.display().to_string();
        let sep = std::path::MAIN_SEPARATOR;

        let mut out = Vec::new();
        let n = write(
            &mut out,
            &tree,
            &rules,
            subtree(&tree, tree.root()),
            Format::Csv,
        )
        .unwrap();
        let text = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(n, 5);
        assert_eq!(lines[0], HEADER.join(","));
        let paths: Vec<&str> = lines[1..]
            .iter()
            .map(|l| l.split(',').next().unwrap())
            .collect();
        assert_eq!(paths[0], format!("{root}{sep}a"));
        assert_eq!(paths[1], format!("{root}{sep}a{sep}b"));
        assert!(lines[3].starts_with(&format!(
            "\"{root}{sep}a{sep}b{sep}x, y.txt\",\"x, y.txt\",file,5,"
        )));
        assert!(lines[5].starts_with(&format!("{root}{sep}top.txt,top.txt,file,2,")));

        let mut out = Vec::new();
        let a = (0..tree.len() as NodeId)
            .find(|&i| &*tree.node(i).name == "a")
            .unwrap();
        write(&mut out, &tree, &rules, subtree(&tree, a), Format::Json).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
        let names: Vec<&str> = v
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["b", "x, y.txt", "z.txt"]);
        assert_eq!(v[0]["type"], "folder");
        assert_eq!(v[0]["size"], 5);
    }
}
