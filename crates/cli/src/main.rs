//! diskmap-cli: scan a folder and print where the space went.

use std::fs::File;
use std::io::{self, BufWriter, IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use clap::Parser;
use scanner::{flags, scan, NodeId, NodeKind, Progress, ScanOptions, ScanTree};

#[derive(Parser)]
#[command(
    name = "diskmap-cli",
    version,
    about = "Scan a folder and show what takes up space"
)]
struct Args {
    /// Folder or drive to scan, for example C:\ or C:\Users\raul
    path: PathBuf,

    /// How many folders and files to list
    #[arg(long, default_value_t = 15)]
    top: usize,

    /// Write the tree as JSON to this file
    #[arg(long, value_name = "FILE")]
    json: Option<PathBuf>,

    /// Folder levels to include in the JSON export
    #[arg(long, default_value_t = 4)]
    depth: usize,

    /// Leave entries smaller than this many bytes out of the JSON export
    #[arg(long, default_value_t = 1_048_576)]
    min_size: u64,

    /// Do not detect hardlinks (hardlinked files are counted more than once)
    #[arg(long)]
    no_hardlinks: bool,

    /// Windows: use the slower portable folder listing (for comparison)
    #[arg(long)]
    slow_listing: bool,

    /// Worker threads (default: one per CPU)
    #[arg(long)]
    threads: Option<usize>,

    /// List every entry that could not be read
    #[arg(long)]
    show_errors: bool,
}

fn main() -> ExitCode {
    let args = Args::parse();
    let progress = Arc::new(Progress::default());
    let opts = ScanOptions {
        threads: args.threads,
        detect_hardlinks: !args.no_hardlinks,
        fast_listing: !args.slow_listing,
        progress: Some(progress.clone()),
        cancel: None,
    };

    let done = Arc::new(AtomicBool::new(false));
    let ticker = io::stderr().is_terminal().then(|| {
        let (progress, done) = (progress.clone(), done.clone());
        thread::spawn(move || {
            while !done.load(Ordering::Relaxed) {
                let (files, dirs, bytes) = progress.snapshot();
                eprint!(
                    "\rScanning... {files} files, {dirs} folders, {}   ",
                    human(bytes)
                );
                thread::sleep(Duration::from_millis(200));
            }
            eprint!("\r{:70}\r", "");
        })
    });

    let result = scan(&args.path, &opts);
    done.store(true, Ordering::Relaxed);
    if let Some(t) = ticker {
        let _ = t.join();
    }

    let tree = match result {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    if let Err(e) = report(&tree, &args) {
        eprintln!("error: {e}");
        return ExitCode::FAILURE;
    }
    if let Some(path) = &args.json {
        match write_json(&tree, path, args.depth, args.min_size) {
            Ok(()) => println!("\nJSON written to {}", path.display()),
            Err(e) => {
                eprintln!("error writing JSON: {e}");
                return ExitCode::FAILURE;
            }
        }
    }
    ExitCode::SUCCESS
}

fn report(tree: &ScanTree, args: &Args) -> io::Result<()> {
    let out = io::stdout();
    let mut o = out.lock();
    let root = tree.node(tree.root());
    let s = tree.stats();

    writeln!(o, "{}", root.name)?;
    writeln!(o, "  Size          {}", human(root.size))?;
    writeln!(o, "  Size on disk  {}", human(root.alloc))?;
    writeln!(
        o,
        "  Contents      {} files, {} folders, {} links",
        s.files, s.dirs, s.symlinks
    )?;
    if s.hardlink_dups > 0 {
        writeln!(
            o,
            "  Hardlinks     {} extra links counted once",
            s.hardlink_dups
        )?;
    }
    if !tree.errors().is_empty() || s.errors > 0 {
        writeln!(
            o,
            "  Unreadable    {} entries (run with --show-errors)",
            tree.errors().len().max(s.errors as usize)
        )?;
    }
    writeln!(o, "  Scan time     {:.2?}", s.elapsed)?;

    writeln!(o, "\nLargest items in {}", root.name)?;
    for id in tree
        .children_by_size(tree.root())
        .into_iter()
        .take(args.top)
    {
        row(&mut o, tree, id, root.size, false)?;
    }

    writeln!(o, "\nLargest files")?;
    for id in tree.largest_files(args.top) {
        row(&mut o, tree, id, root.size, true)?;
    }

    if args.show_errors && !tree.errors().is_empty() {
        writeln!(o, "\nCould not read")?;
        for (id, err) in tree.errors() {
            writeln!(o, "  {}  ({err})", tree.path(*id).display())?;
        }
    }
    Ok(())
}

fn row(
    o: &mut impl Write,
    tree: &ScanTree,
    id: NodeId,
    total: u64,
    full_path: bool,
) -> io::Result<()> {
    let n = tree.node(id);
    let pct = if total > 0 {
        n.size as f64 * 100.0 / total as f64
    } else {
        0.0
    };
    let label = if full_path {
        tree.path(id).display().to_string()
    } else if n.is_dir() {
        format!("{}{}", n.name, std::path::MAIN_SEPARATOR)
    } else {
        n.name.to_string()
    };
    let mut notes = Vec::new();
    if n.has_flag(flags::CLOUD_PLACEHOLDER) {
        notes.push("online-only");
    }
    if n.has_flag(flags::COMPRESSED) {
        notes.push("compressed");
    }
    if n.has_flag(flags::SPARSE) {
        notes.push("sparse");
    }
    if n.has_flag(flags::READ_ERROR) {
        notes.push("partly unreadable");
    }
    if n.kind == NodeKind::Symlink {
        notes.push("link, not followed");
    }
    let notes = if notes.is_empty() {
        String::new()
    } else {
        format!("  [{}]", notes.join(", "))
    };
    writeln!(
        o,
        "  {:>10}  {:>5.1}%  {}{}",
        human(n.size),
        pct,
        label,
        notes
    )
}

fn human(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

fn write_json(tree: &ScanTree, path: &PathBuf, depth: usize, min_size: u64) -> io::Result<()> {
    let mut w = BufWriter::new(File::create(path)?);
    let s = tree.stats();
    write!(
        w,
        "{{\"stats\":{{\"files\":{},\"dirs\":{},\"symlinks\":{},\"errors\":{},\"hardlink_dups\":{},\"elapsed_ms\":{}}},\"tree\":",
        s.files, s.dirs, s.symlinks, s.errors, s.hardlink_dups, s.elapsed.as_millis()
    )?;
    node_json(&mut w, tree, tree.root(), depth, min_size)?;
    writeln!(w, "}}")?;
    w.flush()
}

fn node_json(
    w: &mut impl Write,
    tree: &ScanTree,
    id: NodeId,
    depth: usize,
    min_size: u64,
) -> io::Result<()> {
    let n = tree.node(id);
    let kind = match n.kind {
        NodeKind::File => "file",
        NodeKind::Dir => "dir",
        NodeKind::Symlink => "symlink",
        NodeKind::Error => "error",
    };
    write!(w, "{{\"name\":")?;
    json_str(w, &n.name)?;
    write!(
        w,
        ",\"kind\":\"{kind}\",\"size\":{},\"alloc\":{},\"files\":{},\"modified\":{},\"flags\":{}",
        n.size, n.alloc, n.files, n.modified, n.flags
    )?;
    if n.is_dir() && n.child_count > 0 {
        if depth == 0 {
            write!(w, ",\"truncated\":true")?;
        } else {
            write!(w, ",\"children\":[")?;
            let mut first = true;
            let mut hidden = (0u64, 0u64);
            for c in tree.children_by_size(id) {
                let cn = tree.node(c);
                if cn.size < min_size {
                    hidden.0 += 1;
                    hidden.1 += cn.size;
                    continue;
                }
                if !first {
                    write!(w, ",")?;
                }
                first = false;
                node_json(w, tree, c, depth - 1, min_size)?;
            }
            write!(w, "]")?;
            if hidden.0 > 0 {
                write!(
                    w,
                    ",\"hidden_small\":{{\"count\":{},\"size\":{}}}",
                    hidden.0, hidden.1
                )?;
            }
        }
    }
    write!(w, "}}")
}

fn json_str(w: &mut impl Write, s: &str) -> io::Result<()> {
    write!(w, "\"")?;
    for ch in s.chars() {
        match ch {
            '"' => write!(w, "\\\"")?,
            '\\' => write!(w, "\\\\")?,
            '\n' => write!(w, "\\n")?,
            '\r' => write!(w, "\\r")?,
            '\t' => write!(w, "\\t")?,
            c if (c as u32) < 0x20 => write!(w, "\\u{:04x}", c as u32)?,
            c => write!(w, "{c}")?,
        }
    }
    write!(w, "\"")
}
